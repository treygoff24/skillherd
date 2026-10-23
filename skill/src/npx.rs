//! Staged npx execution.
//!
//! `install`, `update` and `remove` never run npx against the live HOME: they
//! build a throwaway `stage/` home, run `<npx> -y <pkg> <verb> …` there, and
//! reconcile what it left behind. The wrapper also refuses the `~/.local/bin`
//! npx shim, which points back at `skillherd`.
//!
//! The child's output is diagnostics, never data. It is captured for redacted
//! failure details and redacted complete lines stream live to a terminal on stderr.
//! The child is redirected, not sandboxed: git/SSH can still consult system
//! configuration and the passwd home. SKILLHERD_NPX_CREDENTIALS=1 explicitly passes
//! credential variables and points git/npm at the operator's user config.
//!
//! This module also owns the `skills` lock format: reading it, copying the live
//! lock into the stage, and (only with SKILLHERD_SNAPSHOT_LOCK=1) snapshotting it into
//! `library/.npx/skill-lock.<platform>.json`.
//!
//! # Stage ownership
//!
//! `Paths.stage` is a container, not the stage itself: each run builds a
//! uniquely named child (`<stage>/run-<stamp>-<pid>/`) and marks it with
//! [`STAGE_MARKER`] before writing anything else into it. Removal — of the run
//! directory, and of whatever a crashed run left behind — happens only after
//! that marker has been read and recognised, so `SKILLHERD_STAGE=$HOME/Documents`
//! can no longer delete a tree this tool never created. The container carries
//! the same marker, and is dropped once it is ours and empty.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{NpxMeta, PlanOp};
use crate::paths::Paths;

/// The hidden `skills` agent whose only target is the canonical dir
/// (`$HOME/.agents/skills`). With `skills@1.7.0`,
/// `add … -g -a universal -y` exits 0 and copies into the
/// canonical dir alone.
pub const UNIVERSAL_AGENT: &str = "universal";

/// The stage lock, relative to the stage root: npx's own lock layout.
pub const STAGE_LOCK: &str = ".agents/.skill-lock.json";

/// The stage inbox, relative to the stage root.
pub const STAGE_INBOX: &str = ".agents/skills";

/// The snapshot of the live lock kept inside the library.
/// The pool's copy of this machine's live lock, relative to the library:
/// `.npx/skill-lock.<platform>.json`. One file per platform, because every
/// machine's lock differs (install dates, `lastSelectedAgents`) and a single
/// shared snapshot would be rewritten by each machine's reconcile in turn.
pub fn lock_snapshot(paths: &Paths) -> String {
    format!(".npx/skill-lock.{}.json", paths.platform)
}

/// The ownership marker every stage directory carries, as its single line:
/// `skill-stage-owner/v1 skill <nonce>`. A directory is deleted only when this
/// marker has been read from it and recognised as ours.
pub const STAGE_MARKER: &str = ".skill-stage-owner";

/// The marker's format tag; a later version recognises its own.
const STAGE_MARKER_FORMAT: &str = "skill-stage-owner/v1";

/// The owner name the marker records.
const STAGE_OWNER: &str = "skill";

/// The prefix of the per-run stage directory under `Paths.stage`.
const RUN_PREFIX: &str = "run-";

/// How much of a failed run's output the error carries.
const OUTPUT_TAIL_BYTES: usize = 4000;

thread_local! {
    /// The stage the current thread built, so the `Paths`-shaped helpers below
    /// (`run`, `stage_inbox`, `stage_lock`, `clear_stage`) address the same run
    /// as the `Stage` handle. The staged `remove` flow holds only `Paths`.
    static ACTIVE_STAGE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// One run's stage: the throwaway HOME npx sees, and the arrival inbox
/// `reconcile` consumes afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    root: PathBuf,
}

impl Stage {
    /// The run directory: the `HOME` the child is given.
    pub fn home(&self) -> &Path {
        &self.root
    }

    /// The inbox npx writes in the stage.
    pub fn inbox(&self) -> PathBuf {
        self.root.join(STAGE_INBOX)
    }

    /// The lock npx writes in the stage.
    pub fn lock(&self) -> PathBuf {
        self.root.join(STAGE_LOCK)
    }

    /// Discard the stage once its lock, and its inbox, have been consumed.
    pub fn clear(self) -> Result<()> {
        ACTIVE_STAGE.with(|cell| {
            let mut active = cell.borrow_mut();
            if active.as_deref() == Some(self.root.as_path()) {
                *active = None;
            }
        });
        clear_run(&self.root, self.root.parent().unwrap_or(Path::new(".")))
    }
}

/// One `skills` lock entry, as `skills@1.7.0` writes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    #[serde(default)]
    pub source: String,
    #[serde(default, rename = "sourceType")]
    pub source_type: String,
    #[serde(default, rename = "sourceUrl")]
    pub source_url: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default, rename = "skillPath")]
    pub skill_path: String,
    #[serde(default, rename = "skillFolderHash")]
    pub skill_folder_hash: String,
}

