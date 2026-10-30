# Po Labs

**Po Labs** lets you shape Po just by talking about it, and helps Po improve from what real
users actually need.

## Turning it on

Po Labs is **off by default**. Turn it on with `/labs on`, or with the first line of the
settings (F2). The **Labs** light under the input shows its state at all times; tap it to see
what was recorded.

## What it does today

Once on, talk about Po the way you talk about your project — in the same message if you like:

> Fix the failing test in `parser.rs`. Also, the subagent card is too small on my phone.

The model handles your project request as usual, and records the part about Po separately with
its `po_labs` tool: a short, self-contained summary, its nature (bug, improvement, idea), and
whether it touches Po's **core** (agent, models, tools, sessions) or only its appearance and
extensions. A card shows you what was recorded; `/labs` lists everything.

When you turn Po Labs on in the middle of a conversation, the model also goes back over what
you already said and records the requests about Po it finds there.

Requests stay on your machine, in `po/labs/notebook.json` in your user configuration folder.
Nothing is sent anywhere.

## Trying a change

When Po is idle, a discreet proposal offers to implement what was recorded (`Ctrl+Y`). Po Labs
works like a pupil with homework: **one change at a time**, the most important first.

- It works in **its own copy of Po's source**, a git repository in `po/labs/workspace` (handled
  in Rust with gitoxide: no `git` program is needed, and none of your git hooks run). Your own
  checkout of Po is never touched.
- It builds Po, installs it, records the change as a commit of that copy, and restarts your
  session on the new version. If the new version does not start, Po goes back to the previous one.
- The change is then **on trial**. Po asks what you think only once you could see it at work:
  a text at once, how tool calls look after a few calls, the rest after your next message.
- Tap the Po Labs card (or `Ctrl+Y`) to see it, then:
  - **Keep** (`o` in French, `y` in English): its requests are done;
  - **Undo** (`n`): the copy and the installed Po go back exactly to before it, and Po restarts;
  - **Adjust** (`m` / `e`): say what to change; Po Labs takes it up and you try it again.
- The next request waits until you have answered. Po never blocks you: your session goes on as
  usual meanwhile, and whatever you say about Po is sorted — a remark on the change on trial, an
  update of a request already noted, or a new one.

## Contributions

Each change you keep becomes a **contribution**: Po Labs writes it with your model — a title and a
description in English for Po's maintainers, and what it changes in plain words in your
language. It also says whether the change touches **Po's core** (how it works: agent, models,
tools, sessions) or is an **extension** (its look, its texts); fixed rules on the files changed
have the last word. With one slot on your server, it is written when you next let Po Labs work
(`Ctrl+Y`); with more, right after you keep the change.

Once everything in the notebook is done, a discreet line says that contributions are ready to
share. Open them (`Ctrl+Y` or the Po Labs card): one page each, `←`/`→` or the arrows to go from
one to the next, and for each:

- **Share** (`p` in French, `s` in English): Po first shows exactly what leaves and where, and
  sends it only once you say yes (`o` / `y`). See [Sharing](#sharing);
- **Keep for later** (`g` / `l`): set aside, to decide later with `/contributions`;
- **Delete** (`s` / `d`): the contribution is gone, the change stays in your Po;
- **Edit** (`m` / `e`), then:
  - **the text** (`t`): say what to change in the title or the description; the code does not
    change;
  - **the code** (`c`): Po switches to the version of this contribution exactly. Use it as usual
    and say what to change: Po Labs adjusts the contribution, you try it, then keep or undo your
    changes as for any change on trial. Po then goes back to its latest version — the
    contributions made after this one are replayed on top — and the contribution, written again,
    comes back among those to share.

A summary of what was sent, kept and deleted closes the review. Long work — switching to a
contribution's version, going back to the latest one, sending — runs in the background: Po stays
usable meanwhile, and the Po Labs card says what it is doing.

## Sharing

A shared contribution becomes a pull request to Po's repository, from your own copy of it on
GitHub (made if needed). What leaves: the title, the description for the developers and the
content of the changed files — nothing else: not your conversations, not your notebook, and not
your email address (the commit is signed with GitHub's private address of your account).

The change is put on top of Po's latest version. If one of its files changed there since, nothing
is sent: update Po and make the change again with Po Labs.

Signing in: if GitHub CLI is installed and connected (`gh auth login`), Po uses its connection.
Otherwise Po shows a GitHub page and a code to enter there, from any device; the connection is
then remembered for all your projects (`github.json` next to Po's settings).

Afterwards, a core change may become part of a future version of Po, and you are credited among
its contributors; an extension will be offered in the community catalog once approved.

## Extensions and the community catalog

When a skill, a subagent or a status line is enough for what you ask, Po Labs makes it as an
**extension** rather than changing Po's code: a folder in Po's `catalog/`, installed at once,
without a build — Po just restarts to read it. You try it, keep it, undo it or adjust it like any
other change, and sharing it proposes it for the catalog.

`/catalog` (`/catalogue` in French) shows the catalog by category, the ones tried by Po's
maintainers marked certified, then by votes:

- `/catalog <category>` — one category only;
- `/catalog show <name>` — what it does and the content of its files, to read before installing;
- `/catalog install <name>` — in place at once, never over a skill or subagent of yours;
- `/catalog remove <name>` — taken back entirely, status line included;
- `/catalog vote <name>` — a 👍 on its GitHub issue, one per account; again to take it back.

Without a server: Po reads the catalog from Po's repository. The format is described in
[catalog/README.md](../catalog/README.md).

## One Po Labs for every Po open

Po Labs follows you from one project to another: its notebook, its change on trial and its own
session are shared by every Po open on the machine, and stay as you left them when you close Po.
Open Po in another project, and Po Labs is there, exactly where it was.

With several Po open at once, they all see the same notebook, live. Po Labs is held by one
session at a time — the one where you opened it: in the others, its card is greyed ("open in
project-a"), with no proposal, no question and no card opening by itself. What you say about Po
there is still written in the notebook. To bring Po Labs over, press `Ctrl+Y` twice in the other
session; it moves only while it rests, never in the middle of its work. When Po Labs installs a
new version, every Po open restarts on it once idle, with an empty input.

Po's core always stays intact: extensions add to Po, they never remove what it needs to run.
