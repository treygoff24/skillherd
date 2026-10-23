# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Before 1.0, a minor version may
change the command line, the configuration or the library file formats.

## [Unreleased]

First public release.

### Changed

- **Breaking:** renamed the package and command from `skill` / `skill-cli` to
  `skillherd`, avoiding the unrelated `procps` command. Update scripts and
  aliases. The installer removes an old `skill` only when ownership is proven;
  foreign binaries, including `procps`, are preserved.
- Configuration now uses `~/.config/skillherd/config.json`, tool-local state
  uses `XDG_STATE_HOME/skillherd`, and npm cache uses
  `XDG_CACHE_HOME/skillherd/npm`. Move configuration and review old state
  manually. There is no old config, path or environment fallback, and no
  automatic migration. `doctor` reports an old configuration directory as an informational finding
  when the new directory is absent.
- Environment variables now use `SKILLHERD_*`; `SKILL_RENDER_PLATFORM` becomes
  `SKILLHERD_PLATFORM`, and `SKILLS_REAL_NPX` becomes `SKILLHERD_REAL_NPX`.
  Standard harness environment variables are unchanged.
- Platform output is `mac` or `linux`. The legacy `cell` alias remains valid
  for the platform environment, `strip --keep`, policy `!cell`, `--no-cell`
  and platform fences; `--no-linux` is canonical. New snapshots use `linux`;
  legacy `cell` snapshots are read only when the `linux` snapshot is absent.
- The optional shim uses `# skillherd-npx-shim: 1`; the old marker is accepted.
  The library location and internal library filenames remain unchanged.

### Added

- `skillherd`: serve one plain-folder or Git skill library to Claude Code, Codex,
  pi and the shared `~/.agents/skills` folder, per a policy file kept in the
  library (`on`, `off`, `render`, `list`, `search`, `info`).
- Managed-entry ownership: the tool replaces or removes only entries it can
  prove it created, re-checked immediately before each change.
- Per-harness variants and harness and platform text fences, served as
  stripped copies.
- Upstream imports through `npx skills` in a scratch home (`install`,
  `update`, `remove`, `reconcile`), with import records, local-edit detection
  and a crash-safe replace protocol; GitHub release-asset imports.
- Project activation (`add`, `drop`), diagnostics (`doctor`, `doctor --fix`),
  Git library sync (`sync`) and a machine-readable contract (`schema`).
- An optional `npx` shim (`install.sh --with-npx-shim`) and `skillherd npx` for
  routing pasted `npx skills` commands through the library.
- Continuous integration on macOS and Linux.
