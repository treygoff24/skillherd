# Installing and setting up skillherd: a guide for AI agents

You are a coding agent (Claude Code, Codex, pi or another) and your user
asked you to install or set up `skillherd`. This guide takes you from
nothing to a verified install with one skill served, and says which choices
are the user's to make.

`skillherd` keeps one library of agent skills (a folder, optionally a Git
repository) and serves chosen skills into each harness's skill folder:
`~/.claude/skills`, `~/.codex/skills`, the shared `~/.agents/skills` and
`~/.pi/agent/skills`. The policy file `<library>/.globals/sets` says which
skill goes where. The tool changes only links and copies it can prove it
made; anything else in those folders is reported and left alone.

The work is done when every step below has met its "Done when" line and you
have given the user the report in step 8.

## Rules for this whole task

`skillherd` never deletes the library's only copy of a skill, never changes
or removes an entry it did not create, never edits shell configuration or a
harness's settings file, never pushes a Git library, and has no telemetry.
Keep the same discipline on the user's behalf. **Ask the user before you:**

- install or upgrade system software (Rust, a C toolchain, Node.js);
- edit a shell startup file (`~/.zshrc`, `~/.bashrc`, `~/.profile`, …);
- move, rename or delete anything in a harness skill folder, or any file the
  tool reports as foreign (`foreign_entry`, `foreign_shadow`);
- import or update an upstream skill (`skillherd install`, `update`, `npx
  skills …`): it runs third-party code and puts new instructions in front of
  every agent;
- turn on any skill, including the one this repository ships;
- pass `--adopt`, `--keep` or `--force`, run `skillherd reconcile --discard`
  or `skillherd doctor --fix`;
- set `SKILLHERD_NPX_CREDENTIALS=1`, which hands the user's tokens and SSH
  agent to the `npx` run;
- create, commit to or push a Git repository.

Preview every `skillherd` command that changes something with `--dry-run`
first. Read every result: commands print a JSON envelope with `"ok"`, and on
failure `error.code` and `error.suggested_fix`.

## 1. Check the prerequisites

```bash
uname -s                  # Darwin or Linux; anything else is unsupported, stop here
rustc --version           # 1.88 or newer
cargo --version
cc --version              # a C compiler and linker
git --version
bash --version
python3 --version         # this guide uses Python 3 to read JSON; the CLI itself does not need it
node --version; npx --version   # optional: only for importing upstream skills
```

