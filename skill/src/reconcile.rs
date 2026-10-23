//! Reconcile: the npx inbox into the pool (lane T4).
//!
//! The inbox is every real directory carrying a `SKILL.md` in a tool-owned
//! per-run stage. The shared skills surface is never an inbox. One run
//! preflights the whole batch, then per name copies the arrival into the
//! library, imports its upstream harness variants, writes the import record,
//! commits and only then deletes the inbox copy — so every interruption point
//! retries cleanly and converges on one import record and one commit.
//!
//! Order, per the plan: lock → inbox scan → recover scratch → preflight →
//! per-name import → variants → record + `.gitignore` + commit → delete the
//! inbox copy → lock snapshot → render.
//!
//! # Crash safety
//!
//! Replacing a pool entry is an exchange, never a deletion:
//!
//! 1. the complete replacement is built in `.npx/tmp/<n>`, variants carried
//!    into it there;
//! 2. the live entry is renamed to `.npx/backup/<n>`;
//! 3. the replacement is renamed into place;
//! 4. the backup is dropped only after the import commit has landed.
//!
//! The pool therefore always holds a complete copy, and the next run reads the
//! scratch to decide what to do. A run deletes under `.npx/backup` only the
//! copy it displaced itself, in this process, after the import commit landed.
//! Anything at `backup/<n>` that an earlier run left is never deleted, moved,
//! or cleared out of the way: with no live entry it is the pool's only copy,
//! reported (`stage_displaced`) and refused until `--restore <n>`; beside a
//! live entry equal to the arrival it is the landed publish's leftover, so the
//! record and the commit are finished and the backup is reported
//! (`stale_backup`) for a human to compare and remove; beside anything else
//! the name is refused (`stale_backup_refused`) until the human has cleared
//! it. `tmp/<n>` is an incomplete copy of an inbox entry and is redone.
//!
//! `--keep` follows the same rule: a marker in tool-local state outside the
//! library records which owned stage inbox copy it still has to consume. Its
//! target must be exactly an owned inbox plus a validated skill name; the
//! staged `npx remove` and metadata commit come first, the inbox copy last.
//!
//! # Interruption hook
//!
//! [`failpoint`] names stop a run dead at a documented step when
//! `SKILLHERD_RECONCILE_FAILPOINT` carries that name. It is a test hook for the
//! crash tests in `tests/reconcile.rs`, and it exists only in debug builds: a
//! release binary compiles it out, so the acceptance run cannot see it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::fsx::rename_noreplace;
use crate::hash::committed_tree_hash;
use crate::lockfile::{MutatorGuard, lock_library};
use crate::managed::has_marker;
use crate::model::{Imported, NpxMeta, PlanOp, Refusal};
use crate::names::validate_name;
use crate::npx::{self, LockEntry, Verb, read_dir_names};
use crate::output;
use crate::paths::{Paths, lexical_abs};
use crate::render;
use crate::render::RenderReport;
use crate::variants;

/// What to import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReconcileOpts {
    pub dry_run: bool,
    /// Names whose local edits to overwrite with upstream.
    pub adopt: Vec<String>,
    /// Names to keep as house skills, dropping their npx record.
    pub keep: Vec<String>,
    /// Names whose displaced copy in `.npx/backup` to put back at the live
    /// path (only when nothing is there). Never done without being asked.
    #[serde(default)]
    pub restore: Vec<String>,
    /// Owned stage inbox roots; empty discovers recoverable per-run stages.
    pub inbox: Vec<PathBuf>,
    /// Import records for arrivals this run fetched itself from a GitHub
    /// release (`release::fetch_all`), which the npx lock knows nothing about:
    /// they supply the source fields and the revision in its place.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fetched: BTreeMap<String, NpxMeta>,
}

/// The steps a test can stop a run at, in the order a replacement reaches them.
pub mod failpoint {
    pub const AFTER_COPY_TO_TMP: &str = "after-copy-to-tmp";
    pub const AFTER_VARIANT_TRANSFER: &str = "after-variant-transfer";
    pub const AFTER_DISPLACE: &str = "after-displace";
    pub const AFTER_PUBLISH: &str = "after-publish";
    pub const AFTER_RECORD_WRITE: &str = "after-record-write";
    pub const AFTER_GIT_ADD: &str = "after-git-add";
    pub const AFTER_COMMIT: &str = "after-commit";
    pub const BEFORE_INBOX_DELETE: &str = "before-inbox-delete";
    pub const AFTER_KEEP_NPX: &str = "after-keep-npx";
}

/// Stop here when `SKILLHERD_RECONCILE_FAILPOINT` names this step.
///
/// A test hook, not a feature: a release binary has no failpoints at all.
fn failpoint(name: &str) -> Result<()> {
    #[cfg(debug_assertions)]
    if std::env::var("SKILLHERD_RECONCILE_FAILPOINT").is_ok_and(|wanted| wanted == name) {
        return Err(Error::internal(format!(
            "failpoint {name}: a test stopped the run here on purpose"
        )));
    }
    #[cfg(not(debug_assertions))]
    let _ = name;
    Ok(())
}

/// The scratch tree one replacement is built in, before it is published.
fn staging_dir(paths: &Paths, name: &str) -> PathBuf {
    paths.library.join(".npx/tmp").join(name)
}

/// The displaced live entry, kept until the import commit has landed.
fn backup_dir(paths: &Paths, name: &str) -> PathBuf {
    paths.library.join(".npx/backup").join(name)
}

/// The scratch this tool recovers from must be its own real directories: a
/// symlink at `.npx`, `.npx/tmp` or `.npx/backup` (or the marker dirs under
/// them) would make every scratch rename land somewhere the tool did not
/// choose, so the run refuses before it reads any of it.
fn scratch_is_sound(paths: &Paths) -> Result<()> {
    for rel in ["", ".npx", ".npx/tmp", ".npx/tmp/keep", ".npx/backup"] {
        let path = paths.library.join(rel);
        match crate::fsx::open_dir_nofollow(&path) {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(Error::config_error(format!(
                    "{} must be a real directory with no symlink parents: {err}",
                    path.display()
                )));
            }
        }
        if rel.is_empty() {
            continue; // The operator-chosen library root may itself be a link.
        }
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            return Err(Error::config_error(format!(
                "{} is a symlink; the scratch under .npx must be real directories this tool created — remove the link and rerun",
                path.display()
            )));
        }
        if !meta.is_dir() {
            return Err(Error::config_error(format!(
                "{} is not a directory; the scratch under .npx must be real directories this tool created — move it aside and rerun",
                path.display()
            )));
        }
    }
    Ok(())
}

/// `--keep` marks a name here while its transition is in flight; the file body
/// is the inbox copy the keep still has to consume.
fn keep_dir(paths: &Paths) -> PathBuf {
    let library = lexical_abs(&paths.home, &paths.library);
    let key = crate::hash::sha256_hex(library.as_os_str().as_encoded_bytes());
    crate::fsx::state_dir(paths)
        .join("reconcile")
        .join(key)
        .join("keep")
}

fn keep_mark(paths: &Paths, name: &str) -> PathBuf {
    keep_dir(paths).join(name)
}

/// A real directory: a symlink is not one, however it resolves.
fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// The valid names this tool owns inside `dir` (`.npx/tmp` also holds scratch
/// files and clone directories, which are not names).
fn owned_names(dir: &Path) -> Result<Vec<String>> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::io(&format!("reading {}", dir.display()), &err)),
    };
    let mut names = Vec::new();
    for dirent in read {
        let dirent = dirent.map_err(|err| Error::io("reading a scratch entry", &err))?;
        let name = dirent.file_name().to_string_lossy().into_owned();
        if validate_name(&name).is_ok() {
            names.push(name);
        }
    }
    Ok(names)
}

/// The batch outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReconcileReport {
    pub imported: Vec<Imported>,
    pub refused: Vec<Refusal>,
    pub kept: Vec<String>,
    pub variants_pending: Vec<String>,
    pub render: Option<RenderReport>,
    pub plan: Vec<PlanOp>,
}

