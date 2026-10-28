"""Edit bench: measures the reliability of Po's `edit` tool with a real model.

Each task starts from a fresh workspace, booby-trapped the way real repositories are: CRLF line
endings, tab indentation, trailing spaces, repeated lines. Po receives the instruction as a
single prompt (`-p --yes`), then the result is checked by a function, not by eye. The saved
session gives the number of `edit` calls and their failures.

    python scripts/bench_edit.py [path/to/po] [--mode offline] [--repeat N] [--only name]

The model used is the one Po picks in the given mode. The bench sends nothing but what Po sends.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

CART = """\
def compute_total(items):
    total = 0
    for item in items:
        total += item["price"] * item["quantity"]
    return total


def discount(total, rate):
    return total - total * rate


def summary(items):
    total = compute_total(items)
    return f"{len(items)} items, {total:.2f} euros"
"""

LOOP = """\
def rolling_average(values, window):
    results = []
    for i in range(len(values) - window):
        chunk = values[i:i + window]
        results.append(sum(chunk) / window)
    return results
"""

# Tab indentation and trailing spaces: the classic trap of an `old` copied from memory.
TABS = "fn main() {\n\tlet port = 8080;   \n\tlet host = \"localhost\";\n\tprintln!(\"{host}:{port}\");\n}\n"

# Windows line endings.
CRLF = "[server]\r\nport = 8080\r\nhost = \"localhost\"\r\n\r\n[log]\r\nlevel = \"info\"\r\n"

# Two identical lines: the tool must refuse the ambiguity, the model must be more specific.
DUPLICATE = """\
def first():
    value = 1
    return value


def second():
    value = 1
    return value * 2
