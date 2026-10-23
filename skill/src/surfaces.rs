//! Per-surface planning and applying on top of the managed-entry rule.
//!
//! The rule itself lives in `managed.rs` and is implemented once; this module
//! turns a harness row plus its desired set into (a) the plan of operations a
//! surface needs and (b) the skeleton of the report the renderer returns.
//!
//! Each desired name ends up served one of two ways, decided at plan time by
//! the stripper (`strip.rs`), which only reads:
//!
//! * a **link** into the library when its source carries no fence marker;
//! * a **served copy** — a real directory holding the stripped tree and a
//!   `.skill-render.json` record — when it does.
//!
//! The ops are `link` (write a link, replacing a managed entry of ours),
//! `render` (write a served copy, likewise) and `remove`. Plan time records
//! the identity of every entry an op will replace or remove; [`apply_ops`]
//! re-proves that identity immediately before it acts and writes with
//! no-replace renames, so an entry another process swapped in while the plan
//! was being carried out is refused instead of deleted or overwritten.
//! Foreign entries are listed and otherwise ignored: never linked over, never
//! removed, never stripped.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};
use crate::fsx;
use crate::managed::{self, Expected, Proven, Removal};
use crate::model::{HarnessRow, PlanOp, Refusal, StripperReport, now_iso8601};
use crate::names::validate_name;
use crate::output;
use crate::paths::{Paths, normalize};
use crate::render::SurfaceReport;
use crate::strip::{self, CopyItem, Prepared, RENDER_VERSION, ServedCopy};

pub use crate::managed::{
    Entry, EntryKind, RENDER_MARKER, RenderMarker, has_marker, is_managed, list_entries,
    read_marker,
};

/// The directory a harness row serves for one name: the harness variant when
/// the row names one and it exists inside the skill, else the universal copy.
///
/// A variant that resolves outside its skill (a link to another directory, or
/// `../..`, the library itself) is never served: it would make an outside tree
/// the source of a link or a served copy. The universal copy is served instead
/// and a `variant_outside_skill` warning says why.
pub fn source_for(row: &HarnessRow, name: &str, paths: &Paths) -> Result<PathBuf> {
    let base = paths.library.join(name);
    if let Some(key) = row.variant_key.as_deref() {
        validate_name(key)?;
        match variant_tree(&base, key) {
            VariantTree::Inside(variant) => return Ok(variant),
            VariantTree::Outside(real) => output::warn(
                "variant_outside_skill",
                format!(
                    "{name}/.variants/{key} resolves to {}, outside the skill; {} serves the universal copy of {name} instead",
                    real.display(),
                    row.name
                ),
            ),
            VariantTree::Absent => {}
        }
    }
    Ok(base)
}

/// Where a skill's `.variants/<key>` tree stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariantTree {
    /// No such directory.
    Absent,
    /// A directory strictly inside the skill's resolved tree: servable. The
    /// path is the lexical one under the skill.
    Inside(PathBuf),
    /// A directory that resolves outside the skill, or to the skill itself or
    /// one of its ancestors: never served. The path is where it resolves.
    Outside(PathBuf),
}

/// Judge `skill/.variants/<key>` by where it resolves, not by its name.
pub fn variant_tree(skill: &Path, key: &str) -> VariantTree {
    let variant = skill.join(".variants").join(key);
    if !variant.is_dir() {
        return VariantTree::Absent;
    }
    let (Ok(real_skill), Ok(real_variant)) = (fs::canonicalize(skill), fs::canonicalize(&variant))
    else {
        return VariantTree::Absent;
    };
    if real_variant != real_skill && real_variant.starts_with(&real_skill) {
        VariantTree::Inside(variant)
    } else {
        VariantTree::Outside(real_variant)
    }
}

/// The stripped copy a `render` op writes, and the record it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyPlan {
    pub copy: ServedCopy,
    pub marker: RenderMarker,
}

/// One planned mutation plus the identity it must still see when it runs.
///
/// A plan's managed/foreign decision is a claim about the moment it was made:
/// `apply_ops` re-proves the identity carried here immediately before it removes
/// or replaces a path, so an entry another process swapped in between planning
/// and applying is refused rather than deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedOp {
    pub op: PlanOp,
    pub expect: Expected,
    /// What a `render` op writes; `None` for `link` and `remove`.
    pub copy: Option<CopyPlan>,
}

