---
name: skillherd
description: >-
  Manage the user's agent skills with the skillherd CLI: list which skills
  Claude Code, Codex, pi and ~/.agents/skills see, turn skills on or off,
  write a new skill, import or update one from skills.sh or GitHub, and
  diagnose a missing or shadowed skill. Use when asked to add, enable,
  disable, update, remove or find a skill.
---

# skillherd

`skillherd` keeps one skill library and serves chosen skills into each
harness's skill folder as links (or stripped copies). The library and its
policy file, `<library>/.globals/sets`, are the source of truth; the harness
folders are output. Change the library and the policy, never the harness
folders.

Every command prints a JSON envelope: read `ok`, then `data`, or
`error.code` and `error.suggested_fix`. `meta.paths.library` is the library's
location. `skillherd schema` prints the full contract.

## Look

```bash
skillherd list --all                 # every skill and the harnesses that see it
skillherd list --harness claude      # what one harness sees
skillherd info NAME                  # tiers, visibility, variants, upstream record
skillherd search WORD
skillherd doctor                     # exit 1 when warnings remain
```

## Change

Run each change with `--dry-run` first, show the user the plan, then run it
for real.

- **Turn on**: `skillherd on NAME` serves it to Claude Code and Codex.
  `--tier lane` also serves the shared `~/.agents/skills` folder and pi.
  `--only H` / `--not H` narrow it to or away from one harness.
- **Turn off**: `skillherd off NAME`.
- **New skill**: write `<library>/NAME/SKILL.md` (frontmatter `name` and
  `description`), run `skillherd on NAME`, and add `NAME` to
  `<library>/.globals/house` to record that the user maintains it.
- **Edit a skill**: edit `<library>/NAME/`. A skill served as a stripped copy
  (it has `<!-- NAME-only -->` markers) needs `skillherd render` afterwards.
- **Upstream skill**: `skillherd install SOURCE --skill NAME` imports without
  activating. Read the imported `SKILL.md` and summarise it for the user before
  `skillherd on NAME`. `skillherd update [NAME]` and `skillherd remove NAME`
  maintain it.

## Ask the user first

- Installing or updating any upstream skill: it runs third-party code through
  `npx` and puts new instructions in front of every agent.
- `--adopt NAME` (discards their local edits), `--keep NAME`,
  `remove --force`, `reconcile --discard`, and `doctor --fix`.
- `SKILLHERD_NPX_CREDENTIALS=1`, which hands tokens and the SSH agent to the
  `npx` run.
- Moving or deleting anything `skillherd` reports as foreign
  (`foreign_entry`, `foreign_shadow`): another tool or the user put it there.
- Committing or pushing a Git library.

## When something fails

- `foreign_shadow`: a folder the tool did not make holds that name in a
  harness folder, so the skill is skipped there. Report the path.
- `local_edit_refused`: the library copy differs from what was imported. Show
  the difference and ask: `--adopt NAME` takes upstream, `--keep NAME` keeps
  the local copy and stops tracking upstream.
- `library_busy`: another `skillherd` run holds the library lock. Retry.
- `npx_failed`: read `error.details.output_tail`; a private source may need
  `SKILLHERD_NPX_CREDENTIALS=1` (ask first).
- `policy_unrepresentable`: no harness would see that row; pick another tier
  or harness.