If `rustc` is missing or older than 1.88, the usual fix is rustup
(<https://rustup.rs>); on macOS a missing `cc` comes from
`xcode-select --install`. Both change the user's system: ask first.

**Done when** each required command prints a version that meets the bar, or
the user has declined to install what is missing (then stop and say why).

## 2. Look before you change anything

```bash
command -v skillherd
ls -la ~/.claude/skills ~/.codex/skills ~/.agents/skills ~/.pi/agent/skills 2>/dev/null
ls -la ~/.config/skillherd ~/.agents/skill-library 2>/dev/null
printf 'CLAUDE_CONFIG_DIR=%s CODEX_HOME=%s\n' "${CLAUDE_CONFIG_DIR-}" "${CODEX_HOME-}"
```

Note which skills already sit in each harness folder and whether each is a
real folder or a link. `skillherd` will leave all of them where they are. If
`CLAUDE_CONFIG_DIR` or `CODEX_HOME` is set, `skillherd` serves into those
roots instead of `~/.claude` and `~/.codex`; run it from an environment that
has the same values as the user's agents.

**Done when** you can list, per harness folder, what is already there.

## 3. Install

There is no package-registry release. Build from the GitHub repository:

```bash
git clone https://github.com/treygoff24/skillherd ~/src/skillherd
cd ~/src/skillherd
bash install.sh
export PATH="$HOME/.local/bin:$PATH"
```

Pick another clone location if the user keeps source code elsewhere; keep
the clone, since upgrades come from it. `install.sh` builds a release binary
and copies it to
`~/.local/bin/skillherd`. It does not edit shell files, create a library or
install the `npx` shim. It refuses to overwrite a `skillherd` it did not
install, and refuses while another `skillherd` is on `PATH`; report either
refusal to the user rather than passing `--allow-name-collision`.

The `export` covers your current shell only. If `~/.local/bin` is not on the
user's `PATH` already, tell them and ask before adding it to a startup file.

Without a clone, `cargo install --locked --git
https://github.com/treygoff24/skillherd skillherd` installs into
`~/.cargo/bin` instead; it does not install the optional `npx` shim.

**Done when** `bash install.sh` ends with a line `put  …/skillherd` or
`same …/skillherd` and exits 0.

## 4. Verify the install

```bash
command -v skillherd            # prints ~/.local/bin/skillherd (expanded)
skillherd --version             # prints: skillherd 0.1.1
skillherd doctor --json         # prints {"ok":true,...}
bash ~/src/skillherd/install.sh --verify   # prints "same …/skillherd", exit 0
```

`doctor` exits 0 when it found no warnings and 1 when warnings remain; on a
fresh machine expect 0. Informational findings (`"severity":"info"`) do not
count. The `meta.paths` object in any envelope shows every location the tool
resolved: check that the harness folders are the ones the user's agents
read.

**Done when** all four commands give the output shown.

## 5. Ask the user these questions

Ask them together, with the default you recommend. Take the safe default
for anything the user does not answer.

| Decision | Options | Safe default |
| --- | --- | --- |
| Where the library lives | the default folder `~/.agents/skill-library`; another folder; a Git repository they already have (clone URL) | the default folder |
| Which skills to serve | names in the library, or skills to write or import | none until named |
| Which harnesses get each skill | `main` tier: Claude Code and Codex. `lane` tier: those plus `~/.agents/skills` and pi (Grok and Cursor read `~/.agents/skills`). Or one harness with `--only`, or all but one with `--not` | `main` |
| Which harnesses this machine serves at all | the built-in table (Claude Code, Codex, the shared folder, pi); or stop serving one, such as pi | the built-in table |
| Skills already in harness folders | leave them where they are; or move a hand-made one into the library | leave them |
| The optional `npx` shim | install it, so a pasted `npx skills add …` goes into the library instead of the agent folders; or not | not installed |
| Upstream imports | which sources from skills.sh or GitHub, if any | none |
| Turning on the skill this repository ships (`skills/skillherd`), which teaches agents to use `skillherd` | yes or no | ask; recommend yes |
| Subagents that come with imported skills | serve them, or not | not served |
| Adding `~/.local/bin` to `PATH` in a startup file | yes or no | no; tell them the line |

You may decide these yourself, and tell the user what you chose:

- the clone location and the default install prefix `~/.local`;
- writing `~/.config/skillherd/config.json` to record the library the user
  chose;
- running read-only commands (`list`, `info`, `search`, `doctor`) and any
  command with `--dry-run`;
- adding skills the user wrote themselves to `<library>/.globals/house`;
- leaving `npx` telemetry and lock snapshots off (the defaults).

**Done when** every row of the table has an answer or its default.

## 6. Configure

Apply only the choices the user made. Resolve the library path once before
using `$LIB` in this guide; the JSON parser needs Python 3:

```bash
LIB="$(skillherd list --all --json | python3 -c 'import json, sys; print(json.load(sys.stdin)["meta"]["paths"]["library"])')"
test -n "$LIB" && printf 'Library: %s\n' "$LIB"
```

After changing the library configuration, run those two commands again.
Keep existing config keys when editing; the examples below are complete
files only for a fresh install.

**Another library folder.** Merge this key into the existing configuration, preserving other settings. On a fresh install, create the file:

```bash
mkdir -p ~/.config/skillherd
printf '{ "library": "~/my-skills" }\n' > ~/.config/skillherd/config.json
```

**A Git library** (the repository already exists; you are not creating one). Merge these keys into an existing config; use this complete file only when none exists:

```bash
mkdir -p ~/.config/skillherd
cat > ~/.config/skillherd/config.json <<'EOF'
{
  "library": "~/my-skills",
  "remote": "git@github.com:USER/skill-library.git"
}
EOF
skillherd sync
```

`sync` clones when the folder does not exist yet, and later pulls
fast-forward only. It refuses if the folder exists but is not a clone of that
remote. Skills must sit at the top level of the repository.

**Stop serving a harness** (pi in this example): edit the existing harness
table, preserving its other customizations. For a new table, copy the whole
built-in table below and change pi to `none`. The file replaces the built-in
one. The `LIB` commands below use Python 3 to parse the JSON path safely.

```bash
LIB="$(skillherd list --all --json | python3 -c 'import json, sys; print(json.load(sys.stdin)["meta"]["paths"]["library"])')"
mkdir -p "$LIB/.globals"
cat > "$LIB/.globals/harnesses" <<'EOF'
# harness  tier  surface  variant-key  reads
claude     main  claude   claude       surface
codex      main  codex    codex        surface,agents_dir
agents     lane  agents   -            surface
grok       none  grok     -            surface,agents_dir
pi         none  pi       pi           surface
cursor     none  cursor   cursor       surface,agents_dir
EOF
skillherd --dry-run render
skillherd render
```

**The `npx` shim:** rerun the installer with the flag. The shim only takes
effect where `~/.local/bin` comes before the real `npx` on `PATH`.

```bash
bash ~/src/skillherd/install.sh --with-npx-shim
```

**Move a hand-made skill into the library** (only with the user's yes, and
only for a real folder, not a link). Run these one at a time; if the
destination exists or the move fails, stop and compare the two copies before
activating anything:

```bash
mkdir -p "$LIB"
test ! -e "$LIB/NAME" && test ! -L "$LIB/NAME" && mv ~/.claude/skills/NAME "$LIB/NAME"
skillherd on NAME
echo NAME >> "$LIB/.globals/house"
```

A skill that `npx skills add -g` installed is a link or lives in
`~/.agents/skills`. For those, use `npx skills remove NAME -g`, then
`skillherd install SOURCE --skill NAME` and `skillherd on NAME` (all three
need the user's yes).

**Done when** `skillherd doctor` reports no warning you did not expect, and
`meta.paths.library` is the library the user chose.

## 7. First real run

Serve one skill and watch it arrive. If the user agreed to turn on the skill
this repository ships, copy it into the library.

If the destination already existed, inspect its `SKILL.md` before the
activation commands. Preserve local edits and merge the new instructions
into that folder; copying a directory over it nests another directory and
leaves the old skill active. If replacement is wanted, move the old folder
to an unused backup path first, then copy the new one into the now-vacant
path. Keep the backup until the new skill is verified.


```bash
LIB="$(skillherd list --all --json | python3 -c 'import json, sys; print(json.load(sys.stdin)["meta"]["paths"]["library"])')"
mkdir -p "$LIB"
if [ -e "$LIB/skillherd" ] || [ -L "$LIB/skillherd" ]; then
  printf '%s\n' 'Existing skill: compare with ~/src/skillherd/skills/skillherd before updating.'
else
  cp -R ~/src/skillherd/skills/skillherd "$LIB/skillherd"
fi
skillherd --dry-run on skillherd
skillherd on skillherd
ls -l ~/.claude/skills/skillherd ~/.codex/skills/skillherd
skillherd info skillherd
```

The `ls` shows two links into the library, and `info` shows
`"visible_in": ["claude", "codex"]`. This copy does not follow upstream; after
upgrading the clone, compare and merge or replace it using that same backup procedure. Once `npx` is available and the user
agrees to an upstream import, `skillherd install treygoff24/skillherd --skill
skillherd` tracks it instead, so `skillherd update` refreshes it.

If the user named a different skill, use it instead: write
`$LIB/NAME/SKILL.md` (frontmatter `name` and `description`, then the
instructions), or import it with `skillherd install SOURCE --skill NAME`, and
read the imported `SKILL.md` to the user before `skillherd on NAME`.

**Done when** `skillherd list --harness claude` lists the skill and the links
exist.

## 8. Report to the user

Tell them, in plain words:

- where `skillherd` is installed and where the clone is;
- where the library is, and whether it is a folder or a Git clone;
- which skills are on, and which harnesses see each (`skillherd list --all`);
- what was already in their harness folders and was left alone;
- each choice you took the default for;
- how to add a skill (`skillherd on NAME`), remove one (`skillherd off
  NAME`) and check health (`skillherd doctor`);
- how to undo everything (step 9).

## 9. Upgrade and uninstall

Upgrade:

```bash
git -C ~/src/skillherd pull --ff-only
bash ~/src/skillherd/install.sh
```

Uninstall, with the user's yes. Save the active policy, then render without it
to remove managed links and copies. If the backup name exists, choose another
unused name before proceeding; if no policy exists, skip the move. Run the
commands one at a time and stop on any failure:

```bash
LIB="$(skillherd list --all --json | python3 -c 'import json, sys; print(json.load(sys.stdin)["meta"]["paths"]["library"])')"
test ! -e "$HOME/skillherd-sets.saved" && mv "$LIB/.globals/sets" "$HOME/skillherd-sets.saved"
skillherd render
rm -f ~/.local/bin/skillherd
rm -rf ~/.local/share/skillherd ~/.config/skillherd ~/.local/state/skillherd ~/.cache/skillherd
```

Remove `~/.local/bin/npx` too if the shim was installed: check that its
second line is `# skillherd-npx-shim: 1` first. For each project where
`skillherd add` was used, run `skillherd drop NAMES --project DIR` before the
`render`. Show the user `~/.agents/skill-park/` and any
`~/.agents/.skill-stage/` before deleting them. The library is the user's:
keep it unless they say otherwise.