/// One surface's plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfacePlan {
    pub ops: Vec<PlannedOp>,
    pub report: SurfaceReport,
    /// Malformed sources, as `(skill name, "<file>: line N: <reason>")`. A plan
    /// that carries errors must not be applied: nothing of a run that found
    /// one is written.
    pub errors: Vec<(String, String)>,
}

/// Plan one surface: what to link or render, what to remove, what is already
/// right. Nothing is written.
pub fn plan_surface(row: &HarnessRow, desired: &[String], paths: &Paths) -> Result<SurfacePlan> {
    plan(row, desired, paths, false)
}

/// The restricted form: only the named skills are planned, so the entries the
/// run does not name are not swept away either (`render --only`, `add`).
pub fn plan_surface_restricted(
    row: &HarnessRow,
    desired: &[String],
    paths: &Paths,
) -> Result<SurfacePlan> {
    plan(row, desired, paths, true)
}

/// How one desired name should be served.
enum Want {
    Link,
    Copy(ServedCopy),
}

fn plan(
    row: &HarnessRow,
    desired: &[String],
    paths: &Paths,
    restrict: bool,
) -> Result<SurfacePlan> {
    let surface = &row.surface;
    let harness = row.variant_key.as_deref();
    let mut report = SurfaceReport {
        surface: row.name.clone(),
        linked: Vec::new(),
        removed: Vec::new(),
        refreshed: Vec::new(),
        unchanged: Vec::new(),
        foreign: Vec::new(),
        refused: Vec::new(),
        stripper: None,
    };
    let mut stripper = StripperReport {
        rendered: Vec::new(),
        refreshed: Vec::new(),
        unchanged: Vec::new(),
        errors: Vec::new(),
    };
    let mut errors: Vec<(String, String)> = Vec::new();

    let entries = list_entries(surface, paths)?;
    let mut by_name: BTreeMap<&str, &Entry> = BTreeMap::new();
    for entry in &entries {
        if !entry.managed {
            report.foreign.push(entry.clone());
        }
        by_name.insert(entry.name.as_str(), entry);
    }

    let mut desired: Vec<&String> = desired.iter().collect();
    desired.sort();
    desired.dedup();
    for name in &desired {
        validate_name(name)?;
    }

    let mut ops: Vec<PlannedOp> = Vec::new();
    for name in &desired {
        let name = name.as_str();
        let source = source_for(row, name, paths)?;
        if !source.is_dir() {
            output::warn(
                "library_entry_missing",
                format!(
                    "{name} is in the desired set of {} but {} is not a directory; nothing is served for it and its current entry, if any, is left alone",
                    row.name,
                    source.display()
                ),
            );
            continue;
        }
        let existing = by_name.get(name).copied();
        // A foreign entry keeps the name: reported, counted nowhere else, and
        // its source is not even read.
        if existing.is_some_and(|entry| !entry.managed) {
            continue;
        }
        let skill = paths.library.join(name);
        let want = match strip::prepare(&source, &skill, &paths.library, paths.platform, harness)? {
            Ok(Prepared::Plain) => Want::Link,
            Ok(Prepared::Fenced(copy)) => Want::Copy(copy),
            Err(file_errors) => {
                for error in file_errors {
                    errors.push((name.to_string(), error.to_string()));
                }
                continue;
            }
        };

        let link = surface.join(name);
        let text = link_text(surface, &source, &paths.home);
        let served_here = existing.is_some_and(|entry| entry_is_satisfied(entry, &source));
        let expect = match existing {
            Some(entry) => planned_identity(entry)?,
            None => Expected::Absent,
        };
        match want {
            Want::Link => {
                if served_here && existing.is_some_and(Entry::is_symlink) {
                    report.unchanged.push(name.to_string());
                    stripper.unchanged.push(name.to_string());
                    continue;
                }
                let reverts =
                    served_here && existing.is_some_and(|entry| entry.kind == EntryKind::Dir);
                ops.push(PlannedOp {
                    op: PlanOp {
                        op: "link".to_string(),
                        path: link,
                        target: Some(text),
                    },
                    expect,
                    copy: None,
                });
                if reverts {
                    // A served copy whose source stopped carrying markers goes
                    // back to a plain link.
                    report.refreshed.push(name.to_string());
                    stripper.refreshed.push(name.to_string());
                } else {
                    report.linked.push(name.to_string());
                    stripper.unchanged.push(name.to_string());
                }
            }
            Want::Copy(copy) => {
                let current = existing
                    .filter(|entry| served_here && entry.kind == EntryKind::Dir)
                    .and_then(|entry| read_marker(&entry.path))
                    .is_some_and(|marker| {
                        marker.source_hash == copy.source_hash
                            && marker.platform == paths.platform
                            && marker.harness.as_deref() == harness
                            && marker.render_version == Some(RENDER_VERSION)
                    });
                if current {
                    report.unchanged.push(name.to_string());
                    stripper.unchanged.push(name.to_string());
                    continue;
                }
                for skipped in &copy.skipped {
                    output::warn(
                        "served_copy_entry_skipped",
                        format!(
                            "{name}/{} is left out of the served copy on {}: {}",
                            skipped.rel.display(),
                            row.name,
                            skipped.reason
                        ),
                    );
                }
                let marker = RenderMarker {
                    name: name.to_string(),
                    platform: paths.platform,
                    harness: harness.map(str::to_string),
                    render_version: Some(RENDER_VERSION),
                    source_dir: fs::canonicalize(&source).unwrap_or_else(|_| source.clone()),
                    link_target: text,
                    source_hash: copy.source_hash.clone(),
                    rendered_at: None,
                };
                let refresh = served_here;
                ops.push(PlannedOp {
                    op: PlanOp {
                        op: "render".to_string(),
                        path: link,
                        target: Some(source.clone()),
                    },
                    expect,
                    copy: Some(CopyPlan { copy, marker }),
                });
                let had_copy = existing.is_some_and(|entry| entry.kind == EntryKind::Dir);
                if refresh {
                    report.refreshed.push(name.to_string());
                } else {
                    report.linked.push(name.to_string());
                }
                if refresh && had_copy {
                    stripper.refreshed.push(name.to_string());
                } else {
                    stripper.rendered.push(name.to_string());
                }
            }
        }
    }

    if !restrict {
        for entry in &entries {
            if entry.managed && !desired.iter().any(|name| name.as_str() == entry.name) {
                ops.push(PlannedOp {
                    op: PlanOp {
                        op: "remove".to_string(),
                        path: entry.path.clone(),
                        target: None,
                    },
                    expect: planned_identity(entry)?,
                    copy: None,
                });
                report.removed.push(entry.name.clone());
            }
        }
    }

    let served = stripper.rendered.len() + stripper.refreshed.len() + stripper.unchanged.len();
    if served > 0 {
        for list in [
            &mut stripper.rendered,
            &mut stripper.refreshed,
            &mut stripper.unchanged,
        ] {
            list.sort();
        }
        report.stripper = Some(stripper);
    }
    report.refreshed.sort();
    Ok(SurfacePlan {
        ops,
        report,
        errors,
    })
}