impl LockEntry {
    /// The import record for this lock entry, before variants are resolved.
    pub fn to_meta(&self, imported_hash: String, imported_at: String) -> NpxMeta {
        NpxMeta {
            source: self.source.clone(),
            source_type: self.source_type.clone(),
            source_url: self.source_url.clone(),
            r#ref: self.r#ref.clone(),
            skill_path: self.skill_path.clone(),
            skill_folder_hash: self.skill_folder_hash.clone(),
            imported_hash,
            imported_at,
            revision: String::new(),
            variants: BTreeMap::new(),
            variants_pending: false,
            harness_agents: BTreeMap::new(),
            release: None,
        }
    }
}

/// One npx verb, with the exact argv this wrapper runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    /// `add <source> [--skill N]… -g -a universal -y`
    Add { source: String, skills: Vec<String> },
    /// `update [names…] -g -y`
    Update { names: Vec<String> },
    /// `remove <names…> -g -y`
    Remove { names: Vec<String> },
}

impl Verb {
    /// The full child argv, `argv[0]` being the resolved npx binary.
    pub fn argv(&self, paths: &Paths) -> Vec<String> {
        let mut argv = vec![
            paths.npx.display().to_string(),
            "-y".to_string(),
            paths.npx_pkg.clone(),
        ];
        match self {
            Self::Add { source, skills } => {
                argv.push("add".to_string());
                argv.push(source.clone());
                for skill in skills {
                    argv.push("--skill".to_string());
                    argv.push(skill.clone());
                }
                argv.push("-g".to_string());
                argv.push("-a".to_string());
                argv.push(UNIVERSAL_AGENT.to_string());
                argv.push("-y".to_string());
            }
            Self::Update { names } => {
                argv.push("update".to_string());
                argv.extend(names.iter().cloned());
                argv.push("-g".to_string());
                argv.push("-y".to_string());
            }
            Self::Remove { names } => {
                argv.push("remove".to_string());
                argv.extend(names.iter().cloned());
                argv.push("-g".to_string());
                argv.push("-y".to_string());
            }
        }
        argv
    }

    /// How a `--dry-run` reports the invocation it did not make: `path` is the
    /// npx binary and `target` the argv tail, as one space-joined string.
    pub fn plan_op(&self, paths: &Paths) -> PlanOp {
        let argv = self.argv(paths);
        PlanOp {
            op: "npx".to_string(),
            path: paths.npx.clone(),
            target: Some(PathBuf::from(argv[1..].join(" "))),
        }
    }
}

/// What a staged run left behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NpxRun {
    /// The full child argv, including the npx binary.
    pub argv: Vec<String>,
    /// The stage the child ran in; the `HOME` it saw.
    pub home: PathBuf,
    pub exit: i32,
    /// The inbox to reconcile: `<stage>/.agents/skills`.
    pub inbox: PathBuf,
    /// The lock npx wrote: `<stage>/.agents/.skill-lock.json`.
    pub lock: PathBuf,
}

/// Remove a path whatever it is: a real directory, a symlink, or a file. The
/// one removal helper: the stage, the scratch dir and variant storage share it.
pub fn remove_path(path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(Error::io(&format!("reading {}", path.display()), &err)),
    };
    let result = if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|err| Error::io(&format!("removing {}", path.display()), &err))
}

/// Copy `from` over `to`; a missing `from` is not an error.
pub fn copy_file_if_exists(from: &Path, to: &Path) -> Result<bool> {
    let bytes = match crate::fsx::read_to_string(from) {
        Ok(text) => text.into_bytes(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(Error::io(&format!("reading {}", from.display()), &err)),
    };
    crate::fsx::atomic_write(to, &bytes)
        .map_err(|err| Error::io(&format!("writing {}", to.display()), &err))?;
    Ok(true)
}

/// Parse the `skills` lock. A missing lock is an empty one.
pub fn read_lock(path: &Path) -> Result<BTreeMap<String, LockEntry>> {
    let text = match crate::fsx::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(err) => {
            return Err(Error::io(
                &format!("reading the lock {}", path.display()),
                &err,
            ));
        }
    };
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|err| {
        Error::internal(format!(
            "the lock {} is not valid JSON: {err}",
            path.display()
        ))
    })?;
    let skills = value
        .get("skills")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    serde_json::from_value(skills).map_err(|err| {
        Error::internal(format!(
            "the lock {} has unreadable skill entries: {err}",
            path.display()
        ))
    })
}

/// Copy the live lock into `library/<lock_snapshot(paths)>`.
///
/// Returns whether the snapshot changed (or, for `dry_run`, would change).
pub fn snapshot_lock(paths: &Paths, dry_run: bool) -> Result<bool> {
    if !std::env::var("SKILLHERD_SNAPSHOT_LOCK").is_ok_and(|value| value == "1") {
        return Ok(false);
    }
    snapshot_lock_enabled(paths, dry_run)
}

fn snapshot_lock_enabled(paths: &Paths, dry_run: bool) -> Result<bool> {
    let live = match crate::fsx::read_to_string(&paths.lock) {
        Ok(text) => text.into_bytes(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(Error::io(
                &format!("reading the lock {}", paths.lock.display()),
                &err,
            ));
        }
    };
    let snapshot = paths.library.join(lock_snapshot(paths));
    if crate::fsx::read_to_string(&snapshot).is_ok_and(|text| text.as_bytes() == live) {
        return Ok(false);
    }
    if dry_run {
        return Ok(true);
    }
    crate::fsx::atomic_write(&snapshot, &live)
        .map_err(|err| Error::io(&format!("writing {}", snapshot.display()), &err))?;
    Ok(true)
}

