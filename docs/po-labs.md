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

## Where it is going

1. **Share it.** With your consent and after a clear summary, Po turns your changes into
   proposals: extension changes become a shareable configuration, core changes become a
   separate pull request to Po itself.
2. **Configurations for everyone.** A catalog of configurations made by users — browsable from
   Po, applied without updating the application, ranked by use and recommendations, with the
   ones tested by the maintainers marked as approved. The most appreciated changes may become
   part of Po itself, credited to the people who proposed them.

Po's core always stays intact: configurations extend Po, they never remove what it needs to
run.
