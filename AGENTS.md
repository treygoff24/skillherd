# Contributing to skillherd

Instructions for anyone changing this repository, human or coding agent.
`CLAUDE.md` is a link to this file.

## What is here

- `skill/`: the `skillherd` CLI, a Rust crate (edition 2024, Rust 1.88 or newer).
  The binary is `skillherd`; integration tests are in `skill/tests/`, with shell
  fixtures (stub `npx` and `gh`, a local upstream builder, and stripper
  fixtures recorded from the old Python stripper) in `skill/tests/fixtures/`.
- `install.sh`: builds the crate and copies the binary into a prefix.
- `skill/scripts/npx`: the `npx` shim script. `skillherd npx` embeds it at build
  time; `bin/npx` is a link to it, and `install.sh --with-npx-shim` installs
  it as an optional `npx` wrapper.
- `scripts/third-party-notices.sh`: regenerates `THIRD-PARTY-NOTICES`.
- `tests/`: the gate, the end-to-end acceptance script and the shell suites.

Read [ARCHITECTURE.md](ARCHITECTURE.md) before changing anything that writes
to disk. Its three sections (the managed-entry rule, the replace protocol and
the render algorithm) describe the behaviour every change must keep.

## Rules that hold

- **Only touch what you can prove is ours.** A removal or replacement on a
  surface needs a managed-entry proof, re-checked immediately before the
  system call (`managed::verify_expected`, `surfaces::apply_ops`,
  `doctor::park_link`). Anything else is reported and left alone. Do not add a
  deletion that skips the re-check, and do not add a second code path that
  writes to surfaces.
- **Never delete the library's only copy.** Replacements go copy, displace,
  no-replace rename, commit, clean up. A run deletes under `.npx/backup` only
  what it displaced itself, in the same process, after the commit. Use
  `fsx::rename_noreplace`, never `fs::rename`, for anything that lands on a
  live path.
- **Upstream code gets a scratch home, not the real one.** `npx` runs in a
  marked, per-run stage with its own working directory, a cleared and rebuilt
  environment and npm configuration the tool controls (`npx::run`). Credential
  variables and the user's Git and npm config are passed only with
  `SKILLHERD_NPX_CREDENTIALS=1`. This is redirection, not a sandbox: do not
  describe it as isolation, and do not widen what reaches the child without
  saying so in `README.md`.
- **Only the stage is an inbox.** Imports read only marked run folders under
  the stage (`npx::recoverable_inboxes`). `~/.agents/skills` is a surface; never
  scan it for arrivals. An arrival that was refused or interrupted is kept
  until the user adopts, keeps or discards it.
- **Destinations come from this machine.** Nothing read from a library may
  choose a write path: `.globals/harnesses` can only select an entry of
  `policy::HARNESS_REGISTRY`, whose paths come from local options,
  environment, config and defaults.
- **Lock before reading.** Every command that changes state takes the library
  lock before it reads policy or records. `--dry-run` takes no lock and
  changes nothing it reports on (a dry-run import may still clone upstream
  into the system temporary folder to plan variants).

## Testing

```bash
tests/skill-gate.sh                  # fmt, clippy -D warnings, every Rust test, release build
SKIP_GATE=1 tests/acceptance.sh      # end-to-end against the release binary, scratch HOME
bash tests/install.test.sh           # installer, with a stub cargo
bash tests/build-check.test.sh       # the shell syntax checker itself
bash tests/shell-check.sh            # bash -n and shellcheck on every script
```

Run one Rust suite while iterating with
`cargo test --manifest-path skill/Cargo.toml --test reconcile`.

The test scripts need `python3`, `git`, `shasum` and, for the shell checks,
`shellcheck`. The tool itself needs none of Python or shellcheck.

- **Never run the tool against your real home while testing.** Every test and
  probe uses a scratch `HOME` under `$TMPDIR`; `tests/acceptance.sh` shows the
  pattern (`env -i`, `HOME=<scratch>`, explicit `SKILLHERD_*` paths).
- **Every new test must be seen failing.** Run it against the code before the
  fix, or with the guard it tests disabled, and watch it fail before its
  passing result counts. Say in the change which test guards what.
- **Keep tests platform-neutral.** CI runs the gate on macOS and Linux. Do not
  assert a platform name or a path layout that only one of them produces.

## Changes

- Small, focused commits with conventional subjects:
  `feat|fix|test|docs|refactor|chore(scope): summary`, where scope is
  `skillherd`, `install`, `npx` or a test suite.
- Shell changes pass `bash tests/shell-check.sh`.
- Changes to the crate pass `tests/skill-gate.sh`. Changes to what the tool
  writes, imports or serves also pass `SKIP_GATE=1 tests/acceptance.sh`.
- Update `README.md` when user-visible behaviour changes, and
  `ARCHITECTURE.md` when one of its three rules changes.
- When dependencies change, regenerate the license file with
  `bash scripts/third-party-notices.sh > THIRD-PARTY-NOTICES`; CI fails when
  it is out of date.