"""


def read(ws: Path, name: str) -> str:
    return (ws / name).read_bytes().decode("utf-8")


TASKS = [
    {
        "name": "rename",
        "files": {"cart.py": CART},
        "prompt": "In cart.py, rename the function compute_total to order_total, "
                  "including where it is called. Change nothing else.",
        "check": lambda ws: "compute_total" not in read(ws, "cart.py")
        and read(ws, "cart.py").count("order_total") == 2,
    },
    {
        "name": "parameter",
        "files": {"cart.py": CART},
        "prompt": "In cart.py, give the rate parameter of the discount function the default "
                  "value 0.1.",
        "check": lambda ws: "def discount(total, rate=0.1):" in read(ws, "cart.py"),
    },
    {
        "name": "off_by_one",
        "files": {"loop.py": LOOP},
        "prompt": "The function in loop.py misses the last window. Fix the loop bound, "
                  "changing nothing else.",
        "check": lambda ws: "range(len(values) - window + 1)" in read(ws, "loop.py"),
    },
    {
        "name": "tabs",
        "files": {"main.rs": TABS},
        "prompt": "In main.rs, replace port 8080 with 9090.",
        "check": lambda ws: "9090" in read(ws, "main.rs") and "8080" not in read(ws, "main.rs")
        and "\tlet host" in read(ws, "main.rs"),
    },
    {
        "name": "crlf",
        "files": {"config.toml": CRLF},
        "prompt": "In config.toml, change the log level from info to debug.",
        "check": lambda ws: 'level = "debug"' in read(ws, "config.toml")
        and read(ws, "config.toml").count("\r\n") == 6,
    },
    {
        "name": "duplicate",
        "files": {"duplicate.py": DUPLICATE},
        "prompt": "In duplicate.py, only in the function second, replace value = 1 with "
                  "value = 5. The function first must not change.",
        "check": lambda ws: read(ws, "duplicate.py").count("value = 1") == 1
        and "value = 5" in read(ws, "duplicate.py")
        and read(ws, "duplicate.py").index("value = 1") < read(ws, "duplicate.py").index("value = 5"),
    },
    {
        "name": "insertion",
        "files": {"cart.py": CART},
        "prompt": "In cart.py, add a one-line docstring to the summary function: "
                  "\"Describes the cart in one sentence.\"",
        "check": lambda ws: '"""Describes the cart in one sentence."""' in read(ws, "cart.py")
        and read(ws, "cart.py").index("def summary") < read(ws, "cart.py").index("Describes the cart"),
    },
    {
        "name": "deletion",
        "files": {"cart.py": CART},
        "prompt": "In cart.py, delete the discount function entirely.",
        "check": lambda ws: "def discount" not in read(ws, "cart.py")
        and "def summary" in read(ws, "cart.py") and "def compute_total" in read(ws, "cart.py"),
    },
]


def sessions_dir() -> Path:
    if sys.platform == "win32":
        return Path(os.environ["APPDATA"]) / "po" / "sessions"
    base = os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")
    return Path(base) / "po" / "sessions"


def count_tools(session_id: str) -> dict:
    """Tool calls and failures, read from the saved session."""
    path = sessions_dir() / f"{session_id}.json"
    if not path.exists():
        return {}
    data = json.loads(path.read_text(encoding="utf-8"))
    calls, errors = {}, {}
    names = {}
    for message in data.get("messages", []):
        for block in message.get("content", []):
            if block.get("type") == "tool_use":
                names[block["id"]] = block["name"]
                calls[block["name"]] = calls.get(block["name"], 0) + 1
            elif block.get("type") == "tool_result" and block.get("is_error"):
                name = names.get(block.get("tool_use_id"), "?")
                errors[name] = errors.get(name, 0) + 1
    return {"calls": calls, "errors": errors}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("po", nargs="?", default=str(Path(__file__).resolve().parents[1] / "target" / "debug" / ("po.exe" if sys.platform == "win32" else "po")))
    parser.add_argument("--mode", default="offline")
    parser.add_argument("--repeat", type=int, default=1)
    parser.add_argument("--only")
    parser.add_argument("--timeout", type=int, default=300)
    args = parser.parse_args()

    # The session id is read from Po's English output ("session saved: <id>").
    env = {**os.environ, "PO_LANG": "en"}
    results = []
    for repetition in range(args.repeat):
        for task in TASKS:
            if args.only and task["name"] != args.only:
                continue
            ws = Path(tempfile.mkdtemp(prefix=f"po-bench-{task['name']}-"))
            for name, content in task["files"].items():
                (ws / name).write_bytes(content.encode("utf-8"))
            started = time.time()
            try:
                output = subprocess.run(
                    [args.po, "--mode", args.mode, "--yes", "-p", task["prompt"]],
                    cwd=ws, capture_output=True, text=True, encoding="utf-8", errors="replace",
                    timeout=args.timeout, env=env,
                ).stdout
            except subprocess.TimeoutExpired:
                output = "TIMEOUT"
            duration = time.time() - started
            session = ""
            for line in output.splitlines():
                if "session saved" in line:
                    session = line.split(":")[-1].strip()
            try:
                ok = bool(task["check"](ws))
            except Exception:
                ok = False
            tools = count_tools(session) if session else {}
            r = {"task": task["name"], "ok": ok, "duration_s": round(duration, 1), **tools}
            results.append(r)
            print(json.dumps(r, ensure_ascii=False), flush=True)
            if not ok:
                (Path(tempfile.gettempdir()) / f"po-bench-failure-{task['name']}-{repetition}.txt").write_text(output, encoding="utf-8")
            shutil.rmtree(ws, ignore_errors=True)

    passed = sum(r["ok"] for r in results)
    edits = sum(r.get("calls", {}).get("edit", 0) for r in results)
    edit_failures = sum(r.get("errors", {}).get("edit", 0) for r in results)
    print(f"\nSUMMARY: {passed}/{len(results)} tasks passed · {edits} edit calls · "
          f"{edit_failures} edit failures · {sum(r['duration_s'] for r in results):.0f} s total")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