/// The identity a plan carries for an entry: only a managed entry is ever
/// planned for, so a plain file here is a bug, not a refusal.
fn planned_identity(entry: &Entry) -> Result<Expected> {
    Expected::of(entry).ok_or_else(|| {
        Error::internal(format!(
            "{} is a plain file; nothing of ours is there to plan for",
            entry.path.display()
        ))
    })
}

/// True when the surface already serves `source` under this entry, as a link
/// or as a served copy (fresh or not).
fn entry_is_satisfied(entry: &Entry, source: &Path) -> bool {
    match &entry.kind {
        EntryKind::Symlink { target, .. } => normalize(target) == normalize(source),
        EntryKind::Dir => {
            read_marker(&entry.path).is_some_and(|marker| resolves_to(&marker.source_dir, source))
        }
        EntryKind::File => false,
    }
}

/// Compare a recorded source with the desired one after resolving symlinks, so
/// a record made through `/var` matches a path given as `/private/var`.
fn resolves_to(recorded: &Path, desired: &Path) -> bool {
    let left = fs::canonicalize(recorded).unwrap_or_else(|_| normalize(recorded));
    let right = fs::canonicalize(desired).unwrap_or_else(|_| normalize(desired));
    left == right
}

/// The link text to write at `dir` for `source`: relative when both live under
/// `home`, absolute otherwise. A relative link survives moving HOME as a whole.
pub fn link_text(dir: &Path, source: &Path, home: &Path) -> PathBuf {
    if dir.starts_with(home)
        && source.starts_with(home)
        && let Some(relative) = relative_path(dir, source)
    {
        return relative;
    }
    source.to_path_buf()
}