/// Explicitly discard arrivals in owned stages, never the installed copy.
/// Separate from import: even a dry run must not import a name it discards.
pub fn discard(paths: &Paths, names: &[String], dry_run: bool) -> Result<ReconcileReport> {
    for name in names {
        validate_name(name)?;
    }
    let _guard = if dry_run {
        None
    } else {
        Some(lock_library(paths)?)
    };
    let mut plan = Vec::new();
    for name in names {
        let mut found = false;
        for root in npx::recoverable_inboxes(paths)? {
            let path = root.join(name);
            if fs::symlink_metadata(&path).is_err() {
                continue;
            }
            crate::fsx::open_dir_nofollow(&path)
                .map_err(|err| Error::io("checking discard target", &err))?;
            found = true;
            plan.push(PlanOp {
                op: "delete".into(),
                path: path.clone(),
                target: None,
            });
        }
        if !found {
            return Err(Error::usage(format!(
                "--discard {name}: no retained stage arrival has that name"
            )));
        }
    }
    if !dry_run {
        for operation in &plan {
            npx::remove_path(&operation.path)?;
        }
        npx::clear_consumed_stages(paths)?;
    }
    Ok(ReconcileReport {
        imported: Vec::new(),
        refused: Vec::new(),
        kept: Vec::new(),
        variants_pending: Vec::new(),
        render: None,
        plan,
    })
}

/// Import the inbox and render.
pub fn run(paths: &Paths, opts: ReconcileOpts) -> Result<ReconcileReport> {
    Batch::new(paths, opts).execute(None)
}

/// Import the inbox while the caller already holds the library lock: the staged
/// `install`/`update` flow holds it across its npx run.
pub fn run_locked(
    paths: &Paths,
    opts: ReconcileOpts,
    guard: MutatorGuard,
) -> Result<ReconcileReport> {
    Batch::new(paths, opts).execute(Some(guard))
}

/// How one inbox name stands against the pool.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Class {
    /// No library entry at all.
    New,
    /// The library entry already holds exactly the inbox bytes.
    AlreadyImported,
    /// The library entry is the recorded import and the inbox is newer.
    UpstreamNewer,
    /// The library entry carries edits, or no record: never overwritten.
    Conflict(String),
    /// No live entry, but `.npx/backup/<n>` holds the copy an interrupted run
    /// displaced. Refused until `--restore <n>` puts it back; neither
    /// `--adopt` nor `--keep` applies.
    Displaced(String),
    /// Something an earlier run (or somebody) left at `.npx/backup/<n>`
    /// beside a live entry that is not the arrival. This tool never clears
    /// that path to make room: refused until a human has compared and removed
    /// it; neither `--adopt` nor `--keep` applies.
    BackupOccupied(String),
}

/// The first eight characters of a tree hash, as it appears in commit subjects.
pub fn hash8(hash: &str) -> String {
    hash.chars().take(8).collect()
}

/// The outcome of a `git -C <library> …` call, with the two failure modes this
/// module distinguishes.
enum Git {
    Text(String),
    NotARepo,
    Failed(String),
}

fn git(library: &Path, args: &[&str]) -> Git {
    // A plain pool may live inside an unrelated repository. Never let git
    // walk upwards and stage/commit that repository on the pool's behalf.
    if !library.join(".git").exists() {
        return Git::NotARepo;
    }
    let mut argv = vec!["-C".to_string(), library.display().to_string()];
    argv.extend(args.iter().map(|arg| (*arg).to_string()));
    let output = match crate::git_command().args(&argv).output() {
        Ok(output) => output,
        Err(err) => return Git::Failed(format!("failed to run git: {err}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Git::Text(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if stderr.contains("not a git repository") || stderr.contains("cannot change to") {
        return Git::NotARepo;
    }
    Git::Failed(stderr.trim().to_string())
}

/// Stage `paths` and commit them when that changes anything.
///
/// Returns whether a commit was made; `false` means the paths already matched
/// `HEAD`, which is how a retry stays at one import record and one commit.
///
/// A resumed run may name a path a previous run already staged a deletion for.
/// There is nothing to add for it — `git add` refuses a missing path — but the
/// staged deletion is exactly what still needs committing, so the status check
/// covers every path the caller named.
pub fn git_commit(library: &Path, message: &str, paths: &[String]) -> Result<bool> {
    // In folder mode the published tree and record are the transaction's
    // durable result; there is intentionally no implicit git init or commit.
    if !library.join(".git").exists() {
        return Ok(false);
    }
    // `git add` stages a deletion for a path the index still lists, and refuses
    // one whose deletion is already staged — for that path the status check
    // below is all that is left to do.
    let indexed = indexed_paths(library, paths);
    let addable: Vec<&String> = paths
        .iter()
        .filter(|path| library.join(path.as_str()).exists() || index_knows(&indexed, path))
        .collect();
    if !addable.is_empty() {
        let mut add = vec!["add", "--"];
        add.extend(addable.iter().map(|path| path.as_str()));
        match git(library, &add) {
            Git::Text(_) => {}
            Git::NotARepo => {
                return Err(Error::git_failed(
                    format!("{} is not a git repository", library.display()),
                    "git add failed: not a git repository",
                ));
            }
            Git::Failed(message) => return Err(Error::git_failed("git add failed", &message)),
        }
    }

    let mut status = vec!["status", "--porcelain", "--"];
    status.extend(paths.iter().map(String::as_str));
    let pending = match git(library, &status) {
        Git::Text(text) => text,
        Git::NotARepo => {
            return Err(Error::git_failed(
                "git status failed",
                "not a git repository",
            ));
        }
        Git::Failed(message) => return Err(Error::git_failed("git status failed", &message)),
    };
    if pending.trim().is_empty() {
        return Ok(false);
    }
    failpoint(failpoint::AFTER_GIT_ADD)?;

    match git(library, &["commit", "-m", message]) {
        Git::Text(_) => {}
        Git::NotARepo => {
            return Err(Error::git_failed(
                "git commit failed",
                "not a git repository",
            ));
        }
        Git::Failed(detail) => {
            return Err(Error::git_failed(
                format!("git commit failed: {message}"),
                &detail,
            ));
        }
    }
    failpoint(failpoint::AFTER_COMMIT)?;
    Ok(true)
}

/// The paths git's index lists right now.
fn indexed_paths(library: &Path, paths: &[String]) -> BTreeSet<String> {
    let mut argv = vec!["ls-files", "--"];
    argv.extend(paths.iter().map(String::as_str));
    match git(library, &argv) {
        Git::Text(text) => text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
        _ => BTreeSet::new(),
    }
}

/// Whether the index lists `path` itself or anything under it. `git ls-files`
/// names files, so a deleted directory is known only through its children;
/// otherwise `remove` could commit its record while leaving the tree deletion
/// unstaged.
fn index_knows(indexed: &BTreeSet<String>, path: &str) -> bool {
    let prefix = format!("{}/", path.trim_end_matches('/'));
    indexed.contains(path) || indexed.iter().any(|entry| entry.starts_with(&prefix))
}

/// Whether git knows `path` at all: on disk, in the index, or in `HEAD`.
fn git_knows(library: &Path, path: &str) -> bool {
    if library.join(path).exists() || !indexed_paths(library, &[path.to_string()]).is_empty() {
        return true;
    }
    matches!(
        git(library, &["ls-tree", "--name-only", "HEAD", "--", path]),
        Git::Text(text) if !text.trim().is_empty()
    )
}

/// Render every configured surface.
pub fn render_surfaces(paths: &Paths, dry_run: bool) -> Result<Option<RenderReport>> {
    render::all_surfaces(paths, dry_run).map(Some)
}

/// One `git status --porcelain=v1 -z` record.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusEntry {
    x: char,
    y: char,
    path: String,
    /// The original path of a rename or copy; those records carry two.
    origin: Option<String>,
}

impl StatusEntry {
    /// Every path the record names.
    fn paths(&self) -> Vec<&String> {
        let mut paths = vec![&self.path];
        if let Some(origin) = &self.origin {
            paths.push(origin);
        }
        paths
    }

    /// `??`: git tracks nothing under this path.
    fn untracked(&self) -> bool {
        self.x == '?' && self.y == '?'
    }

    /// The index carries a change here (a rename or copy counts).
    fn staged(&self) -> bool {
        self.x != ' ' && self.x != '?'
    }
}

/// Parse `git status --porcelain=v1 -z`: records are NUL-terminated, and a
/// rename or copy carries its original path as the field after the new one.
fn parse_status(text: &str) -> Vec<StatusEntry> {
    let mut entries = Vec::new();
    let mut fields = text.split('\0');
    while let Some(field) = fields.next() {
        let mut chars = field.chars();
        let (Some(x), Some(y)) = (chars.next(), chars.next()) else {
            continue;
        };
        if field.len() < 4 {
            continue;
        }
        // The two status characters and the space are ASCII, so byte 3 is a
        // boundary whatever the path holds.
        let path = field[3..].to_string();
        let origin = if matches!(x, 'R' | 'C') {
            fields.next().map(str::to_string)
        } else {
            None
        };
        entries.push(StatusEntry { x, y, path, origin });
    }
    entries
}

/// `text` without the lines naming any of `names`, byte-identical elsewhere.
fn strip_name_lines(text: &str, names: &BTreeSet<String>) -> String {
    text.split_inclusive('\n')
        .filter(|line| !names.contains(line.trim().trim_start_matches('/')))
        .collect()
}

struct Batch<'a> {
    paths: &'a Paths,
    opts: ReconcileOpts,
    plan: Vec<PlanOp>,
    imported: Vec<Imported>,
    refused: Vec<Refusal>,
    variants_pending: Vec<String>,
    /// Inbox arrivals, first root wins, sorted by name.
    inbox: Vec<(String, PathBuf)>,
    /// The `.npx/<name>.json` records already in the pool.
    records: BTreeMap<String, NpxMeta>,
    /// The live `skills` lock, authoritative for source fields.
    lock: BTreeMap<String, LockEntry>,
    /// Records with `variantsPending` that no inbox name covers.
    retries: Vec<String>,
    /// A `--keep` a crash interrupted: name → the inbox copy it still owns.
    keep_marks: BTreeMap<String, PathBuf>,
    /// Names whose live entry this process displaced into `.npx/backup`: the
    /// only backups a run may delete are the ones it made itself.
    displaced: BTreeSet<String>,
    render: Option<RenderReport>,
}