/// Refuse the `~/.local/bin` shim: it rewrites `npx skills …` into `skillherd …`,
/// so calling it would recurse instead of running upstream npx.
pub fn assert_not_shim(paths: &Paths) -> Result<()> {
    let shim_dir = paths.home.join(".local/bin");
    let unresolved = paths.npx == Path::new("npx")
        && crate::paths::pick_npx_from_path(&std::env::var_os("PATH").unwrap_or_default())
            .is_none();
    if unresolved || paths.npx.starts_with(&shim_dir) || crate::paths::is_npx_wrapper(&paths.npx) {
        return Err(Error::config_error(format!(
            "no usable real npx at {} (missing or a skillherd shim); set SKILLHERD_NPX to the real npx binary",
            paths.npx.display()
        )));
    }
    Ok(())
}

/// The stage inbox: the canonical dir npx writes in the stage.
pub fn stage_inbox(paths: &Paths) -> PathBuf {
    stage_home(paths).join(STAGE_INBOX)
}

/// The lock npx writes in the stage.
pub fn stage_lock(paths: &Paths) -> PathBuf {
    stage_home(paths).join(STAGE_LOCK)
}

/// The stage this thread built, if any.
fn active_stage() -> Option<PathBuf> {
    ACTIVE_STAGE.with(|cell| cell.borrow().clone())
}

/// The active run directory, or the stage root when no run is active: the
/// helpers below address the closest thing that exists rather than inventing a
/// path, and every caller that built a stage gets its own run directory.
fn stage_home(paths: &Paths) -> PathBuf {
    active_stage().unwrap_or_else(|| paths.stage.clone())
}

/// The compact ISO-8601 stamp (`20260921T123456Z`) a run directory is named
/// after: `now_iso8601()` without its separators, since `:` in a path is a
/// needless portability hazard.
fn stamp() -> String {
    crate::model::now_iso8601().replace(['-', ':'], "")
}

/// A fresh nonce for one marker, from the pid, the clock and a counter.
fn nonce() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seed = format!(
        "{} {} {}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        crate::model::now_iso8601()
    );
    crate::hash::sha256_hex(seed.as_bytes())[..32].to_string()
}

/// Whether `text` is a marker this tool wrote.
fn marker_is_owned(text: &str) -> bool {
    let mut parts = text.trim().split(' ');
    parts.next() == Some(STAGE_MARKER_FORMAT)
        && parts.next() == Some(STAGE_OWNER)
        && parts
            .next()
            .is_some_and(|token| token.len() == 32 && token.chars().all(|c| c.is_ascii_hexdigit()))
        && parts.next().is_none()
}

/// Whether `dir` carries this tool's ownership marker.
fn is_owned(dir: &Path) -> bool {
    crate::fsx::open_dir_nofollow(dir).is_ok()
        && crate::fsx::read_to_string(&dir.join(STAGE_MARKER))
            .is_ok_and(|text| marker_is_owned(&text))
}

/// Write the ownership marker into a directory this tool has just created (or
/// adopted while it was empty).
fn mark_owned(dir: &Path) -> Result<()> {
    let path = dir.join(STAGE_MARKER);
    let text = format!("{STAGE_MARKER_FORMAT} {STAGE_OWNER} {}\n", nonce());
    crate::fsx::atomic_write(&path, text.as_bytes())
        .map_err(|err| Error::io(&format!("writing {}", path.display()), &err))
}

pub(crate) fn read_dir_names(dir: &Path) -> Result<Vec<String>> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::io(&format!("reading {}", dir.display()), &err)),
    };
    let mut names = Vec::new();
    for dirent in read {
        let dirent = dirent.map_err(|err| Error::io("reading a directory entry", &err))?;
        names.push(dirent.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
}

/// Refuse a stage root this tool must not touch: HOME itself, a path outside
/// HOME, or an ancestor of a root the tool owns. `SKILLHERD_STAGE` is an operator
/// setting, so every refusal names the path and the way out.
pub fn assert_stage_usable(paths: &Paths) -> Result<()> {
    let stage = &paths.stage;
    if stage == &paths.home {
        return Err(Error::config_error(format!(
            "the stage is HOME itself ({}); set SKILLHERD_STAGE to a scratch directory under HOME (the default is ~/.agents/.skill-stage)",
            stage.display()
        )));
    }
    if !stage.starts_with(&paths.home) {
        return Err(Error::config_error(format!(
            "the stage ({}) lies outside HOME ({}); set SKILLHERD_STAGE to a scratch directory under HOME",
            stage.display(),
            paths.home.display()
        )));
    }
    for (name, root) in [
        ("the library", &paths.library),
        ("the agents dir", &paths.agents_dir),
    ] {
        if root.starts_with(stage) {
            return Err(Error::config_error(format!(
                "the stage ({}) contains {name} ({}); set SKILLHERD_STAGE to a directory that holds neither the library nor the agents dir",
                stage.display(),
                root.display()
            )));
        }
    }
    Ok(())
}

/// The stage root: created and marked by this tool, adopted while empty, or
/// refused untouched.
fn ensure_stage_root(paths: &Paths) -> Result<()> {
    let root = &paths.stage;
    match fs::symlink_metadata(root) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(Error::config_error(format!(
                    "the stage ({}) is not a directory; set SKILLHERD_STAGE to a scratch directory under HOME",
                    root.display()
                )));
            }
            if is_owned(root) {
                return Ok(());
            }
            if read_dir_names(root)?.is_empty() {
                // Not this tool's directory, but it holds nothing to lose.
                return mark_owned(root);
            }
            Err(Error::config_error(format!(
                "the stage ({}) is not empty and carries no {STAGE_MARKER} marker, so this tool did not create it and will not touch it; point SKILLHERD_STAGE at a scratch directory under HOME, or move {} aside",
                root.display(),
                root.display()
            )))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(root)
                .map_err(|err| Error::io(&format!("creating {}", root.display()), &err))?;
            mark_owned(root)
        }
        Err(err) => Err(Error::io(&format!("reading {}", root.display()), &err)),
    }
}

