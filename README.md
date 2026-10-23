# skillherd

**One skill library for every coding agent.** `skillherd` keeps your agent
skills in one folder or Git repository and serves the ones you choose to
Claude Code, Codex, pi and the shared `~/.agents/skills` folder. It changes
only the links and copies it can prove it made, so skills you installed some
other way stay exactly where they are.

Every coding agent reads skills from its own folder. Keeping the same skills
in all of them means copying folders around by hand, and the copies drift.
With `skillherd` you edit a skill once, in the library, and every agent you
chose sees the change.

> **Are you an AI agent setting this up for someone?** Read
> [docs/agent-setup.md](docs/agent-setup.md).

Version 0.1.0, the first public release. macOS and Linux only. Output is JSON,
written for scripts and agents first; `on`, `off`, `render` and `doctor` print
a short summary instead when run in a terminal.

## Quick start

You need Rust 1.88 or newer (with Cargo), a C compiler toolchain and Git.
Build and install from a clone:

```bash
git clone https://github.com/treygoff24/skillherd
cd skillherd
bash install.sh                         # builds skillherd into ~/.local/bin
export PATH="$HOME/.local/bin:$PATH"    # if ~/.local/bin is not on PATH yet
skillherd --version
```

Next, write a skill into the library and turn it on. The library is
`~/.agents/skill-library` unless you configure another, with one folder per
skill:

```bash
mkdir -p ~/.agents/skill-library/review-errors
cat > ~/.agents/skill-library/review-errors/SKILL.md <<'EOF'
---
name: review-errors
description: Review a change for missing error handling.
---
Inspect error paths and report errors that are silently ignored.
EOF

skillherd on review-errors
ls -l ~/.claude/skills ~/.codex/skills
```

`on` adds the line `review-errors main` to the library's policy file,
`.globals/sets`, and then links the skill into Claude Code's and Codex's skill
folders:

```text
~/.claude/skills/review-errors -> ../../.agents/skill-library/review-errors
~/.codex/skills/review-errors  -> ../../.agents/skill-library/review-errors
```

Edit the library copy and both agents see the change. `skillherd off
review-errors` removes the line and both links, and nothing else.

## Is this the right tool?

If all you want is the same skills in every agent, there are lower-friction
options, and you should try them first:

- [`npx skills`](https://skills.sh) (Vercel's `skills` CLI) installs a skill
  into the folders of many agents with one command and keeps a lock file.
  Each install decides which agents get the skill; there is no standing
  policy to re-apply later.
- [skillshare](https://github.com/runkids/skillshare) syncs one library into
  many tools, with per-target include and exclude filters, Git-based sharing,
  a security audit, a desktop app and Windows support. "One library, many
  harnesses" is not new, and skillshare covers more ground than `skillherd`
  does.

`skillherd` is for people who want, in addition:

- A written activation policy: one file in the library says which skills
  each harness sees, with per-harness and per-platform exceptions. Importing a
  skill and turning it on are separate steps, so nothing is served until you
  say so.
- Provable ownership: the tool removes or replaces only entries it can prove
  it created, and checks that proof again immediately before each change.
  Anything else in a skill folder is reported and left alone.
- Per-harness text: a skill can carry a different version for one harness, or
  blocks of text marked for one harness or one platform; each harness gets its
  own copy.
- Contained upstream imports: `skillherd install` runs `npx skills` with a
  scratch home folder and imports the result into your library, so an
  upstream install never scatters copies across your real agent folders.

## How it works

```text
library (folder or Git repo)                 harness folders (output)
  review-errors/SKILL.md                       ~/.claude/skills/review-errors, pdf
  pdf/SKILL.md                  ── render ──>  ~/.codex/skills/review-errors, pdf
  .globals/sets:                               ~/.agents/skills/pdf
    review-errors main                         ~/.pi/agent/skills/pdf
    pdf           main lane
```

The **library** holds one folder per skill and is the only source of truth.
The **policy**, `.globals/sets` in the library, lists the active skills, one
per line. **Rendering** makes every harness folder match the policy: a missing
skill is linked in, a link the tool made that the policy no longer wants is
removed, and anything the tool did not make is left alone. `on`, `off`,
`sync` and the import commands render when they finish; `skillherd render`
does it on demand.

Each policy line puts a skill in a **tier**, and each harness is served one
tier. By default:

| Harness | Folder `skillherd` serves | Tier it gets |
| --- | --- | --- |
| Claude Code | `~/.claude/skills` | `main` |
| Codex | `~/.codex/skills` (Codex also reads `~/.agents/skills`) | `main` |
| Shared folder | `~/.agents/skills` | `lane` |
| pi | `~/.pi/agent/skills` | `lane` |
| Grok | `~/.grok/skills` | none: Grok reads the shared folder |
| Cursor | `~/.cursor/skills` | none: Cursor reads the shared folder |

`skillherd on NAME` puts a skill in `main`: Claude Code and Codex.
`skillherd on NAME --tier lane` puts it in both tiers, so the shared folder
and pi get it too, and through the shared folder Grok and Cursor. Narrow a
skill further with `--only HARNESS`, `--not HARNESS`, `--no-mac` or
`--no-linux`. A rule that no harness would see is refused rather than saved.

Claude Code and Codex behaviour is covered by the test suite against these
folder layouts; whether Grok, Cursor and pi load exactly these folders has not
been verified against those harnesses. A harness may also read other folders;
`skillherd` manages only the ones above. To relocate a folder, change a tier
or stop serving a harness, see [docs/reference.md](docs/reference.md).

## Install

| Requirement | Needed for |
| --- | --- |
| macOS or Linux | everything: other platforms are refused at compile time, because the tool depends on atomic no-replace renames (`renamex_np`, `renameat2`) and will not fall back to a racy check-then-rename |
| Rust 1.88 or newer, Cargo, a C linker | building |
| Bash | `install.sh` |
| Git | Git libraries and upstream imports |
| Node.js and `npx` | `skillherd install`, `update` and `remove` only |
| `gh`, or `GH_TOKEN`/`GITHUB_TOKEN` | GitHub release-asset imports only |

There is no package-registry release; you build from source. `bash
install.sh` (shown in the [quick start](#quick-start)) builds the release
binary and copies it to `~/.local/bin/skillherd`. It does not edit your shell
configuration, create a library or install anything else unless you ask. It
records a checksum under `~/.local/share/skillherd/` so a later run can tell
its own install from someone else's file. It refuses to overwrite a
`skillherd` it did not install, and refuses to install while a `skillherd` it
did not install is anywhere on your `PATH` (`--allow-name-collision` accepts
that risk). To upgrade, `git pull` in the clone and run `bash install.sh`
again.

Other ways to install:

```bash
bash install.sh --prefix "$HOME/tools"            # installs into ~/tools/bin
bash install.sh --verify                          # report drift, change nothing
cargo install --locked --path skill               # into ~/.cargo/bin instead
cargo install --locked --git https://github.com/treygoff24/skillherd skillherd   # no clone kept
```

`--verify` prints `same`, `DIFF`, `MISSING` or `UNWANTED` per file and exits
1 on drift. Builds use `CARGO_TARGET_DIR` if set, otherwise `skill/target` in
the clone; Cargo's caches stay for the next install. `bash install.sh --help`
lists every option.

### The optional `npx` shim

`bash install.sh --with-npx-shim` also installs an `npx` wrapper beside
`skillherd`. With that folder first on `PATH`, a pasted `npx skills add …`,
`npx skills update …` or `npx skills remove …` runs through `skillherd
install`, `update` or `remove`, so it lands in your library instead of your
agent folders. Every other `npx` command, and the read-only `skills` commands,
pass through to the real `npx` unchanged. Unknown `skills` commands,
unsupported flags and package versions other than the pinned `skills@1.7.0`
are refused rather than sent to the real tool against your home folder.

The shim is off by default because it sits in front of every `npx` call on
your machine. Without it, `skillherd npx …` does the same routing for one
command. Everything after `npx` belongs to npx, so put `skillherd`'s own flags
before it:

```bash
skillherd --dry-run npx skills add owner/repo
```

Running `install.sh` again without `--with-npx-shim` removes a shim it
installed earlier and says so. An `npx` it did not install is never touched.

## Using it

### Your library somewhere else

The default library is `~/.agents/skill-library`. To keep it elsewhere, save
the path once:

```bash
mkdir -p ~/.config/skillherd
printf '{ "library": "~/my-skills" }\n' > ~/.config/skillherd/config.json
skillherd list --all
```

`--pool DIR` on any command overrides it for that command. Keep the library
separate from `~/.agents/skills`, which is a place skills are served to, not
a source.

### A Git library

```bash
cat > ~/.config/skillherd/config.json <<'EOF'
{
  "library": "~/my-skills",
  "remote": "https://github.com/example/skill-library.git"
}
EOF
skillherd sync
skillherd on review-errors
```

Replace the example URL with your repository; local paths and SSH URLs work
too, and Git handles authentication. Skills sit at the top level of the
repository (`review-errors/SKILL.md`), not in a subfolder.

When the library folder does not exist yet, `sync` clones it. When it exists,
`sync` requires the configured URL to match `origin` and tracked files to be
clean, runs `git pull --no-rebase --ff-only origin`, and renders. It never
pushes, resolves conflicts or discards local edits. Until the first `sync`
has cloned the library, other commands that would change it refuse to run.

Commit the library's `.globals/` folder; it is your policy. Add `.skill.lock`
(the tool's lock file) to the library's `.gitignore`. Imports commit their own
files with your usual Git identity. On a plain-folder library, `sync` only
renders.

### Skills you write yourself

Write `NAME/SKILL.md` in the library and run `skillherd on NAME`, as in the
quick start. `skillherd doctor` reports a skill it cannot trace to an upstream
source (`unknown_provenance`, informational). Add its name to
`.globals/house` in the library, one name per line, to record that you
maintain it:

```bash
echo review-errors >> ~/.agents/skill-library/.globals/house
```

Edit skills in the library, never the served copies.

This repository ships one skill of its own, `skills/skillherd`, which teaches
an agent to drive `skillherd`. Copy that folder into your library and run
`skillherd on skillherd` to give your agents the instructions.

### Skills from skills.sh and GitHub

| You ran | Now run |
| --- | --- |
| `npx skills add owner/repo --skill foo -g` | `skillherd install owner/repo --skill foo`, then `skillherd on foo` |
| `npx skills update` | `skillherd update` |
| `npx skills remove foo -g` | `skillherd remove foo` |
| `npx skills list` | `skillherd list --all` |

`skillherd install` runs the pinned `skills@1.7.0` package with a scratch
home folder (the **stage**, under `~/.agents/.skill-stage/`), imports what it
installed there into your library with an import record (`.npx/foo.json`, which
remembers the source and a hash for detecting local edits), and copies its
lock file to the usual lock location. It does not activate the skill: read
it, then run `skillherd on foo`.

If you edited an imported skill, `update` refuses to overwrite it. Rerun with
`--adopt foo` to take the upstream copy or `--keep foo` to keep yours and stop
tracking upstream. The download stays in the stage until you decide;
`skillherd reconcile --discard foo` deletes it and never touches the library
copy. `--dry-run` previews any of these.

Skills that `npx skills add -g` already put in `~/.agents/skills`, and the
links it made in `~/.claude/skills`, are not this tool's: `skillherd` leaves
them where they are. If one has the same name as a skill you turn on,
`skillherd doctor` reports it as `foreign_shadow` and the name is skipped in
that folder. To move such a skill under `skillherd`, first remove it with the
real `npx skills remove <name> -g`, then `skillherd install <source> --skill
<name>` and `skillherd on <name>`. Both tools share one lock file, so the
other order would make the removal forget the new install.

`skillherd install owner/repo --release-asset skills.zip --tag-prefix v
--skill foo` imports a skill shipped as a zip on GitHub releases
(experimental). `skillherd update` does not update skills imported from a
local path.

### Per-harness and per-platform text

A skill can hold text for only one harness or platform. Mark it with
whole-line comments:

```markdown
<!-- claude-only -->
Only Claude Code sees this.
<!-- /claude-only -->
<!-- mac-only -->
Only macOS sees this.
<!-- /mac-only -->
```

A skill with markers is served as a stripped copy instead of a link; each
harness gets the text meant for it. A whole replacement version for one
harness goes in `NAME/.variants/<harness>/`. `skillherd strip` applies the
same markers to any file you name, without touching a harness folder. The
details are in [docs/reference.md](docs/reference.md#per-harness-text).

### Project skills (experimental)

`skillherd add NAME --project DIR` links a library skill into one project's
`.claude/skills` and `.agents/skills`; `skillherd drop NAME --project DIR`
removes those links. Without `--project`, the project is the Git root above
the current folder.

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
skillherd schema [all|input|output|error]
```

- `on` and `off` change what every harness sees; `add` and `drop` change one
  project.
- `install` imports an upstream skill; `on` activates a skill already in the
  library.
- `reconcile` finishes an import that was refused or interrupted. You rarely
  run it yourself.
- `doctor` reports problems and exits 1 if any warning remains; `--fix`
  applies the safe ones, moving anything it removes into
  `~/.agents/skill-park/` instead of deleting it.

Every command prints a JSON envelope (`--pretty` indents it), except that
`on`, `off`, `render` and `doctor` print a short summary on a terminal;
`--json` asks for the envelope there too. Every command that changes
something accepts `--dry-run`, except `sync` (use `render --dry-run`).
`skillherd schema` prints the inputs, outputs, error codes and exit codes.

## Configuration

Settings come from, in order of precedence: command-line options, environment
variables, then `~/.config/skillherd/config.json`. The common ones:

| Setting | JSON key | Environment | Option | Default |
| --- | --- | --- | --- | --- |
| Library folder | `library` | `SKILLHERD_LIBRARY` | `--pool DIR` | `~/.agents/skill-library` |
| Library remote | `remote` | `SKILLHERD_POOL_REMOTE` | `--pool-remote URL` | none |
| Claude Code root | `claude_home` | `CLAUDE_SHARED`, `CLAUDE_CONFIG_DIR` | | `~/.claude` |
| Codex root | `codex_home` | `CODEX_HOME` | | `~/.codex` |
| Shared skills folder | `agents_dir` | `SKILLHERD_AGENTS_DIR` | | `~/.agents/skills` |
| pi skills | `pi_skills` | `SKILLHERD_PI_SKILLS` | | `~/.pi/agent/skills` |
| Cursor skills | `cursor_skills` | `SKILLHERD_CURSOR_SKILLS` | | `~/.cursor/skills` |

Every setting, the less common environment variables and the policy file
formats are in [docs/reference.md](docs/reference.md).

Where `skillherd` writes is decided by this machine's configuration only. A
library you clone from someone else cannot point the tool at another folder
on your disk, and every destination is checked against the library and the
other destinations before anything is planned.

## Safety and trust

`skillherd` writes to the harness folders (and `~/.claude/agents` for
subagents you opt into), the library, its scratch and state folders under
`~/.agents`, `~/.local/state/skillherd` and `~/.cache/skillherd`, and, for
`add --project` and `strip --in-place`, the folders and files you name.
[docs/reference.md](docs/reference.md#what-it-writes-and-where) lists every
location. It does not edit shell configuration or any harness's settings
file. It has no telemetry of its own and makes no network requests itself:
`sync` runs Git, the import commands run `npx` and Git, and release-asset
imports call the GitHub API. Everything else works offline.

What you are trusting:

- A skill is instructions your agents will follow, including instructions to
  run commands. Read an upstream skill before you turn it on; `install`
  imports without activating, so you can.
- Updates are not reviewed for you. `skillherd update` takes whatever the
  upstream branch or newest release holds now, and an active skill serves the
  new text on the next render. In a Git library, `git log -p` on the import
  commit shows what changed.
- The `npx` run is redirected, not sandboxed. It gets a scratch home, an
  empty working folder, a scrubbed environment and an npm configuration the
  tool controls, so its output lands in the stage rather than your agent
  folders and it never picks up a package or `.npmrc` from your current
  project. But it is still upstream code running as you: it can read and
  write anywhere you can and use the network, OpenSSH still reads keys from
  your real home folder, and a system-wide Git credential helper (such as the
  macOS keychain) still answers. Only install from sources you trust.
- By default that run gets no SSH agent, no tokens and none of your Git or npm
  configuration, so a private source may fail to clone.
  `SKILLHERD_NPX_CREDENTIALS=1` passes them for one command; the exact list is
  in [docs/reference.md](docs/reference.md#network-and-telemetry).
- A shared library is trusted content. Whoever can push to it decides which
  skills your agents see and what they say. They cannot decide where on your
  disk `skillherd` writes, and policy files that are symbolic links are
  refused.
- Subagents are agent definitions, with the same trust questions as skills,
  and are served only for skills you list in `.globals/agents`.

## Uninstall

1. Remove what it serves: move the policy file aside and render an empty
   policy. Every link and copy the tool made is removed; nothing else is.

   ```bash
   mv ~/.agents/skill-library/.globals/sets ~/skillherd-sets.saved
   skillherd render
   ```

   Use your library's path if it is not the default. For each project where
   you used `add`, run `skillherd drop <names…> --project DIR` first.
2. Delete the binary (`~/.local/bin/skillherd`, and `~/.local/bin/npx` if you
   installed the shim), the install checksums in `~/.local/share/skillherd/`,
   and `~/.config/skillherd/`, `~/.local/state/skillherd/` and
   `~/.cache/skillherd/`. Look inside `~/.agents/skill-park/` and any
   `~/.agents/.skill-stage/` left by a refused import before deleting them.
3. Your library is yours: keep it or delete it.

## Troubleshooting

| Problem | What to do |
| --- | --- |
| A skill is missing from a harness | `skillherd info NAME` shows its tiers and which harnesses see it; `skillherd list --harness claude` and `skillherd doctor` show the rest. Check the library and tier. |
| `foreign_shadow` or a foreign entry | Something the tool did not make holds that name in a harness folder. Inspect it, then move or remove it yourself; the tool will not. |
| `install.sh` refuses a name collision | Another `skillherd` is on your `PATH` or at the destination. Move it, or install with `--prefix` elsewhere. |
| `sync` refuses to pull | Resolve the library's local changes or divergent history yourself, then `skillherd sync` again. |
| `local_edit_refused` on an import | Compare the copies, then rerun with `--adopt NAME` (take upstream) or `--keep NAME` (keep yours). An interrupted replacement may need `skillherd reconcile --restore NAME`. |
| A private source fails to authenticate | Rerun the command with `SKILLHERD_NPX_CREDENTIALS=1`. |
| An `npx` run fails | The error's `details.output_tail` holds the end of its output, with known tokens redacted. On a terminal the output also streams to stderr. |

## Development

[AGENTS.md](AGENTS.md) has the rules every change must keep and how to test
it; [ARCHITECTURE.md](ARCHITECTURE.md) explains how the tool works. In short:

```bash
tests/skill-gate.sh                 # fmt, clippy, every Rust test, release build
SKIP_GATE=1 tests/acceptance.sh     # end to end in a scratch HOME
bash tests/install.test.sh          # the installer
bash tests/shell-check.sh           # bash -n and shellcheck on every script
```

Continuous integration runs these on macOS and Linux. Changes are listed in
[CHANGELOG.md](CHANGELOG.md).

Most of this code was written by AI coding agents working under the author's
direction and review. The test suite, not the authorship, is the evidence that
it behaves as described.

## License

MIT. See [LICENSE](LICENSE). Third-party licenses for the crates compiled into
the binary are in [THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES).