impl<'a> Batch<'a> {
    fn new(paths: &'a Paths, opts: ReconcileOpts) -> Self {
        Self {
            paths,
            opts,
            plan: Vec::new(),
            imported: Vec::new(),
            refused: Vec::new(),
            variants_pending: Vec::new(),
            inbox: Vec::new(),
            records: BTreeMap::new(),
            lock: BTreeMap::new(),
            retries: Vec::new(),
            keep_marks: BTreeMap::new(),
            displaced: BTreeSet::new(),
            render: None,
        }
    }

    fn execute(mut self, guard: Option<MutatorGuard>) -> Result<ReconcileReport> {
        // A dry run neither takes the lock nor writes: it reports the plan.
        let guard = match guard {
            Some(guard) => Some(guard),
            None if self.opts.dry_run => None,
            None => Some(lock_library(self.paths)?),
        };

        scratch_is_sound(self.paths)?;
        self.scan_inbox()?;
        self.records = self.read_records()?;
        self.lock = npx::read_lock(&self.paths.lock)?;
        self.retries = self.pending_records();
        if !read_dir_names(&self.paths.library.join(".npx/tmp/keep"))?.is_empty() {
            output::warn(
                "legacy_keep_markers",
                "Recovery markers in library/.npx/tmp/keep are no longer trusted or resumed. Compare their named arrivals with the library, rerun reconcile --keep if needed, then remove these obsolete markers by hand.",
            );
        }
        self.keep_marks = self.read_keep_marks()?;

        let mut batch: BTreeSet<String> = self.inbox.iter().map(|(name, _)| name.clone()).collect();
        batch.extend(self.retries.iter().cloned());
        batch.extend(self.keep_marks.keys().cloned());
        batch.extend(self.opts.restore.iter().cloned());
        // A displaced copy with no live entry is reported, and put back only
        // on request — before the preflight, because the rename is the pool's
        // only copy moving. Every other scratch decision waits until the
        // preflight has allowed writes.
        scratch_is_sound(self.paths)?;
        self.report_displaced()?;
        self.restore_requested()?;
        self.preflight(&batch)?;
        self.sweep_scratch(&batch)?;

        self.import_inbox()?;
        self.retry_pending()?;
        // The keep step runs after every import is committed; when its staged
        // npx fails, the error still says what landed.
        let kept = match self.keep_resolved() {
            Ok(kept) => kept,
            Err(err) => {
                let imported = serde_json::to_value(&self.imported).unwrap_or_default();
                return Err(err.with_detail("imported", imported));
            }
        };
        self.finish(guard)?;

        Ok(ReconcileReport {
            imported: self.imported,
            refused: self.refused,
            kept,
            variants_pending: self.variants_pending,
            render: self.render,
            plan: self.plan,
        })
    }

    // ---- discovery ----------------------------------------------------

    fn inbox_roots(&self) -> Result<Vec<PathBuf>> {
        let owned = npx::recoverable_inboxes(self.paths)?;
        if self.opts.inbox.is_empty() {
            return Ok(owned);
        }
        for root in &self.opts.inbox {
            if !owned.contains(root) {
                return Err(Error::config_error(format!(
                    "{} is not an owned per-run stage inbox",
                    root.display()
                )));
            }
        }
        Ok(self.opts.inbox.clone())
    }