/// `to` as a path relative to the directory `from_dir`, purely lexically.
fn relative_path(from_dir: &Path, to: &Path) -> Option<PathBuf> {
    if !from_dir.is_absolute() || !to.is_absolute() {
        return None;
    }
    let from: Vec<Component> = from_dir.components().collect();
    let target: Vec<Component> = to.components().collect();
    let common = from
        .iter()
        .zip(target.iter())
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for part in &target[common..] {
        out.push(part.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    Some(out)
}

/// Apply a plan. The surface directory is created when the plan writes into it.
///
/// Every destructive step re-proves the entry against the identity the plan
/// recorded, immediately before it acts: a path that holds something else now —
/// a real user directory where a managed link was planned — is refused, reported
/// in the returned refusals, and left exactly as it is. New entries land with a
/// no-replace rename or `symlink(2)`, neither of which overwrites anything that
/// appeared in between. The rest of the plan still runs.
pub fn apply_ops(surface: &Path, ops: &[PlannedOp], paths: &Paths) -> Result<Vec<Refusal>> {
    if ops.iter().any(|planned| planned.op.op != "remove") {
        fs::create_dir_all(surface)
            .map_err(|err| Error::io(&format!("creating surface {}", surface.display()), &err))?;
    }
    let mut refusals = Vec::new();
    for planned in ops {
        let op = &planned.op;
        let proven = managed::verify_expected(&op.path, paths, &planned.expect)?;
        let outcome = match op.op.as_str() {
            "link" => apply_link(planned, proven, paths)?,
            "render" => match proven {
                Proven::Foreign { reason } => Some(reason),
                _ => apply_render(surface, planned, paths)?,
            },
            "remove" => match proven {
                Proven::Nothing => None,
                Proven::Matches(entry) => match managed::remove_entry(&entry, paths)? {
                    Removal::Removed => None,
                    Removal::Refused(reason) => Some(reason),
                },
                Proven::Foreign { reason } => Some(reason),
            },
            other => {
                return Err(Error::internal(format!(
                    "unknown render op {other:?} for {}",
                    op.path.display()
                )));
            }
        };
        if let Some(reason) = outcome {
            refusals.push(refuse(&op.path, reason));
        }
    }
    Ok(refusals)
}

/// Clear the way for a new entry: nothing there, or the managed entry the plan
/// saw, removed. `Some(reason)` is a refusal.
fn clear(proven: Proven, paths: &Paths) -> Result<Option<String>> {
    Ok(match proven {
        Proven::Nothing => None,
        Proven::Matches(entry) => match managed::remove_entry(&entry, paths)? {
            Removal::Removed => None,
            Removal::Refused(reason) => Some(reason),
        },
        Proven::Foreign { reason } => Some(reason),
    })
}

fn apply_link(planned: &PlannedOp, proven: Proven, paths: &Paths) -> Result<Option<String>> {
    let op = &planned.op;
    let target = op.target.as_ref().ok_or_else(|| {
        Error::internal(format!("link op without a target: {}", op.path.display()))
    })?;
    if let Some(reason) = clear(proven, paths)? {
        return Ok(Some(reason));
    }
    match std::os::unix::fs::symlink(target, &op.path) {
        Ok(()) => Ok(None),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(Some(format!(
            "{} was taken by something else before the link could be written; it is untouched",
            op.path.display()
        ))),
        Err(err) => Err(Error::io(
            &format!("linking {} -> {}", op.path.display(), target.display()),
            &err,
        )),
    }
}

/// Write a served copy: build it in a fresh hidden directory of the surface,
/// re-prove the path, clear the way, then land the copy with a no-replace
/// rename.
///
/// The re-proof runs after the (slow) staging, immediately before anything is
/// removed. A served copy that is being refreshed is moved aside and re-read
/// by `managed::remove_entry` before it is deleted, so only a tree proven ours
/// is ever removed. When something else takes the path before the copy lands,
/// the copy is discarded and the op refused; nothing is overwritten.
fn apply_render(surface: &Path, planned: &PlannedOp, paths: &Paths) -> Result<Option<String>> {
    let op = &planned.op;
    let plan = planned.copy.as_ref().ok_or_else(|| {
        Error::internal(format!("render op without a copy: {}", op.path.display()))
    })?;
    let staged = stage_copy(surface, plan)?;
    #[cfg(test)]
    tests::after_staging(&op.path);
    let proven = match managed::verify_expected(&op.path, paths, &planned.expect) {
        Ok(proven) => proven,
        Err(err) => {
            discard(&staged);
            return Err(err);
        }
    };
    if let Some(reason) = clear(proven, paths)? {
        discard(&staged);
        return Ok(Some(reason));
    }
    match fsx::rename_noreplace(&staged, &op.path) {
        Ok(()) => Ok(None),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            discard(&staged);
            Ok(Some(format!(
                "{} was taken by something else before the served copy could land; it is untouched",
                op.path.display()
            )))
        }
        Err(err) => {
            discard(&staged);
            Err(Error::io(
                &format!("moving the served copy into {}", op.path.display()),
                &err,
            ))
        }
    }
}

fn discard(staged: &Path) {
    let _ = fs::remove_dir_all(staged);
}

/// Build a served copy in a fresh hidden directory of the surface.
///
/// The directory is created with `mkdir(2)` (never reused) and every file in
/// it with `O_CREAT | O_EXCL | O_NOFOLLOW`, so nothing planted in the surface
/// beforehand can redirect a write.
fn stage_copy(surface: &Path, plan: &CopyPlan) -> Result<PathBuf> {
    let name = &plan.marker.name;
    let mut staged = None;
    for attempt in 0..1000u32 {
        let candidate = surface.join(format!(
            ".skill-render-tmp-{name}-{}-{attempt}",
            std::process::id()
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                staged = Some(candidate);
                break;
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(Error::io(
                    &format!("creating {}", candidate.display()),
                    &err,
                ));
            }
        }
    }
    let staged = staged
        .ok_or_else(|| Error::internal(format!("no free staging name in {}", surface.display())))?;
    if let Err(err) = write_copy(&staged, plan) {
        discard(&staged);
        return Err(err);
    }
    Ok(staged)
}

fn write_copy(staged: &Path, plan: &CopyPlan) -> Result<()> {
    for item in &plan.copy.items {
        match item {
            CopyItem::Dir { rel } => {
                let dir = staged.join(rel);
                fs::create_dir(&dir)
                    .map_err(|err| Error::io(&format!("creating {}", dir.display()), &err))?;
            }
            CopyItem::File { rel, bytes, mode } => {
                // The record is written last, from the plan; a record the
                // source happens to carry is not copied.
                if rel.as_os_str() == RENDER_MARKER {
                    continue;
                }
                write_new(&staged.join(rel), bytes, *mode)?;
            }
        }
    }
    let mut record = serde_json::to_value(&plan.marker)
        .map_err(|err| Error::internal(format!("serializing a render record: {err}")))?;
    record["rendered_at"] = serde_json::Value::String(now_iso8601());
    let mut text = serde_json::to_string_pretty(&sorted(record))
        .map_err(|err| Error::internal(format!("serializing a render record: {err}")))?;
    text.push('\n');
    write_new(&staged.join(RENDER_MARKER), text.as_bytes(), 0o644)
}

/// A JSON value with every object's keys in sorted order.
fn sorted(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let ordered: BTreeMap<String, serde_json::Value> = map
                .into_iter()
                .map(|(key, value)| (key, sorted(value)))
                .collect();
            serde_json::Value::Object(ordered.into_iter().collect())
        }
        other => other,
    }
}

