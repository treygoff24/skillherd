# skillherd reference

The details the [README](../README.md) leaves out: every setting, the files
in a library, per-harness text, subagents, where the tool writes, its network
behaviour, and its output contract. For how the tool works inside, see
[ARCHITECTURE.md](../ARCHITECTURE.md).

## Configuration

Settings come from, in order of precedence: the global command-line options,
environment variables, then the configuration file
`~/.config/skillherd/config.json` (`$XDG_CONFIG_HOME/skillherd/config.json`
if `XDG_CONFIG_HOME` is set; `SKILLHERD_CONFIG` names another file). The file
is optional; an unknown key in it is an error.

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

Claude Code's root has its own order: `CLAUDE_SHARED`, then `claude_home` in
the file, then `CLAUDE_CONFIG_DIR` (the variable Claude Code itself reads),
then `~/.claude`. Skills are served into `<root>/skills` (for the Codex and
Grok roots too), and Claude Code subagents into `<Claude Code root>/agents`.

Paths in the JSON file may start with `~/` and are otherwise relative to your
home folder; paths from options and environment are relative to the current
folder.

Less common environment variables:

| Variable | Meaning | Default |
| --- | --- | --- |
| `SKILLHERD_STAGE` | Folder that holds the per-run scratch homes for `npx`; must be inside your home folder | `~/.agents/.skill-stage` |
| `SKILLHERD_LOCK` | The `skills` lock file | `$XDG_STATE_HOME/skillherd/.skill-lock.json` if `XDG_STATE_HOME` is set, else `~/.agents/.skill-lock.json` |
| `SKILLHERD_NPX` | The real `npx` to run | the first `npx` on `PATH` that is not the shim |
| `SKILLHERD_NPX_PKG` | The `skills` package to run | `skills@1.7.0` |
| `SKILLHERD_NPX_CREDENTIALS` | `1` passes your credentials to the `npx` run (see [Network](#network-and-telemetry)) | off |
| `SKILLHERD_NPX_TELEMETRY` | `1` leaves the `skills` package's telemetry on | off |
| `SKILLHERD_SNAPSHOT_LOCK` | `1` keeps a copy of the lock file in the library, `.npx/skill-lock.<platform>.json`, committed after each import | off |
| `SKILLHERD_GH` | The `gh` binary for release imports | `gh` on `PATH` |
| `SKILLHERD_PLATFORM` | Platform for platform markers: `mac` or `linux` | the running platform |
| `XDG_CACHE_HOME` | If absolute, parent of the `npx` run's npm cache, `skillherd/npm` | `~/.cache` |
| `XDG_STATE_HOME` | If absolute, parent of the tool's own state folder, `skillherd` | `~/.local/state` |

The `npx` shim reads two more: `SKILLHERD_REAL_NPX` names the real `npx` it
hands other commands to (default: the first `npx` on `PATH` that is not a copy
of the shim), and `SKILLHERD_BIN` names the `skillherd` it calls (default:
`skillherd` on `PATH`).

A custom `sync_command` replaces the built-in `sync`, rendering included. It
receives `SKILLHERD_LIBRARY` and, when set, `SKILLHERD_POOL_REMOTE` in its
environment. It runs as you; configure only a command you trust.

## The library

```text
library/<name>/SKILL.md                 a skill
library/<name>/.variants/<key>/         optional version served to one harness
library/<name>/.harness-agents/claude/  subagents imported with an upstream skill
library/.globals/sets                   activation policy
library/.globals/harnesses              the harness table (optional)
library/.globals/presets/<name>         a named bundle for `skillherd add @name`
library/.globals/house                  skills you maintain yourself (optional)
library/.globals/agents                 skills whose subagents are served (optional)
library/.npx/<name>.json                import record for an upstream skill
library/.skill.lock                     the tool's lock file; keep it out of Git
```

Skill names are one path component: a letter or digit, then up to 63
letters, digits, `.`, `_` or `-`. Files under `.globals/` are plain text, one
entry per line, and `#` starts a comment. A policy file that is a symbolic
link is refused.

### `.globals/sets`

One row per active skill. `skillherd on` and `skillherd off` edit it for you.

```text
# name   tokens
foo      main
bar      main lane
baz      main only:codex
qux      main !linux
```

| Token | Meaning | `skillherd on` flag |
| --- | --- | --- |
| `main` | served to every harness whose tier is `main` | default |
| `lane` | also served to every harness whose tier is `lane`; a `lane` row must also say `main` | `--tier lane` |
| `only:<harness>` | served only on that harness's own folder | `--only H` |
| `not:<harness>` | not served to that harness | `--not H` |
| `!mac`, `!linux` | skipped on that platform | `--no-mac`, `--no-linux` |

Because the shared folder serves everyone who reads it, two combinations are
refused: `only:agents`, and `not:` for a harness that also reads the shared
folder (Codex, Grok and Cursor in the default table). A row that no harness
would see, such as `only:grok` while Grok has no tier, is refused as
`policy_unrepresentable`.

### `.globals/harnesses`

Without this file the tool uses its built-in table. Writing the file replaces
the whole table, so list every harness you want managed. This is the built-in
table written out:

```text
# harness  tier  surface  variant-key  reads
claude     main  claude   claude       surface
codex      main  codex    codex        surface,agents_dir
agents     lane  agents   -            surface
grok       none  grok     -            surface,agents_dir
pi         lane  pi       pi           surface
cursor     none  cursor   cursor       surface,agents_dir
```

- **tier**: `main`, `lane` or `none`. A `none` harness is served nothing, and
  links the tool made there earlier are removed on the next render. To stop
  serving pi, say, set its tier to `none`.
- **surface**: which built-in folder this row manages: `claude`, `codex`,
  `agents`, `grok`, `pi` or `cursor`. It names a destination, never a path,
  so a shared library cannot send writes elsewhere on your disk. Move a
  folder with the local settings in [Configuration](#configuration) instead.
- **variant-key**: the name used for per-harness text and `.variants/`, or
  `-` for none.
- **reads**: the folders this harness loads skills from: `surface`,
  `agents_dir` (the shared folder), both comma-separated, or `-`. It decides
  what `list --harness` and `info` report as visible, and which `not:` rows
  are refused; it serves nothing by itself.

### `.globals/house`, presets

`.globals/house` lists skills you maintain yourself, so `doctor` does not ask
where they came from. `.globals/presets/<name>` lists skill names, one per
line, that `skillherd add @name --project DIR` activates together.

## Per-harness text

A skill is served as a link into the library, or, when its Markdown holds
marker lines, as a stripped copy:

```markdown
<!-- claude-only -->
Only Claude Code sees this.
<!-- /claude-only -->
<!-- linux-only -->
Only Linux sees this.
<!-- /linux-only -->
```

Platform markers are `mac` and `linux`. Harness markers are `claude`, `codex`,
`grok`, `pi`, `cursor` and `lane`; a harness block is kept only on a folder
whose variant key has that name. In the default table those keys are
`claude`, `codex`, `pi` and `cursor`; the shared folder and Grok have none, so
they get the text with every harness block removed. Only these platform and
harness names define markers; any other `<!-- x-only -->` line is ordinary
text, including the retired `cell-only` spelling.

Each marker sits on a line of its own. A platform block and a harness block
may nest one inside the other, one level deep; a second block of the same
kind inside an open one is an error. Markers inside a fenced code block are
ordinary text. A stripped copy carries a `.skill-render.json` file that
records its source, platform, harness and hash, which is how the tool proves
it made the copy.

A whole replacement for one harness goes in `NAME/.variants/<key>/` (the same
variant key) and takes precedence over the plain skill for that harness. A
`.variants/<key>` that resolves outside its skill is ignored with a warning,
and links inside a skill are followed into a stripped copy only when they
stay inside the skill or the library.

Edit skills in the library, never the served copies: the next render
replaces a stale copy it made, and refuses to touch one it did not make.

`skillherd strip` applies the same markers to files you name, without
touching any harness folder. It prints the text a folder would get, or
rewrites the files with `--in-place`:

```bash
skillherd strip --keep mac --harness claude SKILL.md
skillherd strip --keep linux --in-place notes.md
```

## Subagents

Some upstream repositories ship Claude Code subagents beside a skill. An
import brings in the ones named after the skill (`foo.md` or `foo-*.md` for a
skill `foo`) and stores them in the library beside it; the install output
lists them. Importing serves nothing by itself: a subagent is loaded into
every Claude Code session, which is more reach than the skill it came with.

To serve a skill's subagents, add the skill's name to `.globals/agents`. While
the skill is active and listed, rendering links its subagents into
`~/.claude/agents`. An active skill that is not listed is reported as `held`,
and any link the tool made to its subagents is removed.

## What it writes, and where

| What | Default location | Moved by |
| --- | --- | --- |
| Served skills | the harness folders | the root settings above |
| Served subagents | `~/.claude/agents` | the Claude Code root |
| Project skills (`add`) | `DIR/.claude/skills`, `DIR/.agents/skills` | `--project DIR` |
| Policy, import records, imported skills, Git commits for imports | the library | `library`, `--pool` |
| Stage: one scratch home per `npx` run | `~/.agents/.skill-stage/` | `SKILLHERD_STAGE` |
| The `skills` lock file | `~/.agents/.skill-lock.json` | `SKILLHERD_LOCK`, `XDG_STATE_HOME` |
| Lock snapshot (opt-in) | `library/.npx/skill-lock.<platform>.json` | `SKILLHERD_SNAPSHOT_LOCK=1` |
| Tool state (in-flight `--keep` markers) | `~/.local/state/skillherd/` | `XDG_STATE_HOME` |
| npm cache for the `npx` run | `~/.cache/skillherd/npm/` | `XDG_CACHE_HOME` |
| Entries `doctor --fix` moved aside | `~/.agents/skill-park/` | |
| `strip --in-place` | the files you name | |

A stage folder is removed after its import, unless the import was refused or
interrupted; then it stays until `skillherd reconcile` finishes it or
`reconcile --discard` deletes it.

## Network and telemetry

`skillherd` has no telemetry of its own and makes no network requests itself.
`list`, `search`, `info`, `on`, `off`, `add`, `drop`, `render`, `doctor` and
`strip` work offline. These run programs that use the network:

- `sync` with a remote runs `git clone` or `git pull`.
- `install`, `update` and `remove` run `npx -y skills@1.7.0`, which downloads
  the package from the npm registry (unless cached) and clones the source
  repository; the import then runs `git clone` on the source to collect
  harness variants and subagents.
- Release-asset imports call the GitHub API through `gh` or, without it,
  `curl` with `GH_TOKEN` or `GITHUB_TOKEN`. They never go out anonymously.

The `npx` run does not inherit your environment. These network settings are
passed through so it works on a corporate network: `HTTP_PROXY`,
`HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY` (upper and lower case),
`npm_config_registry`, `npm_config_proxy`, `npm_config_https_proxy`,
`npm_config_cafile` and `npm_config_strict_ssl` (also in their `NPM_CONFIG_`
spellings), `NODE_EXTRA_CA_CERTS`, `SSL_CERT_FILE`, `SSL_CERT_DIR` and
`GIT_SSL_CAINFO`. Your `~/.npmrc` is not read unless you set
`SKILLHERD_NPX_CREDENTIALS=1`, which also passes `SSH_AUTH_SOCK`, `GH_TOKEN`,
`GITHUB_TOKEN`, `NPM_TOKEN` and `NODE_AUTH_TOKEN` and your `~/.gitconfig`.
Git never prompts for a password in that run (`GIT_TERMINAL_PROMPT=0`).

The `skills` package reports installs to its maintainers' telemetry endpoint
unless telemetry is disabled. `skillherd` sets `DISABLE_TELEMETRY=1` and
`DO_NOT_TRACK=1` for that run; `SKILLHERD_NPX_TELEMETRY=1` leaves it on.

## Output and exit codes

Every command except `skillherd npx` prints one JSON envelope on stdout:

```json
{"ok": true,  "data": { … }, "meta": { "schema_version": "1.0", "paths": { … }, "warnings": [] }}
{"ok": false, "error": { "code": "input_not_found", "message": "…", "retryable": false, "suggested_fix": "…" }, "meta": { … }}
```

`meta.paths` shows every resolved location (library, stage, lock, each
harness folder), which is the quickest way to check the configuration. `on`,
`off`, `render` and `doctor` print a short summary instead when stdout is a
terminal; `--json` or `--pretty` forces the envelope.

`skillherd schema error` lists every error code with its exit code and
whether a retry can help. The common exit codes: `0` success, `2` usage
error, `65` invalid input or a refused change, `66` something named does not
exist, `75` temporary (the library is busy, or `npx`, Git or the network
failed), `77` GitHub authentication failed, `78` configuration error.

`doctor` has its own: `0` no warnings (informational findings do not count),
`1` warnings remain, `2` some fixes failed, `3` every fix failed, `4` refused
because a fix path is outside your home folder. `skillherd doctor --capabilities` lists every finding code.

`policy_name_missing` means an applicable policy row names a skill absent
from the library. A row that excludes the current platform is skipped; it
can refer to a skill present only on another machine.

## Glossary

- **Library** (also "pool", as in `--pool`): the folder or Git repository that
  holds your skills and policy. The only source of truth.
- **Harness**: a coding agent that reads skills from a folder (Claude Code,
  Codex, pi, Grok, Cursor), plus the shared `~/.agents/skills` folder.
- **Surface**: a folder the tool serves skills into.
- **Tier**: `main` or `lane`, the two groups a policy row can put a skill in;
  the harness table says which tier each harness gets.
- **Policy**: `.globals/sets`, the list of active skills.
- **Render**: make every surface match the policy.
- **Managed entry**: a link or copy the tool can prove it made: a link into the
  library, or a folder carrying a valid `.skill-render.json`. Anything else is
  a **foreign entry** and is never changed.
- **Variant**: a per-harness version of a skill, in `.variants/<key>/`.
- **Stage**: the scratch home one `npx` run gets, under
  `~/.agents/.skill-stage/`. The only place imports are read from; the shared
  `~/.agents/skills` folder is a surface and is never imported from.
- **Import record**: `.npx/<name>.json`, where an upstream skill came from and
  the hash it had when imported, used to detect local edits.
- **Reconcile**: move skills from the stage into the library.
- **House**: `.globals/house`, skills you maintain yourself.
- **Park**: `~/.agents/skill-park/`, where `doctor --fix` moves things instead
  of deleting them.
