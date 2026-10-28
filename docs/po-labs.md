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

Requests stay on your machine, in `po/labs/demandes.jsonl` in your user configuration folder.
Nothing is sent anywhere.

## Where it is going

1. **Try it live.** Po applies a recorded change to its own code in an isolated copy, builds
   and tests it, and restarts on the trial version without losing your session. You keep it,
   roll it back, or ask for adjustments. If the trial version crashes, Po rolls back on its own.
2. **Share it.** With your consent and after a clear summary, Po turns your changes into
   proposals: extension changes become a shareable configuration, core changes become a
   separate pull request to Po itself.
3. **Configurations for everyone.** A catalog of configurations made by users — browsable from
   Po, applied without updating the application, ranked by use and recommendations, with the
   ones tested by the maintainers marked as approved. The most appreciated changes may become
   part of Po itself, credited to the people who proposed them.

Po's core always stays intact: configurations extend Po, they never remove what it needs to
run.
