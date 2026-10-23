# Architecture

`skillherd` reads a library of skills and a policy, works out which skills each
harness folder ("surface") should hold, and changes the surfaces to match. It
also imports upstream skills into the library. Every change it makes to a
surface or to the library follows three rules, described below: the
managed-entry rule, the replace protocol and the render algorithm. A change
that weakens one of them needs a very good reason and a test that shows the
new behaviour is still safe.

## Map of the crate

All code is in `skill/src/`.

| Module | Role |
| --- | --- |
| `main.rs`, `cli.rs`, `commands/` | Parse arguments, resolve paths, call one command module, print the JSON envelope (`output.rs`) or, for `on`, `off`, `render` and `doctor` on a terminal, a summary (`human.rs`). Commands are thin. `commands/npx.rs` execs the embedded shim script (`skill/scripts/npx`) for `skillherd npx`. |
| `paths.rs`, `config.rs` | Resolve the library, the stage, the lock and every surface from options, environment, the config file and defaults, and refuse overlapping paths. |
| `policy.rs`, `model.rs` | Parse `.globals/sets` and `.globals/harnesses`; compute the set each surface should serve (`effective_set`) and the set each harness can see (`visible_set`). `HARNESS_REGISTRY` is the fixed list of destinations shared policy may select. |
| `managed.rs` | Decide whether an entry on a surface is ours, and re-prove it right before a change. |
| `surfaces.rs`, `render.rs`, `agents.rs`, `project.rs` | Plan and apply the link, copy and removal operations for each surface, for Claude subagents and for one project's folders (`add`, `drop`). |
| `strip.rs` | The fence stripper: pure functions from a skill's source to the bytes a surface serves. Also behind `skillherd strip`. |
| `fsx.rs` | `rename_noreplace` (an atomic rename that fails if the destination exists), atomic writes, no-follow directory walks, and the tool's state folder. |
| `lockfile.rs` | The library lock every command that changes state holds. |
| `hash.rs` | Tree hashes used to detect local edits. |
| `npx.rs` | The stage (per-run scratch homes), the environment the `npx skills` child gets, and the `skills` lock format. |
| `reconcile.rs`, `provenance.rs`, `variants.rs`, `release.rs` | Import from the stage into the library, with import records, harness variants and GitHub release assets. |
| `doctor.rs` | Diagnose, and apply the fixes that are safe and reversible. |
| `library.rs` | Read-only catalog queries for `list`, `search` and `info`. |
| `names.rs`, `error.rs` | Skill-name validation (one safe path component) and the error codes. |

## The managed-entry rule

An entry on a surface is **managed** (ours) only if it is

- a symbolic link whose target, read as text, made absolute against the
  link's own folder, with `..` normalised and nothing resolved, lies inside
  the library; or
- a real folder holding a `.skill-render.json` marker with valid contents,
  which the renderer wrote when it made a stripped copy.

Everything else is **foreign**: reported, never changed, never replaced,
never removed. A link into `~/.agents/skills` is foreign; that folder belongs
to every tool that writes there.

Being managed when the plan was made is not enough. Every operation records
the identity it expects (nothing, a link with a given target, or a managed
folder), and `managed::verify_expected` checks the live entry against it
immediately before the system call. If another process swapped something in
since the plan, the operation is refused and reported, and the rest of the
plan continues. The remaining window is between that check and the call
itself; POSIX offers no "unlink only if the link text is X".

Removals by `doctor --fix` go further: they park the entry under
`~/.agents/skill-park/` (a new link with the same text, then `unlink(2)`,
which cannot remove a folder), so a human can undo them.

## The replace protocol

The library may hold the only copy of a skill, so an import never deletes
before it has a replacement in place:

1. **Stage.** `npx skills` runs with `HOME` set to a fresh, marked folder
   (`run-<stamp>-<pid>/`) under the stage root, which must lie inside the real
   home. What it installs there (`.agents/skills/` in that folder) is the
   inbox. Only run folders carrying this tool's marker are inboxes; in
   particular, `~/.agents/skills` is never swept. The child's environment is
   cleared and rebuilt: `PATH`, locale, `TERM`, `TMPDIR`, the operator's
   proxy, registry and CA variables, an npm configuration inside the run
   folder, a persistent npm cache (`$XDG_CACHE_HOME/skillherd/npm`),
   `GIT_TERMINAL_PROMPT=0`, and telemetry off unless `SKILLHERD_NPX_TELEMETRY=1`.
   Credential variables and the user's `~/.gitconfig` and `~/.npmrc` reach it
   only with `SKILLHERD_NPX_CREDENTIALS=1`. This redirects the child; it does not
   sandbox it.
2. **Preflight.** Under the library lock, before any write: refuse the batch
   if a Git library has staged changes or unrelated changes to import
   metadata, classify each arrival (new, already imported, upstream newer,
   locally edited), and refuse locally edited skills unless the user said
   `--adopt` (take upstream) or `--keep` (keep the local copy).