/// Create `path` (never an existing file, never through a link) with `bytes`
/// and exactly `mode`.
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(path)
        .map_err(|err| Error::io(&format!("creating {}", path.display()), &err))?;
    file.write_all(bytes)
        .map_err(|err| Error::io(&format!("writing {}", path.display()), &err))?;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|err| Error::io(&format!("setting the mode of {}", path.display()), &err))?;
    Ok(())
}

/// Record one refused op: warned for a human, returned for the report.
fn refuse(path: &Path, reason: String) -> Refusal {
    output::warn("render_op_refused", reason.clone());
    Refusal {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        code: "foreign_entry".to_string(),
        message: reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    type Hook = Box<dyn FnOnce(&Path)>;

    thread_local! {
        /// Run once by `apply_render` between staging and its re-proof.
        static AFTER_STAGING: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(super) fn after_staging(path: &Path) {
        if let Some(hook) = AFTER_STAGING.with(|slot| slot.borrow_mut().take()) {
            hook(path);
        }
    }

    /// The re-proof after staging is what refuses a swap made while the copy
    /// was being built: a managed link the plan saw, replaced by the user's
    /// own link once staging has begun, is refused by that re-proof, kept
    /// exactly as the user left it, and no staging directory is left behind.
    #[test]
    fn a_link_swapped_while_the_copy_is_staged_is_refused_and_kept() {
        let home = tempfile::TempDir::new().unwrap();
        let paths = Paths::fixture(home.path());
        let skill = paths.library.join("skill");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "# skill\n").unwrap();
        let user = home.path().join("mine");
        fs::create_dir_all(&user).unwrap();
        fs::write(user.join("SKILL.md"), "# mine\n").unwrap();
        let policy = crate::policy::default_policy(&paths);
        let row = policy.harness("claude").unwrap();
        let desired = ["skill".to_string()];

        let plan = plan_surface(row, &desired, &paths).unwrap();
        assert!(
            apply_ops(&row.surface, &plan.ops, &paths)
                .unwrap()
                .is_empty()
        );
        let served = row.surface.join("skill");
        assert!(fs::symlink_metadata(&served).unwrap().is_symlink());

        fs::write(
            skill.join("SKILL.md"),
            "# skill\n<!-- claude-only -->\nC\n<!-- /claude-only -->\n",
        )
        .unwrap();
        let plan = plan_surface(row, &desired, &paths).unwrap();
        assert_eq!(plan.ops.len(), 1);
        assert_eq!(plan.ops[0].op.op, "render");
        assert!(matches!(
            plan.ops[0].expect,
            Expected::ManagedSymlink { .. }
        ));

        let swapped = user.clone();
        let staged_seen = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = staged_seen.clone();
        let surface = row.surface.clone();
        AFTER_STAGING.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |path: &Path| {
                seen.set(fs::read_dir(&surface).unwrap().any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".skill-render-tmp-")
                }));
                fs::remove_file(path).unwrap();
                std::os::unix::fs::symlink(&swapped, path).unwrap();
            }));
        });

        let refusals = apply_ops(&row.surface, &plan.ops, &paths).unwrap();

        assert!(staged_seen.get(), "the hook ran after staging began");
        assert_eq!(refusals.len(), 1, "{refusals:?}");
        assert!(
            refusals[0].message.contains("is no longer ours"),
            "the re-proof refused it: {refusals:?}"
        );
        assert_eq!(fs::read_link(&served).unwrap(), user);
        assert_eq!(
            fs::read_to_string(user.join("SKILL.md")).unwrap(),
            "# mine\n"
        );
        let leftovers: Vec<_> = fs::read_dir(&row.surface)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(".skill-render-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_link_under_home_is_relative() {
        let home = Path::new("/sandbox/user");
        assert_eq!(
            link_text(
                &home.join(".claude/skills"),
                &home.join(".agents/skill-library/post"),
                home
            ),
            PathBuf::from("../../.agents/skill-library/post")
        );
        assert_eq!(
            link_text(
                &home.join(".agents/skills"),
                &home.join(".agents/skill-library/post/.variants/claude"),
                home
            ),
            PathBuf::from("../skill-library/post/.variants/claude")
        );
    }

    #[test]
    fn a_link_outside_home_is_absolute() {
        let home = Path::new("/sandbox/user");
        assert_eq!(
            link_text(
                &home.join("skills"),
                Path::new("/elsewhere/pool/post"),
                home
            ),
            PathBuf::from("/elsewhere/pool/post")
        );
    }

    #[test]
    fn relative_path_walks_up_and_back_down() {
        assert_eq!(
            relative_path(Path::new("/a/b/c"), Path::new("/a/x/y")),
            Some(PathBuf::from("../../x/y"))
        );
        assert_eq!(
            relative_path(Path::new("/a/b"), Path::new("/a/b")),
            Some(PathBuf::from("."))
        );
        assert_eq!(relative_path(Path::new("relative"), Path::new("/a")), None);
    }

    #[test]
    fn a_symlink_to_the_desired_source_is_satisfied() {
        let entry = Entry {
            name: "post".to_string(),
            path: PathBuf::from("/sandbox/user/.claude/skills/post"),
            kind: EntryKind::Symlink {
                target: PathBuf::from("/sandbox/user/.agents/skill-library/post"),
                dangling: true,
            },
            managed: true,
        };
        assert!(entry_is_satisfied(
            &entry,
            Path::new("/sandbox/user/.agents/skill-library/post")
        ));
        assert!(!entry_is_satisfied(
            &entry,
            Path::new("/sandbox/user/.agents/skill-library/post/.variants/claude")
        ));
    }
}
