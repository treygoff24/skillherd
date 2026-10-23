# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Before 1.0, a minor version may
change the command line, the configuration or the library file formats.

## [Unreleased]

## [0.1.0] - 2026-10-23

First public release.

### Added

- `skillherd`: serve one plain-folder or Git skill library to Claude Code,
  Codex, pi and the shared `~/.agents/skills` folder, per a policy file kept
  in the library (`on`, `off`, `render`, `list`, `search`, `info`).
- Managed-entry ownership: the tool replaces or removes only entries it can
  prove it created, re-checked immediately before each change.
- Per-harness variants and harness and platform text markers, served as
  stripped copies; `skillherd strip` applies the same markers to any file.
- Upstream imports through `npx skills` in a scratch home (`install`,
  `update`, `remove`, `reconcile`), with import records, local-edit detection
  and a crash-safe replace protocol; GitHub release-asset imports.
- Opt-in serving of Claude Code subagents shipped with imported skills.
- Project activation (`add`, `drop`, experimental), diagnostics (`doctor`,
  `doctor --fix`), Git library sync (`sync`) and a machine-readable contract
  (`schema`).
- An optional `npx` shim (`install.sh --with-npx-shim`) and `skillherd npx`
  for routing pasted `npx skills` commands through the library.
- `install.sh`, which builds and installs the binary without overwriting a
  `skillherd` or `npx` it did not install.
- A skill that teaches agents to use the CLI (`skills/skillherd/SKILL.md`)
  and a setup guide for agents installing the tool (`docs/agent-setup.md`).
- Continuous integration on macOS and Linux.
