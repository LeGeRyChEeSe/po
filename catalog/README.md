# Po's community catalog

Extensions made by Po's users, browsable and installable from Po with `/catalog` (`/catalogue`
in French). An extension changes what Po knows or how it looks, never Po's code: it is installed
and removed at once, without updating Po.

## What an extension can hold

An extension is a folder `catalog/<name>/` (lowercase letters, digits and dashes) with:

- `extension.toml` — what it is (see below);
- `skills/<skill>/SKILL.md` and the skill's other files — skills, put in Po's skills folder;
- `agents/<agent>.md` — subagents, put in Po's subagents folder;
- `strata/<preset>.json` — settings added to a Strata config when a model's profile chooses
  that preset (`/profile` → Preset); Strata's own config is never changed. The keys are
  `description`, `args`, `vision_args`, `drop`, `env`, `learn_profile`, `vision_on_spare_card` and
  `template` (see `catalog/strata-tuned/`);
- `statusline.json` — a status line (the format `/statusline` keeps). A line that runs commands
  is installed only once the extension is certified.

Po installs nothing over a skill or a subagent the user already has, and remembers what each
extension put where to take it all back (`/catalog remove <name>`).

## `extension.toml`

```toml
title = "Conventional commit messages"
description = "A skill that writes the commit message of the changes in progress."
category = "commands"   # appearance, commands, tools, models, sessions, performance, accessibility, other
author = "github-login"

# Set by Po's maintainers only:
certified = false       # tried by them
issue = 12              # the GitHub issue (label `catalog`) where it is voted for
```

## Proposing one

With Po Labs, ask Po for what you want: when a skill, a subagent or a status line does it, Po Labs
makes it as an extension, and sharing it opens a pull request adding its folder here. You can also
open the pull request yourself.

The maintainers read it, try it, and on merging open its issue (label `catalog`) and set `issue`;
`certified = true` marks the ones they tried. Votes are the 👍 of that issue: one per GitHub account,
given or taken back with `/catalog vote <name>`.

`cargo test` checks every extension of this folder.
