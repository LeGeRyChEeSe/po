"""Minimal user-space SSH server, to validate Po through a real SSH transport.

The goal is not a production-grade SSH server, but to reproduce what the program sees when an
SSH client — Termius, OpenSSH, PuTTY — connects to it: real SSH protocol, pseudo-terminal
allocated by the server, `SSH_*` variables set, and the latency of a network link (even a
local one).

Listens on 127.0.0.1 only, with a disposable host key and client key.
"""

import asyncio
import os
import pty
import signal
import sys

import asyncssh

PORT = int(os.environ.get("PO_SSH_PORT", "2222"))


class Server(asyncssh.SSHServer):
    def begin_auth(self, username):
        # Authorization uses the client key passed as a parameter at startup.
        return True

    def public_key_auth_supported(self):
        return True


async def handle_session(process):
    """Runs a shell in a real PTY and relays bytes in both directions."""
    term = process.get_terminal_type() or "xterm-256color"
    size = process.get_terminal_size()
    cols, rows = (size[0], size[1]) if size else (80, 24)

    pid, fd = pty.fork()
    if pid == 0:
        # Child side: set the environment a real SSH session provides, which is precisely what
        # Po consults to detect a remote transport.
        os.environ["TERM"] = term
        os.environ["SSH_TTY"] = os.ttyname(0) if os.isatty(0) else "/dev/pts/0"
        os.environ["SSH_CONNECTION"] = "127.0.0.1 50000 127.0.0.1 %d" % PORT
        os.environ["SSH_CLIENT"] = "127.0.0.1 50000 %d" % PORT
        os.environ["LANG"] = "C.UTF-8"
        command = process.command
        if command:
            os.execvp("bash", ["bash", "-lc", command])
        else:
            os.execvp("bash", ["bash", "-l"])
        os._exit(1)

    # Parent side: size the PTY as the client requested.
    import fcntl
    import struct
    import termios

    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    loop = asyncio.get_running_loop()
    reader = asyncio.StreamReader()
    await loop.connect_read_pipe(lambda: asyncio.StreamReaderProtocol(reader), os.fdopen(fd, "rb", 0))

    async def pty_to_client():
        try:
            while True:
                data = await reader.read(4096)
                if not data:
                    break
                process.stdout.write(data.decode("utf-8", "replace"))
                await process.stdout.drain()
        except Exception:
            pass

    task = asyncio.create_task(pty_to_client())
    try:
        async for line in process.stdin:
            os.write(fd, line.encode())
    except Exception:
        pass

    await task
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    os.waitpid(pid, 0)
    process.exit(0)


async def main():
    host_key = sys.argv[1]
    client_pub = sys.argv[2]

    await asyncssh.create_server(
        Server,
        "127.0.0.1",
        PORT,
        server_host_keys=[host_key],
        authorized_client_keys=client_pub,
        process_factory=handle_session,
    )
    print(f"test SSH server listening on 127.0.0.1:{PORT}", flush=True)
    await asyncio.Future()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except (KeyboardInterrupt, SystemExit):
        pass