/// Remove the stages a crashed run left behind. Anything under the stage root
/// that is not ours stops the run instead of being deleted.
fn clean_stale_stages(root: &Path) -> Result<()> {
    for name in read_dir_names(root)? {
        if name == STAGE_MARKER {
            continue;
        }
        let path = root.join(&name);
        let meta = fs::symlink_metadata(&path)
            .map_err(|err| Error::io(&format!("reading {}", path.display()), &err))?;
        if meta.is_dir() && is_owned(&path) {
            // An interrupted import's arrival may be the only complete copy.
            // Preserve it for reconcile rather than discarding it on install.
            if read_dir_names(&path.join(STAGE_INBOX))?.is_empty() {
                remove_path(&path)?;
            }
            continue;
        }
        return Err(Error::config_error(format!(
            "the stage root ({}) holds {}, which carries no {STAGE_MARKER} marker; this tool only removes stages it created, so remove that path by hand or point SKILLHERD_STAGE elsewhere",
            root.display(),
            path.display()
        )));
    }
    Ok(())
}

/// Create one run's directory under the stage root and mark it before anything
/// else is written into it.
fn create_run_dir(root: &Path) -> Result<PathBuf> {
    for attempt in 0..1000u32 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let path = root.join(format!(
            "{RUN_PREFIX}{}-{}{suffix}",
            stamp(),
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => {
                mark_owned(&path)?;
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(Error::io(&format!("creating {}", path.display()), &err)),
        }
    }
    Err(Error::config_error(format!(
        "the stage root ({}) holds 1000 run directories from this pid alone; remove them or point SKILLHERD_STAGE elsewhere",
        root.display()
    )))
}

/// Remove one marked run directory, and the stage root with it once the root is
/// ours and holds nothing else.
fn clear_run(root: &Path, container: &Path) -> Result<()> {
    if fs::symlink_metadata(root).is_err() {
        return Ok(());
    }
    if !is_owned(root) {
        return Err(Error::config_error(format!(
            "refusing to remove the stage {}: it carries no {STAGE_MARKER} marker, so this tool did not create it",
            root.display()
        )));
    }
    remove_path(root)?;
    if is_owned(container) {
        let leftovers: Vec<String> = read_dir_names(container)?
            .into_iter()
            .filter(|name| name != STAGE_MARKER)
            .collect();
        if leftovers.is_empty() {
            let _ = fs::remove_file(container.join(STAGE_MARKER));
            let _ = fs::remove_dir(container);
        }
    }
    Ok(())
}

/// Only tool-owned per-run stages are import inboxes. The shared agents
/// surface is never scanned, and a symlink is never evidence of ownership.
pub fn recoverable_inboxes(paths: &Paths) -> Result<Vec<PathBuf>> {
    assert_stage_usable(paths)?;
    if !is_owned(&paths.stage) {
        return Ok(Vec::new());
    }
    let mut inboxes = Vec::new();
    for name in read_dir_names(&paths.stage)? {
        let root = paths.stage.join(&name);
        if name.starts_with(RUN_PREFIX) && is_owned(&root) {
            let inbox = root.join(STAGE_INBOX);
            if fs::symlink_metadata(&inbox).is_ok_and(|meta| meta.is_dir()) {
                inboxes.push(inbox);
            }
        }
    }
    inboxes.sort();
    Ok(inboxes)
}

/// Remove consumed stages only; unconsumed arrivals survive subsequent runs.
pub fn clear_consumed_stages(paths: &Paths) -> Result<()> {
    for inbox in recoverable_inboxes(paths)? {
        if read_dir_names(&inbox)?.is_empty() {
            let root = inbox
                .parent()
                .and_then(Path::parent)
                .expect("stage inbox layout");
            clear_run(root, &paths.stage)?;
        }
    }
    Ok(())
}

/// Build a fresh stage with isolated npm configuration and no credential links.
pub fn prepare_stage(paths: &Paths) -> Result<Stage> {
    assert_stage_usable(paths)?;
    ensure_stage_root(paths)?;
    clean_stale_stages(&paths.stage)?;
    let stage = Stage {
        root: create_run_dir(&paths.stage)?,
    };
    ACTIVE_STAGE.with(|cell| *cell.borrow_mut() = Some(stage.root.clone()));

    let inbox = stage.inbox();
    fs::create_dir_all(&inbox)
        .map_err(|err| Error::io(&format!("creating {}", inbox.display()), &err))?;
    copy_file_if_exists(&paths.lock, &stage.lock())?;
    fs::create_dir(stage.home().join("work"))
        .map_err(|err| Error::io("creating the isolated npx working directory", &err))?;
    // npm refuses to load one file as both user and global config, so each
    // role gets its own controlled file.
    for name in ["npmrc", "npmrc-global"] {
        crate::fsx::atomic_write(
            &stage.home().join(name),
            b"# Controlled config: registry/proxy/CA overrides come from the operator environment.\n",
        )
        .map_err(|err| Error::io("writing the isolated npm configuration", &err))?;
    }
    Ok(stage)
}

/// Discard the stage this thread built once its lock has been moved into the
/// live lock.
pub fn clear_stage(paths: &Paths) -> Result<()> {
    let active = ACTIVE_STAGE.with(|cell| cell.borrow_mut().take());
    match active {
        Some(root) => clear_run(&root, &paths.stage),
        None => Ok(()),
    }
}

/// Shared diagnostic redaction for subprocess argv, stderr and HTTP bodies.
/// This is best-effort logging hygiene, not a sandbox or a secret detector.
pub fn redact(text: &str) -> String {
    let mut redacted = text.to_string();
    // Strip URL userinfo even when it is not one of the known token values.
    let mut cursor = 0;
    while let Some(found) = redacted[cursor..].find("://") {
        let start = cursor + found + 3;
        let end = redacted[start..]
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '/' | '?' | '#' | '\'' | '"'))
            .map_or(redacted.len(), |offset| start + offset);
        if let Some(at) = redacted[start..end].rfind('@') {
            redacted.replace_range(start..start + at, "[REDACTED]");
            cursor = start + "[REDACTED]@".len();
        } else {
            cursor = end;
        }
    }
    let mut secrets: Vec<String> = CREDENTIAL_ENV
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .filter(|value| !value.is_empty())
        .collect();
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    for secret in secrets {
        redacted = redacted.replace(&secret, "[REDACTED]");
    }
    redacted
}