3. **Copy.** Copy the arrival to scratch inside the library (`.npx/tmp/`),
   carrying over the existing skill's variants and subagents.
4. **Displace.** `rename_noreplace` the live skill to a backup
   (`.npx/backup/`), then `rename_noreplace` the copy into its place. A
   destination that appeared in between is an error, never overwritten. A
   filesystem without no-replace renames is refused, never emulated with a
   check followed by a rename.
5. **Commit.** Write the import record, then commit those exact paths in a
   Git library. In a plain-folder library, the published tree and its record
   are the durable point instead.
6. **Clean up.** Only after the commit, delete the backup this run made, in
   this process, then the inbox copy. A run folder is removed only once its
   inbox is empty. With `SKILLHERD_SNAPSHOT_LOCK=1`, the live lock is then copied
   to `.npx/skill-lock.<platform>.json` and committed.

An arrival the batch refused, or one a crash left behind, is never discarded
implicitly: its run folder stays, a later run's stale-stage cleanup skips any
run folder whose inbox is not empty, and `skillherd reconcile` (with no names)
picks the arrivals up again. A recovered arrival that does not match the
recorded upstream is refused as `stale_arrival` until the user says
`--adopt <name>` or `--discard <name>`; `--discard` deletes only the stage
copy, never the library's. An in-flight `--keep` records which inbox copy it
still has to consume in tool-local state (`$XDG_STATE_HOME/skillherd/reconcile/`),
outside the library.

A backup left by an earlier, interrupted run is never deleted automatically:
nothing in a writable folder can prove who put it there. With no live skill
beside it, the name is refused until `skillherd reconcile --restore <name>`
renames it back; beside a live skill it is reported for a human to remove.
Policy and metadata files are written to a temporary file in the same verified
folder and renamed into place, and a symbolic link where one of them should
be is refused.

## The render algorithm

Under the library lock, for each surface in the harness table:

1. **Desired set.** `effective_set` for that harness: the policy rows whose
   tier the harness gets, filtered by `only:`, `not:` and platform tokens.
2. **Source.** For each desired skill, its source is
   `<library>/<name>/.variants/<variant-key>/` when the harness has a variant
   key and that variant exists, else `<library>/<name>/`.
3. **Form.** If the source's Markdown holds fence markers
   (`<!-- claude-only -->`, `<!-- mac-only -->` and so on), the served entry is
   a stripped copy (`strip::prepare`): platform blocks kept only for the
   render's platform, harness blocks kept only when they name the surface's
   variant key (a surface with none, like the shared folder, keeps no harness
   block), a
   `.skill-render.json` recording the source path, platform, harness, renderer
   version and source hash. Otherwise it is a relative symbolic link to the
   source.
4. **Plan.** Compare desired with what is there. A missing entry is linked or
   copied; a managed entry that is wrong or stale is replaced; a managed entry
   that is not desired is removed; a foreign entry is reported and, if it holds
   a desired name, that name is skipped (`foreign_shadow`).
5. **Apply.** `surfaces::apply_ops` carries out the plan, re-proving each
   entry first. Stripped copies are written by the same step, under the same
   checks as links; there is no second writer.
6. **Agents.** For Claude, the imported subagents of each active skill that
   is also listed in `.globals/agents` are linked into the agents folder
   under a stricter proof: only a link whose target is exactly that skill's
   stored agent file is ours. An active skill with subagents that is not
   listed is reported as `held`, and any link of ours to its subagents is
   removed.

Two consecutive renders with no change in between report every entry
unchanged. `--dry-run` returns the same plan and touches nothing.

Fence rules: a marker is a whole line. Machine fences and harness fences may
nest one level, one inside the other; a second fence of the same kind inside
an open one is an error. Markers inside a fenced code block are ordinary text.
Removing a block closes the gap it leaves without touching blank lines
elsewhere, and a file with no markers is served unchanged.

## Paths and configuration

All write destinations come from this machine: options, environment, the
config file and built-in defaults. The library's `.globals/harnesses` may only
select one of the built-in destinations in `HARNESS_REGISTRY` by name (or by
its exact legacy token, such as `{claude_shared}/skills`); a path, a `~` or any
other expansion is refused. `Paths` checks every surface against the library,
the stage, the lock and the other surfaces, after resolving links, and refuses
overlaps before any plan is made. Claude's root resolves as `CLAUDE_SHARED`,
then the config file's `claude_home`, then `CLAUDE_CONFIG_DIR`, then
`~/.claude`.

## Concurrency

Every command that changes state takes an exclusive `flock` on
`<library>/.skill.lock` before it reads policy or records, and holds it until
it is done. `--dry-run` takes no lock and changes nothing it reports on; a
dry-run import may still make its variant clone in the system temporary
folder to plan variants.