    /// Real directories carrying a `SKILL.md`, with no managed marker.
    fn scan_inbox(&mut self) -> Result<()> {
        for root in self.inbox_roots()? {
            let read = match fs::read_dir(&root) {
                Ok(read) => read,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(Error::io(
                        &format!("reading the inbox {}", root.display()),
                        &err,
                    ));
                }
            };
            let mut arrivals: Vec<(String, PathBuf)> = Vec::new();
            for dirent in read {
                let dirent = dirent.map_err(|err| Error::io("reading an inbox entry", &err))?;
                let name = dirent.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let path = dirent.path();
                let meta = fs::symlink_metadata(&path)
                    .map_err(|err| Error::io(&format!("reading {}", path.display()), &err))?;
                if !meta.file_type().is_dir() {
                    continue;
                }
                if !path.join("SKILL.md").is_file() || has_marker(&path) {
                    continue;
                }
                arrivals.push((name, path));
            }
            arrivals.sort();
            for (name, path) in arrivals {
                match validate_name(&name) {
                    Ok(_) => {
                        if !self.inbox.iter().any(|(known, _)| *known == name) {
                            self.inbox.push((name, path));
                        }
                    }
                    Err(error) => self.refused.push(Refusal {
                        name,
                        code: "invalid_name".to_string(),
                        message: error.to_string(),
                    }),
                }
            }
        }
        Ok(())
    }

    fn read_records(&self) -> Result<BTreeMap<String, NpxMeta>> {
        let dir = self.paths.library.join(".npx");
        let read = match fs::read_dir(&dir) {
            Ok(read) => read,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(err) => {
                return Err(Error::io(&format!("reading {}", dir.display()), &err));
            }
        };
        let mut records = BTreeMap::new();
        for dirent in read {
            let dirent = dirent.map_err(|err| Error::io("reading an import record", &err))?;
            let file = dirent.file_name().to_string_lossy().into_owned();
            let Some(stem) = file.strip_suffix(".json") else {
                continue;
            };
            if stem.is_empty() || stem.starts_with("skill-lock") || validate_name(stem).is_err() {
                continue;
            }
            let path = dirent.path();
            match crate::fsx::read_to_string(&path).and_then(|text| {
                serde_json::from_str::<NpxMeta>(&text)
                    .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
            }) {
                Ok(meta) => {
                    records.insert(stem.to_string(), meta);
                }
                Err(err) => output::warn(
                    "npx_record_unreadable",
                    format!(
                        "{} is not a readable import record ({err}); {} is treated as untracked",
                        path.display(),
                        stem
                    ),
                ),
            }
        }
        Ok(records)
    }

    fn pending_records(&self) -> Vec<String> {
        let inbox: BTreeSet<&String> = self.inbox.iter().map(|(name, _)| name).collect();
        self.records
            .iter()
            .filter(|(name, record)| record.variants_pending && !inbox.contains(name))
            .map(|(name, _)| name.clone())
            .collect()
    }

    // ---- preflight ----------------------------------------------------

    /// Refuse a library that carries work unrelated to this batch, before any
    /// write.
    ///
    /// `git status --porcelain=v1 -z` is the input, so a rename's two paths are
    /// both seen. This batch owns: a `.npx/<name>.json` record for one of its
    /// names, the names' own entries, a `.gitignore` whose only change is the
    /// lines for its names, and its scratch (`.npx/tmp/`, `.npx/backup/`).
    /// Everything else — any other staged path, any other `.npx` change, a
    /// `.npx/skill-lock.<platform>.json` change — is `library_dirty_unrelated`.
    fn preflight(&self, batch: &BTreeSet<String>) -> Result<()> {
        let text = match git(&self.paths.library, &["status", "--porcelain=v1", "-z"]) {
            Git::Text(text) => text,
            Git::NotARepo => return Ok(()),
            Git::Failed(message) => {
                return Err(Error::git_failed(
                    format!("git status failed in {}", self.paths.library.display()),
                    &message,
                ));
            }
        };

        let mut dirty: Vec<String> = Vec::new();
        for entry in parse_status(&text) {
            for path in entry.paths() {
                let trimmed = path.trim_end_matches('/');
                // git collapses a wholly untracked `.npx/` to one line; judge
                // each child by its own rule instead.
                if trimmed == ".npx" && entry.untracked() {
                    dirty.extend(self.dirty_npx_children(batch));
                } else if !self.owned_path(path, &entry, batch) {
                    dirty.push(path.clone());
                }
            }
        }
        dirty.sort();
        dirty.dedup();
        if dirty.is_empty() {
            return Ok(());
        }
        Err(Error::library_dirty_unrelated(
            format!(
                "the library has unrelated changes; reconcile only commits its own batch: {}",
                dirty.join(", ")
            ),
            dirty,
        ))
    }

    /// Whether one porcelain path is this tool's own in-flight state for this
    /// batch rather than a change to protect.
    fn owned_path(&self, path: &str, entry: &StatusEntry, batch: &BTreeSet<String>) -> bool {
        let trimmed = path.trim_end_matches('/');
        // This run's own lock file: every mutator creates it before the
        // preflight can look.
        if trimmed == ".skill.lock" {
            return true;
        }
        // The scratch, whole or as git collapses an untracked directory to one
        // line: `.npx/tmp/`, `.npx/backup/`, `.npx/tmp`, `.npx/backup`.
        if matches!(trimmed, ".npx/tmp" | ".npx/backup")
            || trimmed.starts_with(".npx/tmp/")
            || trimmed.starts_with(".npx/backup/")
        {
            return true;
        }
        if let Some(child) = trimmed.strip_prefix(".npx/") {
            return self.owned_npx_child(child, entry.staged(), batch);
        }
        if trimmed == ".gitignore" {
            return self.gitignore_is_ours(entry, batch);
        }
        // A keep in flight declares its names house before its commit; an
        // interrupted keep leaves that line behind for the resume to commit.
        if trimmed == crate::provenance::HOUSE_REL && !self.keep_marks.is_empty() {
            return true;
        }
        if batch.contains(trimmed) {
            return true;
        }
        // A file inside a batch entry is that entry's business: `classify`
        // decides, and a local edit is refused by its hash guard rather than by
        // the preflight.
        let in_batch_entry = batch.iter().any(|name| {
            trimmed
                .strip_prefix(name.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        });
        if in_batch_entry {
            return true;
        }
        // The plan's rule is about the index and about this tool's own
        // metadata: an unstaged change to some other file is not this batch's,
        // and every commit here names its paths explicitly, so it stays
        // uncommitted. A staged one is somebody's half-made commit.
        !entry.staged()
    }

    /// One `.npx/` child, under the same rule whether git named it or collapsed
    /// its parent: the scratch and this batch's records are ours, a directory
    /// that is not the scratch is not, and a stray file (an editor's, a
    /// filesystem's) is tolerated while it is unstaged — staged, it is
    /// somebody's half-made commit, like any other staged path.
    fn owned_npx_child(&self, child: &str, staged: bool, batch: &BTreeSet<String>) -> bool {
        if child == "tmp" || child == "backup" {
            return true;
        }
        if let Some(stem) = child.strip_suffix(".json") {
            return batch.contains(stem);
        }
        !staged && !is_real_dir(&self.paths.library.join(".npx").join(child))
    }

    /// A collapsed `?? .npx/` line, expanded into the paths it stands for.
    fn dirty_npx_children(&self, batch: &BTreeSet<String>) -> Vec<String> {
        let dir = self.paths.library.join(".npx");
        let mut dirty = Vec::new();
        for name in read_dir_names(&dir).unwrap_or_default() {
            // A collapsed `?? .npx/` is untracked, so nothing under it is staged.
            if !self.owned_npx_child(&name, false, batch) {
                dirty.push(format!(".npx/{name}"));
            }
        }
        dirty
    }

    /// Whether a `.gitignore` change is exactly the lines this batch drops:
    /// the file must be HEAD's content with the batch names' lines removed, in
    /// the worktree, and — when the change is staged — in the index too.
    fn gitignore_is_ours(&self, entry: &StatusEntry, batch: &BTreeSet<String>) -> bool {
        let expected = strip_name_lines(&self.blob("HEAD:.gitignore").unwrap_or_default(), batch);
        let worktree =
            crate::fsx::read_to_string(&self.paths.library.join(".gitignore")).unwrap_or_default();
        if worktree != expected {
            return false;
        }
        !entry.staged() || self.blob(":.gitignore").as_deref() == Some(expected.as_str())
    }

    /// One blob out of git, or `None` when it is absent or the call fails.
    fn blob(&self, spec: &str) -> Option<String> {
        match git(&self.paths.library, &["show", spec]) {
            Git::Text(text) => Some(text),
            _ => None,
        }
    }

    // ---- recovery ------------------------------------------------------

    /// The one repair made before the preflight: `backup/<n>` with no live
    /// entry is the only complete copy of `n`, displaced by a crash between the
    /// displace and the publish. It goes back by rename — nothing is deleted,
    /// nothing is read from the inbox — so a later refusal never leaves the
    /// pool without it.
    /// Every `backup/<n>` that is a real directory with no live `library/<n>`
    /// is the pool's only copy of that entry, left by a crash between the
    /// displace and the publish. This tool never moves it on its own: nothing
    /// in a writable scratch can prove who put it there (fix round 5, Sol
    /// re-check 4), so the run reports it and refuses the name until a human
    /// asks for the restore with `--restore <n>`.
    fn report_displaced(&self) -> Result<()> {
        let backups = self.paths.library.join(".npx/backup");
        for name in owned_names(&backups)? {
            if self.opts.restore.contains(&name) {
                continue;
            }
            let backup = backup_dir(self.paths, &name);
            let entry = self.paths.library.join(&name);
            if fs::symlink_metadata(&backup).is_err() || fs::symlink_metadata(&entry).is_ok() {
                continue;
            }
            if !is_real_dir(&backup) {
                output::warn(
                    "stale_backup",
                    format!(
                        "{} is not a directory and no live entry exists; this tool never removes what it did not displace — remove it by hand",
                        backup.display()
                    ),
                );
                continue;
            }
            output::warn(
                "stage_displaced",
                format!(
                    "{} holds the copy of {} an interrupted run displaced, and no live entry exists; nothing is restored automatically — `skillherd reconcile --restore {name}` puts it back",
                    backup.display(),
                    entry.display()
                ),
            );
        }
        Ok(())
    }

    /// `--restore <n>`: the human's instruction to put `backup/<n>` back at
    /// the live path. Only a real directory, only onto an empty path, by a
    /// no-replace rename; a dry run plans it. The destination side is
    /// race-free (the rename refuses a path that appeared); the source side
    /// is not: another process of the same user swapping `backup/<n>` between
    /// the check and the rename moves its object instead. Nothing is
    /// overwritten or deleted by that, and it is the accepted same-user
    /// concurrent-mutation limit of a path-named rename.
    fn restore_requested(&mut self) -> Result<()> {
        for name in self.opts.restore.clone() {
            validate_name(&name)?;
            let backup = backup_dir(self.paths, &name);
            let entry = self.paths.library.join(&name);
            if !is_real_dir(&backup) {
                return Err(Error::usage(format!(
                    "--restore {name}: {} is not a directory, so there is nothing to restore",
                    backup.display()
                )));
            }
            if fs::symlink_metadata(&entry).is_ok() {
                return Err(Error::usage(format!(
                    "--restore {name}: {} already exists; compare it with {} and keep one by hand",
                    entry.display(),
                    backup.display()
                )));
            }
            self.plan.push(PlanOp {
                op: "restore".to_string(),
                path: backup.clone(),
                target: Some(entry.clone()),
            });
            if self.opts.dry_run {
                continue;
            }
            rename_noreplace(&backup, &entry).map_err(|err| {
                Error::io(
                    &format!("restoring {} over {}", backup.display(), entry.display()),
                    &err,
                )
            })?;
            output::warn(
                "stage_restored",
                format!(
                    "{} is back in place from {}",
                    entry.display(),
                    backup.display()
                ),
            );
        }
        Ok(())
    }

    /// After the preflight: clear the scratch a crash left for names this batch
    /// will not touch. An orphan `tmp/<n>` beside a live entry is an incomplete
    /// copy with a complete one to fall back on. An orphan `backup/<n>` beside a
    /// live entry is never deleted — no record can say who put it there — only
    /// reported (`stale_backup`) for a human to compare and remove.
    fn sweep_scratch(&mut self, batch: &BTreeSet<String>) -> Result<()> {
        let staging = self.paths.library.join(".npx/tmp");
        for name in owned_names(&staging)? {
            if batch.contains(&name) {
                continue;
            }
            let tmp = staging_dir(self.paths, &name);
            let entry = self.paths.library.join(&name);
            if tmp.join("SKILL.md").is_file() && fs::symlink_metadata(&entry).is_ok() {
                self.plan.push(PlanOp {
                    op: "delete".to_string(),
                    path: tmp.clone(),
                    target: None,
                });
                if !self.opts.dry_run {
                    npx::remove_path(&tmp)?;
                }
            }
        }
        let backups = self.paths.library.join(".npx/backup");
        for name in owned_names(&backups)? {
            if batch.contains(&name) {
                continue;
            }
            let backup = backup_dir(self.paths, &name);
            let entry = self.paths.library.join(&name);
            // With no live entry, `report_displaced` has already spoken.
            if fs::symlink_metadata(&backup).is_err() || fs::symlink_metadata(&entry).is_err() {
                continue;
            }
            output::warn(
                "stale_backup",
                format!(
                    "{} was left beside {} by an earlier run; this tool never removes what it did not displace — compare the two and remove the backup by hand",
                    backup.display(),
                    entry.display()
                ),
            );
        }
        Ok(())
    }

    // ---- the import batch ---------------------------------------------

    fn classify(&self, name: &str, inbox: &Path) -> Result<Class> {
        let entry = self.paths.library.join(name);
        let meta = match fs::symlink_metadata(&entry) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let backup = backup_dir(self.paths, name);
                if is_real_dir(&backup) {
                    return Ok(Class::Displaced(format!(
                        "{} has no live entry, and {} holds the copy an interrupted run displaced",
                        entry.display(),
                        backup.display()
                    )));
                }
                if fs::symlink_metadata(&backup).is_ok() {
                    return Ok(Class::BackupOccupied(format!(
                        "{} has no live entry, and {} is occupied by something that is not a directory",
                        entry.display(),
                        backup.display()
                    )));
                }
                return Ok(Class::New);
            }
            Err(err) => return Err(Error::io(&format!("reading {}", entry.display()), &err)),
        };

        // Anything at `backup/<n>` beside a live entry was left by an earlier
        // run (or planted). The one case this tool can finish without touching
        // it is a live directory equal to the arrival — the publish a crash
        // landed but never recorded; `import` then finishes the record and the
        // commit and leaves the backup reported. Every other live entry would
        // need the backup path cleared to be replaced, and this tool never
        // clears what it did not displace.
        let backup = backup_dir(self.paths, name);
        if fs::symlink_metadata(&backup).is_ok() {
            if meta.is_dir() && self.pool_hash(name, &entry)? == self.pool_hash(name, inbox)? {
                return Ok(Class::AlreadyImported);
            }
            return Ok(Class::BackupOccupied(format!(
                "{} is not the arrival, and {} was left beside it by an earlier run; compare the two, remove the backup by hand, and rerun",
                entry.display(),
                backup.display()
            )));
        }

        if meta.file_type().is_symlink() {
            let link = fs::read_link(&entry).map_err(|err| Error::io("reading a link", &err))?;
            let target = lexical_abs(entry.parent().unwrap_or_else(|| Path::new(".")), &link);
            return Ok(Class::Conflict(format!(
                "{} is a symlink to {}, so it is not an npx import",
                entry.display(),
                target.display()
            )));
        }
        if !meta.is_dir() {
            return Ok(Class::Conflict(format!(
                "{} is not a directory",
                entry.display()
            )));
        }

        // A live entry equal to the arrival is the import (the ordinary
        // re-run). A live entry equal to the recorded import is the old tree.
        // Anything else is a local edit.
        let entry_hash = self.pool_hash(name, &entry)?;
        if entry_hash == self.pool_hash(name, inbox)? {
            return Ok(Class::AlreadyImported);
        }
        match self.records.get(name) {
            Some(record) if record.imported_hash == entry_hash => Ok(Class::UpstreamNewer),
            Some(record) => Ok(Class::Conflict(format!(
                "{} was edited locally (its hash is not the recorded import {})",
                entry.display(),
                hash8(&record.imported_hash)
            ))),
            None => Ok(Class::Conflict(format!(
                "{} exists with no .npx/{name}.json import record",
                entry.display()
            ))),
        }
    }

    fn import_inbox(&mut self) -> Result<()> {
        let entries: Vec<(String, PathBuf)> = self.inbox.clone();
        let mut classes: Vec<Class> = Vec::with_capacity(entries.len());
        for (name, inbox) in &entries {
            classes.push(self.classify(name, inbox)?);
        }

        // `--adopt` with no arrival would do nothing and report success, which
        // reads as "adopted" when nothing was: a refused `skillherd update` keeps
        // its arrival in its own stage, where a plain `skillherd reconcile` does
        // not look. Refuse it and name the retry that fetches the arrival.
        for name in &self.opts.adopt {
            if !entries.iter().any(|(candidate, _)| candidate == name) {
                return Err(Error::usage(format!(
                    "--adopt {name}: no inbox directory named {name}, so there is nothing to adopt; `skillherd install <source> --skill {name} --adopt {name}` fetches the upstream copy and adopts it in one run"
                )));
            }
        }

        // `--keep` means "keep the house copy of a conflict"; anything else is a
        // typo or a stale retry, and the argument is worth rejecting.
        for name in self.opts.keep.clone() {
            if self.keep_marks.contains_key(&name) {
                continue;
            }
            let class = entries
                .iter()
                .position(|(candidate, _)| *candidate == name)
                .map(|index| classes[index].clone());
            match class {
                Some(Class::Conflict(_)) => {}
                Some(Class::Displaced(_)) | Some(Class::BackupOccupied(_)) => {
                    return Err(Error::usage(format!(
                        "--keep {name}: {name} is refused while {} exists; clear it first (`--restore {name}` when no live entry exists)",
                        backup_dir(self.paths, &name).display()
                    )));
                }
                Some(_) => {
                    return Err(Error::usage(format!(
                        "--keep {name}: the pool copy of {name} already matches the inbox copy, so there is nothing to keep"
                    )));
                }
                None => {
                    return Err(Error::usage(format!(
                        "--keep {name}: no inbox directory named {name}"
                    )));
                }
            }
        }

        for ((name, inbox), class) in entries.iter().zip(classes) {
            // A keep a crash interrupted owns this name: `keep_resolved`
            // finishes it, inbox copy and all.
            if self.keep_marks.contains_key(name) {
                continue;
            }
            if class == Class::UpstreamNewer
                && !self.opts.adopt.contains(name)
                && !self
                    .opts
                    .inbox
                    .iter()
                    .any(|root| inbox.parent() == Some(root.as_path()))
            {
                let root = inbox
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::parent)
                    .expect("owned inbox layout");
                let stage_lock = npx::read_lock(&root.join(npx::STAGE_LOCK))?;
                let matches_record = stage_lock
                    .get(name)
                    .zip(self.records.get(name))
                    .is_some_and(|(arrival, record)| {
                        !arrival.skill_folder_hash.is_empty()
                            && arrival.skill_folder_hash == record.skill_folder_hash
                    });
                if !matches_record {
                    self.refused.push(Refusal {
                        name: name.clone(),
                        code: "stale_arrival".into(),
                        message: format!("{} is a recovered arrival, not this run's upstream; use `skillherd reconcile --adopt {name}` to import it or `skillherd reconcile --discard {name}` to discard it", inbox.display()),
                    });
                    continue;
                }
            }
            match class {
                Class::Conflict(_) if self.opts.adopt.contains(name) => {
                    self.plan.push(PlanOp {
                        op: "refresh".to_string(),
                        path: self.paths.library.join(name),
                        target: Some(inbox.clone()),
                    });
                    self.import(name, inbox, &class)?;
                }
                Class::Conflict(_) if self.opts.keep.contains(name) => {
                    self.keep(name, inbox)?;
                }
                Class::Conflict(reason) => self.refused.push(Refusal {
                    name: name.clone(),
                    code: "local_edit_refused".to_string(),
                    message: format!(
                        "{reason}; rerun with `--adopt {name}` to take upstream or `--keep {name}` to keep the house copy and stop tracking it upstream (the error's suggested_fix names the command to rerun), and forking under a distinct name is the house rule"
                    ),
                }),
                Class::Displaced(reason) => self.refused.push(Refusal {
                    name: name.clone(),
                    code: "displaced_refused".to_string(),
                    message: format!(
                        "{reason}; nothing is restored automatically — `skillherd reconcile --restore {name}` puts it back, then rerun"
                    ),
                }),
                Class::BackupOccupied(reason) => self.refused.push(Refusal {
                    name: name.clone(),
                    code: "stale_backup_refused".to_string(),
                    message: format!(
                        "{reason}; this tool never removes what it did not displace, so neither `--adopt` nor `--keep` applies until the backup is gone"
                    ),
                }),
                class => self.import(name, inbox, &class)?,
            }
        }
        Ok(())
    }

    /// Copy one arrival in, variants and all, and commit it before the inbox
    /// copy goes away.
    fn import(&mut self, name: &str, inbox: &Path, class: &Class) -> Result<()> {
        let entry = self.paths.library.join(name);
        let backup = backup_dir(self.paths, name);
        let tmp = staging_dir(self.paths, name);
        // An entry that already equals the arrival — the ordinary re-run, or
        // the publish a crash landed but never recorded — has nothing to copy
        // or to swap: only its record and its commit may be missing.
        let replacing = *class != Class::AlreadyImported;
        let swapping = replacing;

        if swapping {
            self.plan.push(PlanOp {
                op: "copy".to_string(),
                path: tmp.clone(),
                target: Some(inbox.to_path_buf()),
            });
            if fs::symlink_metadata(&entry).is_ok() {
                self.plan.push(PlanOp {
                    op: "rename".to_string(),
                    path: backup.clone(),
                    target: Some(entry.clone()),
                });
            }
            self.plan.push(PlanOp {
                op: "rename".to_string(),
                path: entry.clone(),
                target: Some(tmp.clone()),
            });
            if !self.opts.dry_run {
                self.publish(name, inbox)?;
            }
        }

        // A dry run describes the arrival where a real run has already put it.
        let baseline = if self.opts.dry_run && swapping {
            inbox
        } else {
            entry.as_path()
        };
        let imported_hash = self.pool_hash(name, baseline)?;
        let previous = self.records.get(name).cloned();
        let fetched = self.opts.fetched.get(name).cloned();
        // A release fetch has no clone to take variants from: its record is
        // complete as `release::record_for` built it.
        let base = match fetched {
            Some(_) => None,
            None => self.source_meta(name, previous.as_ref()),
        };
        let mut variants_map = BTreeMap::new();
        let mut agents_map = BTreeMap::new();
        let mut revision = String::new();
        let mut pending = false;
        let mut pending_reason = String::new();
        if let Some(record) = &base {
            let outcome = variants::import(self.paths, name, record, baseline, self.opts.dry_run)?;
            variants_map = outcome.variants;
            agents_map = outcome.harness_agents;
            revision = outcome.revision;
            pending = outcome.pending;
            pending_reason = outcome.reason.unwrap_or_default();
            self.plan.extend(outcome.plan);
            self.refused.extend(outcome.refusals);
        }

        let meta = match (base, fetched, previous.clone()) {
            (Some(base), _, _) => NpxMeta {
                imported_hash,
                imported_at: pick_at(&base.imported_at, previous.as_ref()),
                revision,
                variants: variants_map,
                variants_pending: pending,
                harness_agents: agents_map,
                ..base
            },
            // Fetched from a release by this run: the fetch's record, stamped
            // like any import.
            (None, Some(fetched), _) => NpxMeta {
                imported_hash,
                imported_at: pick_at(&fetched.imported_at, previous.as_ref()),
                ..fetched
            },
            // Tracked before, with no upstream to fetch variants from: keep the
            // record, refresh only what this run observed.
            (None, None, Some(record)) => NpxMeta {
                imported_hash,
                imported_at: pick_at(&record.imported_at, Some(&record)),
                variants_pending: false,
                ..record
            },
            (None, None, None) => NpxMeta {
                source: String::new(),
                source_type: "local".to_string(),
                source_url: String::new(),
                r#ref: None,
                skill_path: String::new(),
                skill_folder_hash: String::new(),
                imported_hash,
                imported_at: crate::model::now_iso8601(),
                revision: String::new(),
                variants: BTreeMap::new(),
                variants_pending: false,
                harness_agents: BTreeMap::new(),
                release: None,
            },
        };
        if meta.variants_pending {
            let reason = if pending_reason.is_empty() {
                "upstream is unreachable".to_string()
            } else {
                pending_reason
            };
            output::warn_error(&Error::variants_unavailable(name, &reason));
            self.variants_pending.push(name.to_string());
        }
        let record_changed = self.write_record(name, &meta)?;
        failpoint(failpoint::AFTER_RECORD_WRITE)?;

        let mut staged = vec![name.to_string(), format!(".npx/{name}.json")];
        if self.drop_gitignore_line(name)? {
            staged.push(".gitignore".to_string());
        }
        let subject = format!("npx: import {name} ({})", hash8(&meta.imported_hash));
        self.commit(&subject, &staged, replacing || record_changed)?;

        // The import is durable, so the copy this process displaced and its
        // scratch tree are not the pool's only copy any more. A backup some
        // earlier run left (the publish a crash landed, finished just now) is
        // not this run's to delete: reported, kept.
        let owns_backup = self.displaced.contains(name);
        if !owns_backup && fs::symlink_metadata(&backup).is_ok() {
            output::warn(
                "stale_backup",
                format!(
                    "{} was left beside {} by an earlier run and the import is now complete; this tool never removes what it did not displace — compare the two and remove the backup by hand",
                    backup.display(),
                    entry.display()
                ),
            );
        }
        for scratch in [&backup, &tmp] {
            if fs::symlink_metadata(scratch).is_err() {
                continue;
            }
            if *scratch == backup && !owns_backup {
                continue;
            }
            self.plan.push(PlanOp {
                op: "delete".to_string(),
                path: scratch.clone(),
                target: None,
            });
            if !self.opts.dry_run {
                npx::remove_path(scratch)?;
            }
        }

        // Only now: the inbox copy survives until the commit has landed.
        failpoint(failpoint::BEFORE_INBOX_DELETE)?;
        self.plan.push(PlanOp {
            op: "delete".to_string(),
            path: inbox.to_path_buf(),
            target: None,
        });
        if !self.opts.dry_run {
            self.validate_inbox_target(name, inbox)?;
            npx::remove_path(inbox)?;
            if !self.opts.inbox.is_empty() {
                // A current fetch supersedes older arrivals of this name. Do
                // this only after its import commit is durable, under the lock.
                for root in npx::recoverable_inboxes(self.paths)? {
                    if self.opts.inbox.contains(&root) {
                        continue;
                    }
                    let stale = root.join(name);
                    self.validate_inbox_target(name, &stale)?;
                    npx::remove_path(&stale)?;
                }
            }
        }
        self.imported.push(Imported {
            name: name.to_string(),
            hash: meta.imported_hash.clone(),
            variants: meta.variants.keys().cloned().collect(),
        });
        Ok(())
    }

    /// Build the complete replacement for `library/<name>` in the scratch and
    /// swap it in, leaving the pool without a complete copy of the entry for
    /// no instant: the displaced entry waits in `.npx/backup` until the import
    /// commit has landed.
    ///
    /// Idempotent, so the run after a crash resumes from whatever is on disk;
    /// `classify` never routes a name whose backup path is occupied here, so
    /// the displace below never has to clear anything out of its way.
    fn publish(&mut self, name: &str, inbox: &Path) -> Result<()> {
        let entry = self.paths.library.join(name);
        let tmp = staging_dir(self.paths, name);
        let backup = backup_dir(self.paths, name);

        if fs::symlink_metadata(&backup).is_ok() {
            // Whatever is there, this tool did not put it there in this run,
            // and it is not this tool's to remove or to rename over.
            return Err(Error::internal(format!(
                "refusing to publish {}: {} is occupied by something this run did not displace",
                entry.display(),
                backup.display()
            )));
        }

        // The replacement is complete in the scratch before the live entry is
        // touched, so no `copy_tree` ever deletes a destination that is the
        // only copy.
        variants::copy_tree(inbox, &tmp, &[])?;
        failpoint(failpoint::AFTER_COPY_TO_TMP)?;
        if is_real_dir(&entry) {
            // Previously imported variant and agent trees belong to the entry,
            // not to the arrival: carry them across so their hash guards keep
            // working (an edited copy is refused, never silently replaced).
            for kept in [".variants", crate::agents::AGENTS_DIR] {
                let old = entry.join(kept);
                if old.is_dir() {
                    let dest = tmp.join(kept);
                    npx::remove_path(&dest)?;
                    variants::copy_tree(&old, &dest, &[])?;
                }
            }
        }
        failpoint(failpoint::AFTER_VARIANT_TRANSFER)?;

        if fs::symlink_metadata(&entry).is_ok() {
            if let Some(parent) = backup.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| Error::io(&format!("creating {}", parent.display()), &err))?;
            }
            // The backup path is free (guard above): nothing is cleared to
            // make room. From here this process owns `backup/<n>`.
            rename_noreplace(&entry, &backup).map_err(|err| {
                Error::io(
                    &format!("displacing {} to {}", entry.display(), backup.display()),
                    &err,
                )
            })?;
            self.displaced.insert(name.to_string());
        }
        failpoint(failpoint::AFTER_DISPLACE)?;
        // The live path was empty a moment ago; anything there now is not
        // ours to replace, and the displaced copy waits in the backup.
        rename_noreplace(&tmp, &entry)
            .map_err(|err| Error::io(&format!("renaming {} into place", entry.display()), &err))?;
        failpoint(failpoint::AFTER_PUBLISH)?;
        Ok(())
    }

    /// The record a variant pass starts from: the live lock's source fields,
    /// with the previous record's variant hashes as the edit guard.
    /// The hash of a tree as the pool commits it at `name`: the same for an
    /// arrival and for the pool copy it becomes, and the same on every clone.
    fn pool_hash(&self, name: &str, dir: &Path) -> Result<String> {
        committed_tree_hash(&self.paths.library, name, dir, &[])
    }

    fn source_meta(&self, name: &str, previous: Option<&NpxMeta>) -> Option<NpxMeta> {
        let from_lock = self
            .lock
            .get(name)
            .map(|entry| entry.to_meta(String::new(), String::new()));
        let meta = match (from_lock, previous) {
            (Some(mut meta), Some(previous)) => {
                meta.variants = previous.variants.clone();
                meta.harness_agents = previous.harness_agents.clone();
                meta.revision = previous.revision.clone();
                meta
            }
            (Some(meta), None) => meta,
            (None, Some(previous)) => previous.clone(),
            (None, None) => return None,
        };
        // A skill with no upstream at all is not a pending variant import: it
        // has no variants to fetch, now or later. Nor is a release-tracked
        // one: its upstream is a zip, not a repository to clone.
        if meta.source_url.trim().is_empty() || meta.release.is_some() {
            return None;
        }
        Some(meta)
    }

    /// Write `library/.npx/<name>.json` atomically; the return value says
    /// whether the bytes on disk changed, which is what decides a commit.
    fn write_record(&mut self, name: &str, meta: &NpxMeta) -> Result<bool> {
        for spec in crate::agents::AGENT_HARNESSES {
            if !meta.harness_agents.contains_key(spec.harness) {
                continue;
            }
            let stored = crate::agents::stored_agents(self.paths, name, spec.harness);
            let files = npx::read_dir_names(&stored)?;
            if !files.is_empty() {
                output::warn(
                    "imported_subagents",
                    format!(
                        "{name} includes {} subagent definitions: {}. These are instructions loaded by the harness; review them before use.",
                        spec.harness,
                        files.join(", ")
                    ),
                );
            }
        }
        let target = self.paths.library.join(".npx").join(format!("{name}.json"));
        self.plan.push(PlanOp {
            op: "write".to_string(),
            path: target.clone(),
            target: None,
        });
        let mut text = serde_json::to_string_pretty(meta)
            .map_err(|err| Error::internal(format!("serializing the import record: {err}")))?;
        text.push('\n');
        if crate::fsx::read_to_string(&target).is_ok_and(|existing| existing == text) {
            return Ok(false);
        }
        if self.opts.dry_run {
            return Ok(true);
        }
        crate::fsx::atomic_write(&target, text.as_bytes())
            .map_err(|err| Error::io(&format!("writing {}", target.display()), &err))?;
        Ok(true)
    }

    /// Drop `name` from the library `.gitignore`, byte-identically elsewhere.
    fn drop_gitignore_line(&mut self, name: &str) -> Result<bool> {
        let path = self.paths.library.join(".gitignore");
        let text = match crate::fsx::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => {
                return Err(Error::io(&format!("reading {}", path.display()), &err));
            }
        };
        let names: BTreeSet<String> = std::iter::once(name.to_string()).collect();
        let kept = strip_name_lines(&text, &names);
        if kept == text {
            return Ok(false);
        }
        self.plan.push(PlanOp {
            op: "write".to_string(),
            path: path.clone(),
            target: None,
        });
        if !self.opts.dry_run {
            crate::fsx::atomic_write(&path, kept.as_bytes())
                .map_err(|err| Error::io(&format!("writing {}", path.display()), &err))?;
        }
        Ok(true)
    }

    /// Stage and commit, or, for a dry run, report the commit `predicted` by
    /// the caller's own reasoning about what would change.
    fn commit(&mut self, subject: &str, paths: &[String], predicted: bool) -> Result<bool> {
        let changed = if self.opts.dry_run {
            predicted
        } else {
            git_commit(&self.paths.library, subject, paths)?
        };
        if changed {
            self.plan.push(PlanOp {
                op: "git".to_string(),
                path: self.paths.library.clone(),
                target: None,
            });
        }
        Ok(changed)
    }

    /// Complete imports whose variants could not be fetched last time.
    fn retry_pending(&mut self) -> Result<()> {
        for name in self.retries.clone() {
            let Some(record) = self.records.get(&name).cloned() else {
                continue;
            };
            let entry = self.paths.library.join(&name);
            let outcome = variants::import(
                self.paths,
                &name,
                &record,
                entry.as_path(),
                self.opts.dry_run,
            )?;
            self.plan.extend(outcome.plan.clone());
            self.refused.extend(outcome.refusals.clone());
            let meta = NpxMeta {
                revision: outcome.revision,
                variants: outcome.variants,
                variants_pending: outcome.pending,
                harness_agents: outcome.harness_agents,
                ..record.clone()
            };
            if outcome.pending {
                output::warn_error(&Error::variants_unavailable(
                    &name,
                    outcome
                        .reason
                        .as_deref()
                        .unwrap_or("upstream is unreachable"),
                ));
                self.variants_pending.push(name.clone());
            }
            if meta == record {
                continue;
            }
            self.write_record(&name, &meta)?;
            let subject = format!("npx: import {name} ({})", hash8(&meta.imported_hash));
            self.commit(&subject, &[name.clone(), format!(".npx/{name}.json")], true)?;
        }
        Ok(())
    }

    /// Recovery may only consume exactly an owned stage root plus a validated
    /// skill name. A marker body is never an arbitrary deletion capability.
    fn validate_inbox_target(&self, name: &str, path: &Path) -> Result<()> {
        validate_name(name)?;
        let normalized = lexical_abs(&self.paths.home, path);
        let allowed = npx::recoverable_inboxes(self.paths)?
            .iter()
            .any(|root| normalized == root.join(name));
        if path != normalized || !allowed {
            return Err(Error::config_error(format!(
                "recovery target {} is not exactly an owned stage inbox/{name}",
                path.display()
            )));
        }
        if let Ok(meta) = fs::symlink_metadata(path)
            && !meta.is_dir()
        {
            return Err(Error::config_error(
                "an inbox deletion target is not a real directory",
            ));
        }
        Ok(())
    }

    /// A `--keep` name keeps its pool copy and stops being tracked upstream.
    ///
    /// The order is the recovery contract, and `keep_resolved` runs it: mark
    /// the intent here, run the staged `npx remove` and commit the metadata
    /// transition there, and delete the inbox copy only once that commit has
    /// landed. The marker names the inbox copy, so the next run can finish any
    /// interruption point.
    fn keep(&mut self, name: &str, inbox: &Path) -> Result<()> {
        let mark = keep_mark(self.paths, name);
        self.plan.push(PlanOp {
            op: "write".to_string(),
            path: mark.clone(),
            target: None,
        });
        if !self.opts.dry_run {
            self.validate_inbox_target(name, inbox)?;
            crate::fsx::atomic_write(&mark, format!("{}\n", inbox.display()).as_bytes())
                .map_err(|err| Error::io(&format!("writing {}", mark.display()), &err))?;
        }
        self.keep_marks
            .insert(name.to_string(), inbox.to_path_buf());
        Ok(())
    }

    /// The keeps this run has to finish, fresh ones and ones a crash left
    /// behind.
    fn read_keep_marks(&self) -> Result<BTreeMap<String, PathBuf>> {
        let dir = keep_dir(self.paths);
        let names = match crate::fsx::read_dir_names_nofollow(&dir) {
            Ok(names) => names,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(err) => return Err(Error::io(&format!("reading {}", dir.display()), &err)),
        };
        let mut marks = BTreeMap::new();
        for name in names {
            let name = name.to_string_lossy().into_owned();
            if validate_name(&name).is_err() {
                continue;
            }
            let marker = dir.join(&name);
            let target = crate::fsx::read_to_string(&marker)
                .map_err(|err| Error::io("reading a recovery marker", &err))
                .and_then(|body| {
                    let inbox = PathBuf::from(body.trim());
                    self.validate_inbox_target(&name, &inbox)?;
                    Ok(inbox)
                });
            match target {
                Ok(inbox) => {
                    marks.insert(name, inbox);
                }
                Err(err) => {
                    output::warn(
                        "stale_keep_marker",
                        format!(
                            "ignoring stale recovery marker {}: {err}; no arrival will be deleted",
                            marker.display()
                        ),
                    );
                    if !self.opts.dry_run {
                        fs::remove_file(&marker)
                            .map_err(|err| Error::io("removing stale recovery marker", &err))?;
                    }
                }
            }
        }
        Ok(marks)
    }

    /// The staged npx run that makes the lock forget the kept names, the
    /// metadata commit, and last the inbox copies.
    fn keep_resolved(&mut self) -> Result<Vec<String>> {
        let names: Vec<String> = self.keep_marks.keys().cloned().collect();
        if names.is_empty() {
            return Ok(names);
        }

        // 1. The lock transition: npx forgets what it still tracks. A crashed
        //    keep may have got this far already, and then there is nothing to
        //    do — the live lock is the record of it.
        let tracked: Vec<String> = names
            .iter()
            .filter(|name| self.lock.contains_key(*name))
            .cloned()
            .collect();
        if !tracked.is_empty() {
            let verb = Verb::Remove { names: tracked };
            self.plan.push(verb.plan_op(self.paths));
            if !self.opts.dry_run {
                let stage = npx::prepare_stage(self.paths)?;
                npx::run(self.paths, &verb)?;
                npx::copy_file_if_exists(&stage.lock(), &self.paths.lock)?;
                stage.clear()?;
            }
            failpoint(failpoint::AFTER_KEEP_NPX)?;
        }

        // 2. The metadata transition: the record goes, and the commit is what
        //    makes that durable. A crashed keep may already have deleted the
        //    record from disk, so the commit names every record this run knew
        //    about — the ones a previous run staged are still stageable, and
        //    `git_commit` decides whether anything is left to commit.
        let mut dropped: Vec<String> = Vec::new();
        for name in &names {
            let record = self.paths.library.join(".npx").join(format!("{name}.json"));
            if fs::symlink_metadata(&record).is_err() {
                continue;
            }
            dropped.push(name.clone());
            self.plan.push(PlanOp {
                op: "delete".to_string(),
                path: record.clone(),
                target: None,
            });
            if !self.opts.dry_run {
                npx::remove_path(&record)?;
            }
        }
        //    Every name's record is named, not only the ones still on disk: a
        //    resumed run has to commit the deletion a previous run staged, and
        //    `git_commit` drops the paths that are neither on disk nor tracked.
        let mut staged: Vec<String> = names
            .iter()
            .map(|name| format!(".npx/{name}.json"))
            .collect();
        let mut predicted = names.iter().any(|name| {
            self.records.contains_key(name.as_str())
                || git_knows(&self.paths.library, &format!(".npx/{name}.json"))
        });
        //    A kept copy is a house copy: `.globals/house` says so in the same
        //    commit, so doctor never reports it as a copy of unknown origin.
        let house = crate::provenance::load_house(self.paths)?;
        for name in &names {
            if house.contains(name) {
                continue;
            }
            let source = self
                .records
                .get(name.as_str())
                .map(|record| record.source.clone())
                .or_else(|| {
                    self.lock
                        .get(name.as_str())
                        .map(|entry| entry.source.clone())
                })
                .filter(|source| !source.is_empty());
            let note = match source {
                Some(source) => format!("kept over upstream {source} by `--keep`"),
                None => "kept over its upstream copy by `--keep`".to_string(),
            };
            self.plan.push(PlanOp {
                op: "write".to_string(),
                path: crate::provenance::house_path(self.paths),
                target: None,
            });
            predicted = true;
            if !self.opts.dry_run {
                crate::provenance::declare_house(self.paths, name, Some(&note))?;
            }
        }
        staged.push(crate::provenance::HOUSE_REL.to_string());
        self.commit(
            &format!("npx: keep {}", names.join(" ")),
            &staged,
            predicted,
        )?;

        // 3. Only after the commit: the inbox copy, then the marker that
        //    remembers it.
        for name in &names {
            // Only an inbox copy under a configured inbox root is ever
            // deleted: the marker's path is a record, not a licence.
            let inbox = self.keep_marks.get(name).cloned();
            if let Some(ref inbox) = inbox {
                self.validate_inbox_target(name, inbox)?;
            }
            if let Some(inbox) = inbox {
                self.plan.push(PlanOp {
                    op: "delete".to_string(),
                    path: inbox.clone(),
                    target: None,
                });
                if !self.opts.dry_run {
                    npx::remove_path(&inbox)?;
                }
            }
            let mark = keep_mark(self.paths, name);
            if fs::symlink_metadata(&mark).is_ok() {
                self.plan.push(PlanOp {
                    op: "delete".to_string(),
                    path: mark.clone(),
                    target: None,
                });
                if !self.opts.dry_run {
                    npx::remove_path(&mark)?;
                }
            }
        }
        Ok(names)
    }

    // ---- close out ----------------------------------------------------

    fn finish(&mut self, guard: Option<MutatorGuard>) -> Result<()> {
        if npx::snapshot_lock(self.paths, self.opts.dry_run)? {
            let snapshot = npx::lock_snapshot(self.paths);
            self.plan.push(PlanOp {
                op: "write".to_string(),
                path: self.paths.library.join(&snapshot),
                target: None,
            });
            self.commit("npx: lock snapshot", &[snapshot], true)?;
        }
        // Render takes the library lock itself, so it must not be called while
        // this batch still holds it.
        if !self.opts.dry_run {
            npx::clear_consumed_stages(self.paths)?;
        }
        drop(guard);
        self.render = render_surfaces(self.paths, self.opts.dry_run)?;
        Ok(())
    }
}

/// When the entry was imported: the live lock carries no stamp, so the record's
/// own stands — the next record of a previous import — and only a genuinely new
/// import is stamped now. A retry that re-stamped the record would commit a
/// byte-different record for an unchanged tree.
fn pick_at(existing: &str, previous: Option<&NpxMeta>) -> String {
    if !existing.is_empty() {
        return existing.to_string();
    }
    match previous {
        Some(previous) if !previous.imported_at.is_empty() => previous.imported_at.clone(),
        _ => crate::model::now_iso8601(),
    }
}