fn output_tail(text: &str) -> String {
    // Redact before truncating so a boundary cannot expose half a token.
    let tail = redact(text);
    let tail = tail.trim();
    if tail.len() <= OUTPUT_TAIL_BYTES {
        return tail.to_string();
    }
    let mut cut = tail.len() - OUTPUT_TAIL_BYTES;
    while !tail.is_char_boundary(cut) {
        cut += 1;
    }
    match tail[cut..].find('\n') {
        Some(next) => format!("…\n{}", &tail[cut + next + 1..]),
        None => "…\n[output line exceeded diagnostic limit]".to_string(),
    }
}

/// Network settings are operator input, not untrusted project npm config.
const NETWORK_ENV: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "GIT_SSL_CAINFO",
    "npm_config_registry",
    "npm_config_proxy",
    "npm_config_https_proxy",
    "npm_config_cafile",
    "npm_config_strict_ssl",
    "NPM_CONFIG_REGISTRY",
    "NPM_CONFIG_PROXY",
    "NPM_CONFIG_HTTPS_PROXY",
    "NPM_CONFIG_CAFILE",
    "NPM_CONFIG_STRICT_SSL",
];

const CREDENTIAL_ENV: &[&str] = &[
    "SSH_AUTH_SOCK",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NPM_TOKEN",
    "NODE_AUTH_TOKEN",
];

fn npm_cache(paths: &Paths) -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| paths.home.join(".cache"))
        .join("skillherd/npm")
}

/// Drain one diagnostic stream, optionally forwarding redacted lines live.
fn capture_stream(
    reader: impl std::io::Read,
    mut live: Option<impl std::io::Write>,
) -> std::io::Result<String> {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(reader);
    let mut line = Vec::new();
    let mut text = String::new();
    while reader.read_until(b'\n', &mut line)? != 0 {
        // Redact complete lines, so a read boundary cannot expose half a token.
        let safe = redact(&String::from_utf8_lossy(&line));
        if let Some(writer) = &mut live {
            // Keep draining the child even if the terminal goes away.
            let _ = writer
                .write_all(safe.as_bytes())
                .and_then(|()| writer.flush());
        }
        text.push_str(&safe);
        line.clear();
    }
    Ok(text)
}

fn capture_output(command: &mut Command, live: bool) -> std::io::Result<(i32, String)> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("piped child stdout");
    let stderr = child.stderr.take().expect("piped child stderr");
    std::thread::scope(|scope| {
        let stdout = scope.spawn(|| capture_stream(stdout, live.then(std::io::stderr)));
        let stderr = scope.spawn(|| capture_stream(stderr, live.then(std::io::stderr)));
        let status = child.wait()?;
        let mut text = stdout
            .join()
            .map_err(|_| std::io::Error::other("stdout reader panicked"))??;
        text.push_str(
            &stderr
                .join()
                .map_err(|_| std::io::Error::other("stderr reader panicked"))??,
        );
        Ok((status.code().unwrap_or(128), text))
    })
}

