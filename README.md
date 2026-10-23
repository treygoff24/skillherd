# skillherd

Every coding agent reads skills from its own folder, so keeping the same skills
in Claude Code, Codex, pi and the shared `~/.agents/skills` folder means
copying them around by hand, and the copies drift.

`skillherd` keeps one plain-folder or Git skill library and serves a chosen subset
to each agent harness through links it can prove it created. It re-checks that
proof before every removal, never touches entries it did not create, never
deletes the library's only copy, and imports upstream skills in a scratch home
instead of your real one.

The project is early (version 0.1, no tagged release yet). Output is JSON,
written for scripts and agents first; `on`, `off`, `render` and `doctor` print
a short summary instead when run in a terminal.

## Is this the right tool?

If all you want is the same skills in every agent, there are lower-friction
options, and you should try them first:

- [`npx skills`](https://skills.sh) (Vercel's `skills` CLI) installs a skill
  into the folders of many agents with one command and keeps a lock file. It
  has no persistent activation policy: each install decides where copies go.
- [skillshare](https://github.com/runkids/skillshare) syncs one library into
  many targets with per-target include and exclude rules, and pushes and pulls
  the library with Git. "One library, many harnesses" is not new; skillshare
  does it well.

`skillherd` is for people who want, in addition:

- **A written activation policy.** One file in the library says which skills
  each harness sees, with per-harness and per-platform exceptions. Importing a
  skill and turning it on are separate steps.
- **Provable ownership.** The tool removes or replaces only entries it can
  prove it created, and checks that proof again immediately before each
  change. Anything else on a skill folder is reported and left alone.
- **Per-harness text.** A skill can carry a different version for one harness,
  or blocks of text fenced for one harness or one platform; the tool serves each
  harness its own copy.
- **Contained upstream imports.** `skillherd install` runs `npx skills` in a
  scratch home and imports the result into your library, so upstream installs
  never scatter copies across your real agent folders.

## Platforms and requirements

- **macOS and Linux.** Other platforms are refused at compile time: the tool
  depends on atomic no-replace renames (`renamex_np` on macOS, `renameat2` on
  Linux) and will not fall back to a racy check-then-rename.
- **Rust 1.88 or newer**, Cargo and a C linker, to build.
- **Bash**, for `install.sh`.
- **Git**, for Git-backed libraries and upstream imports.
- **Node.js and `npx`**, only for `skillherd install`, `update` and `remove` from
  skills.sh sources.
- **`gh` or a `GH_TOKEN`/`GITHUB_TOKEN`**, only for GitHub release-asset
  imports.

Python is not needed.

## Install

```bash
git clone https://github.com/treygoff24/skillherd
cd skillherd
bash install.sh
export PATH="$HOME/.local/bin:$PATH"
command -v skillherd     # must print ~/.local/bin/skillherd
skillherd --version
```

The installer builds the binary in release mode and copies it into
`~/.local/bin`. It does not edit your shell configuration, sync a library or
install anything else unless you ask. If the destination already holds a
`skillherd` it did not install, it stops without changing anything. It records
checksums under `PREFIX/share/skillherd/` so later installs can recognise the
previous build without executing it. A recognised pre-receipt install is
adopted and reported; a receipt mismatch is treated as a local edit.

The command is `skillherd`, not `skill`. Debian and Ubuntu's `procps` package
ships an unrelated `skill` command that signals processes. The installer
preserves that program and any other foreign `skill`. When upgrading, it
removes an old `skill` binary only when it can prove this tool owns it;
otherwise it leaves it in place and reports it. Update scripts and aliases to
call `skillherd`.

Other options:

```bash
bash install.sh --prefix "$HOME/tools"            # installs into ~/tools/bin
bash install.sh --prefix "$HOME/tools" --verify   # report drift, change nothing
cargo install --locked --path skill               # into ~/.cargo/bin instead
```

`--verify` reports `same`, `DIFF`, `MISSING` or `UNWANTED` per file and exits 1
on drift. An owned optional wrapper is unwanted when its install flag is
absent. Verification never removes it. `--root DIR` is an older spelling of
`--prefix DIR/.local`; an explicit `--prefix` wins. Builds use
`CARGO_TARGET_DIR` if set, otherwise `skill/target`; Cargo's download and build
caches remain for the next install.

### The optional `npx` shim

`bash install.sh --with-npx-shim` also installs an `npx` wrapper into the same
folder. With that folder first on `PATH`, a pasted `npx skills add …`,
`npx skills update …` or `npx skills remove …` runs through `skillherd install`,
`update` or `remove`, so it lands in your library instead of your agent
folders. Unrelated commands and known read-only `skills` commands pass through
unchanged. Unknown `skills` commands, unsupported mutation flags and requested
package versions other than `${SKILLHERD_NPX_PKG:-skills@1.7.0}` are refused, not
silently sent to the real tool against your home directory. Only the package
position selects `skills`: `npx some-pkg skills add …` remains `some-pkg`'s call.

The real `npx` is selected in PATH order, skipping recognised copies of this
shim, just like the importer. `SKILLHERD_REAL_NPX` explicitly overrides the shim's
discovery. A foreign `npx` at the install destination is never overwritten.

The shim is off by default because it sits in front of every `npx` call on your
machine. Without it, `skillherd npx skills add …` does the same routing for one
command. All arguments after `npx` belong to npx; put skillherd's flags before it:

```bash
skillherd --dry-run npx skills add owner/repo
skillherd npx some-pkg --dry-run
```

Reinstalling **without** `--with-npx-shim` removes an owned wrapper, including
the old default-installed wrapper, and prints a warning explaining how to keep
it. Foreign or locally modified wrappers stay put. A retired owned
`skill-render` helper is also removed: rendering is now built into `skillherd`.

### Upgrading from an earlier build

This is a breaking rename from `skill` / `skill-cli` to `skillherd`.

- Update commands, scripts and aliases to use `skillherd`.
- Move configuration from `~/.config/skill/config.json` to
  `~/.config/skillherd/config.json` (or the corresponding `XDG_CONFIG_HOME`
  paths). Rename `SKILL_*` variables to `SKILLHERD_*`, except
  `SKILL_RENDER_PLATFORM`, which becomes `SKILLHERD_PLATFORM`.
  `SKILLS_REAL_NPX` becomes `SKILLHERD_REAL_NPX`.
- Tool-local state and npm cache now live under `XDG_STATE_HOME/skillherd`
  and `XDG_CACHE_HOME/skillherd/npm` (by default `~/.local/state/skillherd`
  and `~/.cache/skillherd/npm`). Review old state before moving it; the tool
  does not migrate it automatically. There is no fallback to old config,
  state, cache or environment names. `skillherd doctor` reports an old configuration directory
  as an informational migration finding when the new directory is absent; it does not move or delete them.
- The library remains `~/.agents/skill-library`. Its internal filenames and
  policy formats remain unchanged, including `.skill.lock`, `.skill-stage`,
  `.skill-stage-owner`, `.globals/` and `.npx/skill-lock.*.json`.
- Platform output is now `linux` rather than `cell`. `cell` remains accepted
  in `SKILLHERD_PLATFORM`, `strip --keep`, policy `!cell`, `--no-cell` and
  `cell-only` fences; `--no-linux` is the canonical flag. New lock snapshots
  use `.npx/skill-lock.linux.json`; the legacy `cell` snapshot is read only
  when the `linux` snapshot is absent.
- The optional `npx` shim uses `# skillherd-npx-shim: 1`; the old marker is
  still recognised. Reinstall with `--with-npx-shim` to keep the wrapper.
- Subagents are served only for skills listed in `.globals/agents` (see
  [Harness coverage](#harness-coverage)). List any you want before rendering.
- Lock snapshots are opt-in with `SKILLHERD_SNAPSHOT_LOCK=1`. Without that
  opt-in, existing snapshots are left untouched and ignored.

## Quick start with a folder

A library holds one directory per skill, each with a `SKILL.md`:

```bash
mkdir -p "$HOME/my-skills/foo"
cat > "$HOME/my-skills/foo/SKILL.md" <<'EOF'
---
name: foo
description: Review a change for missing error handling.
---
Inspect error paths and report errors that are silently ignored.
EOF
skillherd --pool "$HOME/my-skills" on foo
skillherd --pool "$HOME/my-skills" list --harness claude
skillherd --pool "$HOME/my-skills" doctor
```

`on` records the activation in the library's policy file and then renders it:
`~/.claude/skills/foo` and `~/.codex/skills/foo` become links to
`~/my-skills/foo`. Pass `--pool` on every command, or save the library path in
the configuration file and drop it:

```bash
mkdir -p "$HOME/.config/skillherd"
printf '{ "library": "~/my-skills" }\n' > "$HOME/.config/skillherd/config.json"
skillherd list --all
```

A plain-folder library needs no Git repository; `skillherd sync` on it only
renders.

## Use a Git library

```bash
mkdir -p "$HOME/.config/skillherd"
cat > "$HOME/.config/skillherd/config.json" <<'EOF'
{
  "library": "~/my-skills",
  "remote": "https://github.com/example/skill-library.git"
}
EOF
skillherd sync
skillherd on foo
```

Replace the example URL with your repository. Skills must sit at the top level
of the repository (`foo/SKILL.md`), not in a subfolder.

When the library folder does not exist yet, `sync` clones into private scratch
space and then moves the clone into place without replacing anything. When it
exists, `sync` requires the configured URL to match `origin` and tracked files
to be clean, runs `git pull --no-rebase --ff-only origin`, and renders. It
never pushes, resolves conflicts or discards local edits. Local paths and SSH
URLs work too; Git handles authentication. Until the first `sync` has cloned
the library, other commands that change it refuse to run, so they cannot
create an empty folder that would block the clone.

The library's `.globals/` folder is your policy: commit it. Add `.skill.lock`
(the tool's lock file) to the library's `.gitignore`. Imports commit their own
paths with your usual Git identity.

## Coming from `npx skills`

| You ran | Now run |
| --- | --- |
| `npx skills add owner/repo --skill foo -g` | `skillherd install owner/repo --skill foo`, then `skillherd on foo` |
| `npx skills update` | `skillherd update` |
| `npx skills remove foo -g` | `skillherd remove foo` |
| `npx skills list` | `skillherd list --all` |

`skillherd install` runs the pinned `skills@1.7.0` package in a scratch home (the
stage), imports what it installed there into your library with an import
record (`.npx/foo.json`), and copies its lock file to the real lock location.
It does not activate the skill; `skillherd on` does. The stage this tool creates
for each run is the only place it imports from.

If an import is refused (a local edit, say), the stage is kept, because it may
hold the only copy of what was downloaded. `skillherd reconcile --adopt foo` or
`--keep foo` finishes it; `skillherd reconcile --discard foo` deletes the kept
arrival and never touches the copy in your library. `--dry-run` shows either
first.

Skills that `npx skills add -g` already put in `~/.agents/skills`, and the
links it made in `~/.claude/skills`, are not this tool's: `skillherd` leaves them
where they are and never imports or deletes them. If one has the same name as a
skill you activate, `skillherd` reports it (`foreign_shadow` in `skillherd doctor`) and
does not replace it. To move such a skill under `skillherd`, remove the old copy
first with the real `npx skills remove <name> -g`, then run
`skillherd install <source> --skill <name>` and `skillherd on <name>`. Both tools use
the same lock file, so doing it in the other order would make the removal
forget the new install.

## Configuration

Settings come from, in order of precedence: the global command-line options,
environment variables, then `~/.config/skillherd/config.json` (or
`$XDG_CONFIG_HOME/skillherd/config.json`; `SKILLHERD_CONFIG` names another file).

| Setting | JSON key | Environment | Option | Default |
| --- | --- | --- | --- | --- |
| Library folder | `library` | `SKILLHERD_LIBRARY` | `--pool DIR` | `~/.agents/skill-library` |
| Library remote | `remote` | `SKILLHERD_POOL_REMOTE` | `--pool-remote URL` | none |
| Custom sync command | `sync_command` | `SKILLHERD_SYNC_COMMAND` | | none |
| Claude Code root | `claude_home` | `CLAUDE_SHARED`, `CLAUDE_CONFIG_DIR` | | `~/.claude` |
| Codex root | `codex_home` | `CODEX_HOME` | | `~/.codex` |
| Shared skills folder | `agents_dir` | `SKILLHERD_AGENTS_DIR` | | `~/.agents/skills` |
| pi skills | `pi_skills` | `SKILLHERD_PI_SKILLS` | | `~/.pi/agent/skills` |
| Cursor skills | `cursor_skills` | `SKILLHERD_CURSOR_SKILLS` | | `~/.cursor/skills` |
| Grok root | | `GROK_HOME` | | `~/.grok` |

Claude's root has a specific precedence: `CLAUDE_SHARED` overrides an explicit
`claude_home` in the JSON file; that setting overrides the session's
`CLAUDE_CONFIG_DIR`; otherwise the root is `~/.claude`. No other folder is
picked up automatically.

Less common environment variables:

| Variable | Meaning | Default |
| --- | --- | --- |
| `SKILLHERD_STAGE` | Folder that holds the per-run scratch homes for `npx`; must be inside your home folder | `~/.agents/.skill-stage` |
| `SKILLHERD_LOCK` | The `skills` lock file | `$XDG_STATE_HOME/skillherd/.skill-lock.json` if `XDG_STATE_HOME` is set, else `~/.agents/.skill-lock.json` |
| `SKILLHERD_NPX` | The real `npx` to run | the first `npx` on `PATH` that is not the shim |
| `SKILLHERD_NPX_PKG` | The `skills` package to run | `skills@1.7.0` |
| `SKILLHERD_NPX_CREDENTIALS` | `1` passes your credentials to the `npx` run (see [Trust boundary](#trust-boundary)) | off |
| `SKILLHERD_NPX_TELEMETRY` | `1` leaves the `skills` package's telemetry on | off |
| `SKILLHERD_SNAPSHOT_LOCK` | `1` keeps a copy of the lock file in the library, `.npx/skill-lock.<platform>.json`, committed after each import | off |
| `SKILLHERD_GH` | The `gh` binary for release imports | `gh` on `PATH` |
| `SKILLHERD_PLATFORM` | Platform for platform fences: `mac` or `linux` (`cell` is a legacy alias) | the running platform |
| `XDG_CACHE_HOME` | If absolute, parent of the `npx` run's npm cache, `skillherd/npm` | `~/.cache` |
| `XDG_STATE_HOME` | If absolute, parent of the tool's own state folder, `skillherd` | `~/.local/state` |

Paths in the JSON file may start with `~/` and are otherwise relative to your
home folder; paths from options and environment are relative to the working
directory. Keep the library separate from `~/.agents/skills`, which is a place
skills are served to, not a source.

Where `skillherd` writes is decided by this machine's configuration only: the
settings above and the built-in defaults. A library you clone from someone
else cannot point the tool at another folder on your disk. Before planning
anything, every destination is checked against the library, the stage and the
other destinations, with links resolved, and an overlap is refused.

The `.globals/harnesses` surface column names a local destination, not a path:
`claude`, `codex`, `agents`, `grok`, `pi` or `cursor`. Only these built-in
destinations are supported for now; relocate them with the local settings above
(for example, `claude_home`), not a custom path in shared policy. The exact
legacy tokens such as `{claude_shared}/skills` remain aliases for those names;
there is no general `{name}` expansion. Policy tokens are limited to 128 bytes,
and diagnostics show only a bounded offending token, never the whole row.

A custom `sync_command` replaces the built-in `sync`, rendering included. It
receives `SKILLHERD_LIBRARY` and, when set, `SKILLHERD_POOL_REMOTE` in its environment.
It runs as you; configure only a command you trust.

## Harness coverage

| Harness | Folder `skillherd` serves | Served by default |
| --- | --- | --- |
| Claude Code | `~/.claude/skills` | yes (`main` tier) |
| Codex | `~/.codex/skills` | yes (`main` tier); Codex also reads `~/.agents/skills` |
| Shared folder | `~/.agents/skills` | yes (`lane` tier, universal copies only) |
| pi | `~/.pi/agent/skills` | yes (`lane` tier) |
| Grok | `~/.grok/skills` | no: nothing is served there, and links the tool made there earlier are removed |
| Cursor | `~/.cursor/skills` | no: Cursor reads `~/.agents/skills` instead |

With the default table, Grok and Cursor see only what is in the shared folder.
A row no harness would see is refused rather than saved: `skillherd on foo --only
grok` fails with `policy_unrepresentable`, because Grok has no tier of its own
and the shared folder never serves an `only:` row.

Claude Code and Codex behaviour is covered by the test suite against these
folder layouts; whether Grok, Cursor and pi load exactly these folders has not
been verified against those harnesses. A harness may also read other folders,
or have compatibility settings that expose more skills than this policy
intends; `skillherd` manages only the folders above.

Some upstream repositories ship Claude Code subagents beside a skill. An
import brings in the ones named after the skill (`foo.md` or `foo-*.md` for a
skill `foo`) and stores them in the library beside it; install output lists
them. Importing serves nothing by itself: a subagent is loaded into every
Claude Code session, which is more reach than the skill it came with. To serve
a skill's subagents, list the skill in `library/.globals/agents`, one name per
line (`#` starts a comment). While the skill is active and listed, render
links its subagents into `~/.claude/agents`; an active skill that is not
listed is reported as `held`, and any link `skillherd` made to its subagents is
removed.

## Policy and rendering

```text
library/<name>/SKILL.md               a skill
library/<name>/.variants/<harness>/   optional version served to one harness
library/.globals/sets                 activation policy
library/.globals/harnesses            tiers and variant keys per harness
library/.globals/presets/<name>       named bundles for `skillherd add @name`
library/.globals/house                skills you maintain yourself (optional)
library/.globals/agents               skills whose subagents are served (optional)
library/.npx/<name>.json              import record for an upstream skill
```

`.globals/sets` has one row per active skill:

```text
# name   tokens
foo      main
bar      main lane
baz      main only:codex
```

Tokens: `main` and `lane` are the two tiers (a `lane` row must also say
`main`); `only:<harness>` and `not:<harness>` narrow a row; `!mac` and `!linux`
skip a row on macOS or on Linux (`!cell` is a legacy alias). By default `main` is served to Claude Code and
Codex, and `lane` to the shared folder and pi. `skillherd on` and `skillherd off` edit
this file for you (`skillherd on --only H`, `--not H`, `--no-mac`, `--no-linux`; `--no-cell` remains an alias).

Because the shared folder serves everyone who reads it, two combinations are
refused: `only:agents`, and `not:` for a harness that also reads the shared
folder (Codex, Grok and Cursor in the default table).

Rendering makes each harness folder match the policy. A skill is served as a
link into the library, or, when its text holds fenced blocks, as a stripped
copy:

```markdown
<!-- claude-only -->
Only Claude Code sees this.
<!-- /claude-only -->
<!-- mac-only -->
Only macOS sees this.
<!-- /mac-only -->
```

Platform fences are `mac` and `linux`; `cell-only` remains a legacy alias for
`linux-only`. Harness fences are `claude`,
`codex`, `grok`, `pi`, `cursor` and `lane`; a harness block is kept only on a
surface whose variant key (the fourth column of `.globals/harnesses`) has that
name. In the default table those keys are `claude`, `codex`, `pi` and
`cursor`; the shared folder and Grok have none, so they get the text with
every harness block removed.

A harness variant in `.variants/<key>/` (the same variant key) takes
precedence over the plain skill for that harness. Edit skills in the library,
never the served copies; the next render replaces a stale copy it made and
refuses to touch one it did not. A `.variants/<key>` that resolves outside its
skill (a link to another folder, say) is ignored with a warning, and links
inside a skill are followed into a stripped copy only when they stay inside the
skill or the library.

`skillherd strip` applies the same fences to files you name, without touching any
harness folder: it returns the text a surface would serve, or rewrites the
files with `--in-place`:

```bash
skillherd strip --keep mac --harness claude SKILL.md
skillherd strip --keep linux -i docs/*.md
```

## Commands

```text
skillherd list [--harness H | --tier main|lane | --all]
skillherd search <query> [--names]
skillherd info <name>
skillherd on <names…> [--tier main|lane] [--only H] [--not H] [--no-mac] [--no-linux]
skillherd off <names…>
skillherd add <names…|@preset> [--project DIR]
skillherd drop <names…> [--project DIR]
skillherd render [--surface H]
skillherd install <source> [--skill N] [--adopt N] [--keep N]
              [--release-asset ZIP --tag-prefix P [--release-dir DIR]]
skillherd update [names…] [--adopt N] [--keep N]
skillherd remove <names…> [--force]
skillherd reconcile [--adopt N] [--keep N] [--restore N] | --discard N
skillherd doctor [--fix] [--capabilities]
skillherd strip --keep mac|linux [--harness H] [--in-place] <files…>
skillherd sync
skillherd npx <npx arguments…>
skillherd schema
```

- `on`/`off` change what every harness sees; `add`/`drop` link skills into one
  project's `.claude/skills` and `.agents/skills` (experimental).
- `install` imports an upstream skill into the library; `on` activates a skill
  that is already there. `skillherd install owner/repo --release-asset skills.zip
  --tag-prefix v --skill foo` imports a skill shipped as a GitHub release asset
  (experimental).
- `reconcile` finishes or resumes an import: it moves what an interrupted or
  refused `install` or `update` left in its stage into the library, then
  renders. `--discard N` deletes a kept arrival instead. You rarely run it
  yourself.
- `doctor` reports problems; `--fix` applies the safe ones, moving anything it
  removes into `~/.agents/skill-park/` instead of deleting it.

Every command prints a JSON envelope (`--pretty` indents it), except that `on`,
`off`, `render` and `doctor` print a short summary when output goes to a
terminal; `--json` or `--pretty` asks for the envelope there too. A command
`skillherd npx` passes through to the real `npx` keeps that program's own output.
Every command that changes something accepts `--dry-run`, except `sync`.
`skillherd schema` describes the inputs, outputs and error codes.

## What it writes, and how to uninstall

`skillherd` writes only these places:

- links and stripped copies in the harness folders listed above, and links in
  `~/.claude/agents` for the subagents of skills listed in `.globals/agents`.
  A stripped copy carries a `.skill-render.json` file that records how it was
  made;
- with `add --project DIR`: links and copies in `DIR/.claude/skills` and
  `DIR/.agents/skills`;
- the library: `.globals/` policy files, `.npx/` import records and scratch,
  `.skill.lock`, imported skills, and Git commits for imports in a Git library;
- the stage, `~/.agents/.skill-stage/` (or `SKILLHERD_STAGE`): one scratch home
  per `npx` run, removed after the import unless the import was refused or
  interrupted, in which case it stays until `skillherd reconcile` finishes or
  discards it;
- the `skills` lock file (`~/.agents/.skill-lock.json` unless `SKILLHERD_LOCK` or
  `XDG_STATE_HOME` says otherwise), copied from the stage after an `npx` run;
- with `SKILLHERD_SNAPSHOT_LOCK=1`: a copy of that lock in the library,
  `.npx/skill-lock.<platform>.json`;
- `~/.local/state/skillherd/` (under `XDG_STATE_HOME` if set) for markers of an
  in-flight `--keep`;
- `~/.cache/skillherd/npm/` (under `XDG_CACHE_HOME` if set), the `npx` run's npm
  cache, kept between runs;
- `~/.agents/skill-park/` for anything `doctor --fix` moved aside;
- with `strip --in-place`: the files you name, and nothing else.

It does not edit shell configuration or any harness's settings file.

To uninstall:

1. Remove what it serves: move the policy file aside and render an empty
   policy. Every link and copy the tool made is removed; nothing else is.

   ```bash
   mv ~/my-skills/.globals/sets ~/sets.saved
   skillherd --pool ~/my-skills render
   ```

   For each project where you used `add`, run `skillherd drop <names…> --project DIR`
   first.
2. Delete the binary (`~/.local/bin/skillherd`, and `~/.local/bin/npx` if you
   installed the shim), the installer's receipts in `~/.local/share/skillherd/`,
   `~/.config/skillherd/`, `~/.local/state/skillherd/`, `~/.cache/skillherd/`, and, after
   looking inside, `~/.agents/skill-park/` and any `~/.agents/.skill-stage/`
   left by a refused import.
3. Your library is yours: keep it or delete it.

## Network use and telemetry

`skillherd` has no telemetry of its own and makes no network requests itself.
Commands that read or change only local state (`list`, `search`, `info`, `on`,
`off`, `add`, `drop`, `render`, `doctor`) work offline. These commands run
programs that use the network:

- `sync` with a remote runs `git clone` or `git pull`.
- `install`, `update` and `remove` run `npx -y skills@1.7.0`, which downloads
  the package from the npm registry (unless cached) and clones the source
  repository; the import then runs `git clone` on the source for harness
  variants and subagents.
- Release-asset imports call the GitHub API through `gh` or `curl`.

The `npx` run does not inherit your environment, but your proxy, registry and
certificate settings are passed through so it works on a corporate network:
`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY` (upper and lower case),
`npm_config_registry`, `npm_config_proxy`, `npm_config_https_proxy`,
`npm_config_cafile` and `npm_config_strict_ssl` (also in their `NPM_CONFIG_`
spellings), `NODE_EXTRA_CA_CERTS`, `SSL_CERT_FILE`, `SSL_CERT_DIR` and
`GIT_SSL_CAINFO`. Settings in your own `~/.npmrc` are not read unless you set
`SKILLHERD_NPX_CREDENTIALS=1`. Its npm cache is `~/.cache/skillherd/npm`
(`$XDG_CACHE_HOME/skillherd/npm` if set), kept between runs, so a package already
downloaded is not fetched again.

The `skills` package reports installs to its maintainers' telemetry endpoint
unless telemetry is disabled. `skillherd` sets `DISABLE_TELEMETRY=1` and
`DO_NOT_TRACK=1` in that child's environment; set `SKILLHERD_NPX_TELEMETRY=1` to
leave the package's telemetry on.

## Trust boundary

- **A skill is instructions your agents will follow,** including instructions
  to run commands. Read an upstream skill before you turn it on. `install`
  imports without activating, so you can.
- **Updates are not reviewed for you.** `skillherd update` takes whatever the
  upstream branch or newest release holds now, and an active skill serves the
  new text on the next render. In a Git library, `git log -p` on the import
  commit shows what changed.
- **The `npx` run is redirected, not sandboxed.** It runs with a scratch home
  (`HOME` points into the stage), an empty working directory of its own, a
  scrubbed environment (only `PATH`, locale, `TERM`, `TMPDIR` and the network
  settings listed above get through) and an npm configuration the tool
  controls. Its normal output therefore lands in the stage rather than your
  agent folders, and it never picks up a package or `.npmrc` from your current
  project. Git does not prompt for a password (`GIT_TERMINAL_PROMPT=0`).

  That is not isolation. It is still upstream code running with your user's
  permissions: it can read and write anywhere you can and use the network.
  System-wide configuration still applies, and some credentials are found
  without the environment: OpenSSH reads keys and config from your real home
  directory (it looks up your account, not `HOME`), and a Git credential
  helper configured system-wide, such as the macOS keychain helper, still
  answers. Only install from sources you trust.

  By default the run gets no SSH agent, no tokens and none of your own Git or
  npm configuration, so a private source may fail to clone. To fetch one, opt
  in for that command with `SKILLHERD_NPX_CREDENTIALS=1`: it passes
  `SSH_AUTH_SOCK`, `GH_TOKEN`, `GITHUB_TOKEN`, `NPM_TOKEN` and
  `NODE_AUTH_TOKEN`, and points Git at your `~/.gitconfig` and npm at your
  `~/.npmrc`.

  The `git clone` that `skillherd` itself makes afterwards, to import harness
  variants and subagents, is an ordinary clone with your normal environment
  and Git configuration; it runs no upstream code.
- **A shared library is trusted content.** Whoever can push to your library
  decides which skills your agents see and what they say. They cannot decide
  where on your disk `skillherd` writes, and policy files that are symbolic links
  are refused.
- **Subagents are served only when you ask.** They are agent definitions, with
  the same trust questions as skills.

## Troubleshooting and limits

- If a skill is missing, run `skillherd info foo`, `skillherd list --harness claude`
  and `skillherd doctor`, and check that you selected the library and tier you
  meant.
- If rendering reports a foreign entry, inspect it, then move it or remove it
  yourself. The tool will not overwrite it.
- If Git sync refuses a pull, resolve the library's local changes or divergent
  history yourself and run `skillherd sync` again.
- If an import refuses local edits, compare the copies first. `--adopt N`
  takes the incoming copy; `--keep N` keeps yours and stops tracking it
  upstream. An interrupted replacement may need `skillherd reconcile --restore N`.
  The refused download stays in the stage until one of those, or
  `skillherd reconcile --discard N`, deals with it.
- If a private source fails to authenticate, rerun the command with
  `SKILLHERD_NPX_CREDENTIALS=1`.
- `skillherd update` does not update skills imported from a local path.
- On a terminal, the `npx` run's output streams to stderr while it runs.
  Errors from a failed run carry the end of its output, with known tokens and
  URL passwords redacted; rerun `npx skills` by hand in a scratch folder to see
  all of it.

## Glossary

- **Library** (also "pool", as in `--pool`): the folder or Git repository that
  holds your skills and policy. The only source of truth.
- **Harness**: a coding agent that reads skills from a folder (Claude Code,
  Codex, pi, Grok, Cursor), plus the shared `~/.agents/skills` folder.
- **Surface**: a folder the tool serves skills into.
- **Tier**: `main` or `lane`, the two groups the policy assigns skills to; the
  harness table says which tier each harness gets.
- **Policy**: `.globals/sets`, the list of active skills.
- **Render**: make every surface match the policy.
- **Managed entry**: a link or copy the tool can prove it made: a link into the
  library, or a folder carrying a valid `.skill-render.json`. Anything else is a
  **foreign entry** and is never changed.
- **Variant**: a per-harness version of a skill, in `.variants/<harness>/`.
- **Fence**: a `<!-- name-only -->` block kept for one harness or platform.
- **Stage**: the scratch home one `npx` run gets, under
  `~/.agents/.skill-stage/`. The only place imports are read from; the shared
  `~/.agents/skills` folder is a surface and is never imported from.
- **Import record**: `.npx/<name>.json`, where an upstream skill came from and
  the hash it had when imported, used to detect local edits.
- **Reconcile**: move skills from the stage into the library.
- **House**: `.globals/house`, the names of skills you maintain yourself, so
  `doctor` does not ask where they came from.
- **Park**: `~/.agents/skill-park/`, where `doctor --fix` moves things instead
  of deleting them.

## Development

See [AGENTS.md](AGENTS.md) for the rules a change must keep and how to test it,
and [ARCHITECTURE.md](ARCHITECTURE.md) for how the tool works. In short:

```bash
tests/skill-gate.sh                 # fmt, clippy, tests, release build
SKIP_GATE=1 tests/acceptance.sh     # end-to-end in a scratch HOME
```

Continuous integration runs the gate on macOS and Linux.

Most of this code was written by AI coding agents working under the author's
direction and review. The test suite, not the authorship, is the evidence that
it behaves as described.

## License

MIT. See [LICENSE](LICENSE). Third-party licenses for the crates compiled into
the binary are in [THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES).