/// Run one verb in the stage. The child's stdout and stderr are diagnostics.
pub fn run(paths: &Paths, verb: &Verb) -> Result<NpxRun> {
    assert_not_shim(paths)?;
    assert_stage_usable(paths)?;
    let argv = verb.argv(paths);
    let home = active_stage().ok_or_else(|| {
        Error::config_error(format!(
            "no stage is active under {}; a run needs a stage built by prepare_stage (set SKILLHERD_STAGE to a scratch directory under HOME)",
            paths.stage.display()
        ))
    })?;

    // npx is `#!/usr/bin/env node`: its own directory leads PATH so the node
    // beside it is found, not a version manager's shim that cannot resolve a
    // version once HOME is the stage.
    let mut path_env = std::ffi::OsString::new();
    if let Some(dir) = paths.npx.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        path_env.push(dir);
        path_env.push(":");
    }
    path_env.push(std::env::var_os("PATH").unwrap_or_default());

    let cache = npm_cache(paths);
    crate::fsx::trust_root(cache.parent().and_then(Path::parent).expect("cache layout"))
        .and_then(|()| crate::fsx::create_dir_all_nofollow(&cache).map(|_| ()))
        .map_err(|err| Error::io("preparing persistent npm cache", &err))?;
    let credentials = std::env::var("SKILLHERD_NPX_CREDENTIALS").is_ok_and(|value| value == "1");
    let userconfig = if credentials {
        paths.home.join(".npmrc")
    } else {
        home.join("npmrc")
    };
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .env_clear()
        .current_dir(home.join("work"))
        .env("HOME", &home)
        .env("PATH", path_env)
        .env("npm_config_cache", &cache)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("npm_config_userconfig", userconfig)
        .env("npm_config_globalconfig", home.join("npmrc-global"))
        .env("CI", "1");
    for key in ["LANG", "LC_ALL", "TERM", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    for key in NETWORK_ENV {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    if credentials {
        for key in CREDENTIAL_ENV {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command.env("GIT_CONFIG_GLOBAL", paths.home.join(".gitconfig"));
    }
    if !std::env::var("SKILLHERD_NPX_TELEMETRY").is_ok_and(|value| value == "1") {
        command
            .env("DISABLE_TELEMETRY", "1")
            .env("DO_NOT_TRACK", "1");
    }
    use std::io::IsTerminal;
    let (exit, tail) =
        capture_output(&mut command, std::io::stderr().is_terminal()).map_err(|err| {
            Error::npx_failed(
                &argv,
                127,
                &format!("failed to run {}: {err}", paths.npx.display()),
            )
        })?;

    let run = NpxRun {
        argv,
        home,
        exit,
        inbox: stage_inbox(paths),
        lock: stage_lock(paths),
    };
    if exit != 0 {
        return Err(Error::npx_failed(&run.argv, exit, &output_tail(&tail)));
    }
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_diagnostics_stream_before_eof_and_are_still_captured() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let observed = Rc::new(RefCell::new(Vec::<u8>::new()));
        struct Live(Rc<RefCell<Vec<u8>>>);
        impl std::io::Write for Live {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        struct Diagnostic {
            source: std::io::Cursor<&'static [u8]>,
            observed: Rc<RefCell<Vec<u8>>>,
        }
        impl std::io::Read for Diagnostic {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.source.position() as usize == self.source.get_ref().len() {
                    assert!(
                        !self.observed.borrow().is_empty(),
                        "live progress must precede EOF"
                    );
                }
                self.source.read(buffer)
            }
        }
        let captured = capture_stream(
            Diagnostic {
                source: std::io::Cursor::new(b"progress https://user:password@example.invalid\n"),
                observed: observed.clone(),
            },
            Some(Live(observed.clone())),
        )
        .unwrap();
        let live = String::from_utf8(observed.borrow().clone()).unwrap();
        assert!(live.contains("progress"));
        assert!(!live.contains("user:password"));
        assert_eq!(captured, live);
    }

    #[test]
    fn failure_diagnostics_redact_argv_and_name_the_credential_opt_in() {
        let argv = vec![
            "npx".into(),
            "https://user:password@example.invalid/private".into(),
        ];
        let error = Error::npx_failed(&argv, 1, "Permission denied (publickey)");
        let details = error.details().to_string();
        assert!(!details.contains("user:password"), "{details}");
        assert!(
            error
                .suggested_fix()
                .unwrap()
                .contains("SKILLHERD_NPX_CREDENTIALS=1")
        );
        for reason in [
            "could not read Username",
            "Authentication failed",
            "Repository not found",
            "HTTP 401",
            "HTTP 403",
        ] {
            let error = Error::npx_failed(&argv, 1, reason);
            assert!(
                error
                    .suggested_fix()
                    .unwrap()
                    .contains("SKILLHERD_NPX_CREDENTIALS=1"),
                "{reason}"
            );
        }
    }

    #[test]
    fn output_tails_start_on_a_complete_line() {
        let text = format!("{}\nlast complete line", "x".repeat(5000));
        assert_eq!(output_tail(&text), "…\nlast complete line");
    }

    #[test]
    fn the_verbs_build_the_documented_argv() {
        let paths = Paths::fixture("/sandbox/user");
        assert_eq!(
            Verb::Add {
                source: "owner/repo".into(),
                skills: vec!["post".into()],
            }
            .argv(&paths),
            vec![
                "/opt/homebrew/bin/npx",
                "-y",
                "skills@1.7.0",
                "add",
                "owner/repo",
                "--skill",
                "post",
                "-g",
                "-a",
                "universal",
                "-y",
            ]
        );
        assert_eq!(
            Verb::Update {
                names: vec!["post".into()],
            }
            .argv(&paths),
            vec![
                "/opt/homebrew/bin/npx",
                "-y",
                "skills@1.7.0",
                "update",
                "post",
                "-g",
                "-y",
            ]
        );
        assert_eq!(
            Verb::Remove {
                names: vec!["post".into()],
            }
            .argv(&paths),
            vec![
                "/opt/homebrew/bin/npx",
                "-y",
                "skills@1.7.0",
                "remove",
                "post",
                "-g",
                "-y",
            ]
        );
        let op = Verb::Update { names: vec![] }.plan_op(&paths);
        assert_eq!(op.op, "npx");
        assert_eq!(op.path, paths.npx);
        assert_eq!(
            op.target.as_deref().unwrap().to_string_lossy(),
            "-y skills@1.7.0 update -g -y"
        );
    }

    #[test]
    fn the_lock_snapshot_is_named_for_the_platform() {
        let mut paths = Paths::fixture("/sandbox/user");
        paths.platform = crate::model::Platform::Mac;
        assert_eq!(lock_snapshot(&paths), ".npx/skill-lock.mac.json");
        paths.platform = crate::model::Platform::Linux;
        assert_eq!(lock_snapshot(&paths), ".npx/skill-lock.linux.json");
    }

    #[test]
    fn the_shim_is_refused() {
        let mut paths = Paths::fixture("/sandbox/user");
        paths.npx = paths.home.join(".local/bin/npx");
        let err = assert_not_shim(&paths).unwrap_err();
        assert_eq!(err.code(), "config_error");
        assert!(err.to_string().contains(".local/bin"), "{err}");
        paths.npx = PathBuf::from("/opt/homebrew/bin/npx");
        assert!(assert_not_shim(&paths).is_ok());
    }

    #[test]
    fn a_missing_lock_reads_as_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        crate::fsx::trust_root(dir.path()).unwrap(); // Operator-chosen test root.
        let lock = read_lock(&dir.path().join("nope.json")).unwrap();
        assert!(lock.is_empty());
        let err = read_lock(dir.path()).unwrap_err();
        assert_eq!(err.code(), "internal");
    }

    #[test]
    fn lock_entries_carry_the_documented_keys() {
        let dir = tempfile::TempDir::new().unwrap();
        crate::fsx::trust_root(dir.path()).unwrap(); // Operator-chosen test root.
        let path = dir.path().join("lock.json");
        fs::write(
            &path,
            r#"{"version":3,"skills":{"post":{"source":"s","sourceType":"github",
                "sourceUrl":"https://example.invalid/x.git","skillPath":".agents/skills/post/SKILL.md",
                "skillFolderHash":"abc","updatedAt":"2026-01-01T00:00:00Z"}}}"#,
        )
        .unwrap();
        let lock = read_lock(&path).unwrap();
        let entry = &lock["post"];
        assert_eq!(entry.source_type, "github");
        assert_eq!(entry.skill_path, ".agents/skills/post/SKILL.md");
        assert_eq!(entry.r#ref, None);
        let meta = entry.to_meta("hash".into(), "2026-09-21T00:00:00Z".into());
        assert_eq!(meta.imported_hash, "hash");
        assert!(meta.variants.is_empty());
        assert!(!meta.variants_pending);
    }

    #[test]
    fn the_snapshot_tracks_the_live_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        fs::create_dir_all(paths.lock.parent().unwrap()).unwrap();
        fs::write(&paths.lock, b"{\"version\":3,\"skills\":{}}").unwrap();
        assert!(
            snapshot_lock_enabled(&paths, true).unwrap(),
            "no snapshot exists yet, so the first run writes one"
        );
        assert!(!paths.library.join(lock_snapshot(&paths)).exists());
        assert!(snapshot_lock_enabled(&paths, false).unwrap());
        assert!(paths.library.join(lock_snapshot(&paths)).is_file());
        assert!(!snapshot_lock_enabled(&paths, false).unwrap(), "unchanged");
        fs::write(&paths.lock, b"{\"version\":3,\"skills\":{\"a\":{}}}").unwrap();
        assert!(
            snapshot_lock_enabled(&paths, true).unwrap(),
            "a dry run reports it"
        );
        assert_ne!(
            fs::read(paths.library.join(lock_snapshot(&paths))).unwrap(),
            fs::read(&paths.lock).unwrap(),
            "a dry run writes nothing"
        );
    }

    #[test]
    fn the_stage_is_a_fresh_marked_run_under_the_stage_root() {
        let dir = tempfile::TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        fs::create_dir_all(paths.lock.parent().unwrap()).unwrap();
        fs::write(&paths.lock, b"{\"version\":3,\"skills\":{}}").unwrap();
        fs::create_dir_all(paths.home.join(".ssh")).unwrap();
        fs::write(paths.home.join(".gitconfig"), b"[user]\n").unwrap();

        let first = prepare_stage(&paths).unwrap();
        assert!(
            first.home().starts_with(&paths.stage),
            "{} is under the stage root",
            first.home().display()
        );
        assert!(
            first
                .home()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(RUN_PREFIX),
            "{:?}",
            first.home()
        );
        assert!(is_owned(first.home()), "the run directory is marked");
        assert!(is_owned(&paths.stage), "the stage root is marked");
        assert!(stage_inbox(&paths).is_dir());
        assert!(stage_lock(&paths).is_file());
        assert!(!first.home().join(".ssh").exists());
        assert!(!first.home().join(".gitconfig").exists());

        // A run that never cleared its stage is a stale stage: the next run
        // removes it, marker and all.
        let leftover = first.home().join("crash-leftover");
        fs::write(&leftover, b"x").unwrap();
        let second = prepare_stage(&paths).unwrap();
        assert!(is_owned(second.home()), "the new run is marked");
        assert!(!leftover.exists(), "the stale run's tree is gone");
        assert!(stage_inbox(&paths).is_dir(), "and the new inbox is fresh");
        assert_eq!(
            read_dir_names(&paths.stage)
                .unwrap()
                .into_iter()
                .filter(|name| name != STAGE_MARKER)
                .count(),
            1,
            "exactly one run directory is left"
        );

        second.clear().unwrap();
        assert!(!paths.stage.exists(), "the empty stage root goes too");
    }

    #[test]
    fn a_stage_root_this_tool_did_not_create_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        fs::create_dir_all(&paths.stage).unwrap();
        let keep = paths.stage.join("notes.md");
        fs::write(&keep, b"operator data\n").unwrap();

        let err = prepare_stage(&paths).unwrap_err();
        assert_eq!(err.code(), "config_error");
        assert_eq!(err.exit_code(), 78);
        assert!(err.to_string().contains(STAGE_MARKER), "{err}");
        assert_eq!(fs::read(&keep).unwrap(), b"operator data\n");
        assert!(paths.stage.is_dir(), "the directory is still there");

        // A file where the stage root goes is refused as well.
        fs::remove_file(&keep).unwrap();
        fs::remove_dir(&paths.stage).unwrap();
        fs::write(&paths.stage, b"not a directory\n").unwrap();
        let err = prepare_stage(&paths).unwrap_err();
        assert_eq!(err.code(), "config_error");
        assert!(err.to_string().contains("is not a directory"), "{err}");
        assert_eq!(fs::read(&paths.stage).unwrap(), b"not a directory\n");
    }

    #[test]
    fn an_empty_stage_root_is_adopted() {
        let dir = tempfile::TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        fs::create_dir_all(&paths.stage).unwrap();
        let stage = prepare_stage(&paths).unwrap();
        assert!(is_owned(&paths.stage), "an empty root is marked and used");
        stage.clear().unwrap();
        assert!(!paths.stage.exists());
    }

    #[test]
    fn an_unowned_path_inside_the_stage_root_stops_the_run() {
        let dir = tempfile::TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        fs::create_dir_all(&paths.stage).unwrap();
        mark_owned(&paths.stage).unwrap();
        let foreign = paths.stage.join("run-20250101T000000Z-1");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("SKILL.md"), b"hand-made\n").unwrap();

        let err = prepare_stage(&paths).unwrap_err();
        assert_eq!(err.code(), "config_error");
        assert!(
            err.to_string().contains(foreign.to_string_lossy().as_ref()),
            "{err}"
        );
        assert_eq!(fs::read(foreign.join("SKILL.md")).unwrap(), b"hand-made\n");
    }

    #[test]
    fn the_stage_root_is_refused_when_it_is_home_or_outside_it() {
        let base = Paths::fixture("/sandbox/user");
        assert!(assert_stage_usable(&base).is_ok());

        let mut paths = base.clone();
        paths.stage = paths.home.clone();
        let err = assert_stage_usable(&paths).unwrap_err();
        assert_eq!(err.code(), "config_error");
        assert!(err.to_string().contains("HOME itself"), "{err}");

        let mut paths = base.clone();
        paths.stage = PathBuf::from("/tmp/skill-stage");
        let err = assert_stage_usable(&paths).unwrap_err();
        assert!(err.to_string().contains("outside HOME"), "{err}");

        let mut paths = base.clone();
        paths.stage = paths.home.join(".agents");
        let err = assert_stage_usable(&paths).unwrap_err();
        assert_eq!(err.code(), "config_error");
        assert!(err.to_string().contains(".agents"), "{err}");
        assert!(err.to_string().contains("contains"), "{err}");

        let mut paths = base.clone();
        paths.stage = paths.home.parent().unwrap().to_path_buf();
        let err = assert_stage_usable(&paths).unwrap_err();
        assert!(err.to_string().contains("outside HOME"), "{err}");
    }
}
