//! Project activation (`skillherd add` / `skillherd drop`): serving pool skills inside
//! one project's `.claude/skills` and `.agents/skills`.
//!
//! A project surface is planned and applied exactly like a global one: each
//! project root is a harness row ([`project_rows`]), `surfaces::plan_surface_restricted`
//! decides link versus stripped copy per name and records what it will
//! replace, and `surfaces::apply_ops` re-proves and writes. So the managed-entry
//! rule, the served-copy freshness check and the no-replace writes are the
//! global ones, and each surface carries its own harness identity end to end.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{Error, Result};
use crate::library::edit_distance;
use crate::managed::{self, EntryKind, Expected, Proven, Removal};
use crate::model::{HarnessRow, PlanOp, ReadRoot, Refusal, StripperReport, Tier};
use crate::names::validate_name;
use crate::paths::{Paths, lexical_abs};

/// One skill activated in a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectActivation {
    pub name: String,
    /// The harness variant served for the name, when the pool carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// The project entries written for the name (links or served copies).
    pub links: Vec<PathBuf>,
}

/// The stripping pass over one project surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectSurface {
    pub surface: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    pub report: StripperReport,
}

/// The `skillherd add` payload.
///
/// Under `--dry-run` `added` describes what the run would write and nothing is
/// written; `plan` carries the operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectAddReport {
    pub project: PathBuf,
    pub added: Vec<ProjectActivation>,
    /// Names already served correctly on every project surface.
    pub unchanged: Vec<String>,
    /// Names refused because an entry at a project path is not ours.
    pub refused: Vec<Refusal>,
    pub plan: Vec<PlanOp>,
    /// The stripping pass, per project surface that serves anything.
    pub stripper: Vec<ProjectSurface>,
}

/// One project entry `drop` looked at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectDrop {
    pub name: String,
    /// Which of the project roots held the name (`.claude/skills`,
    /// `.agents/skills`, the legacy `.codex/skills`).
    pub existed: Vec<PathBuf>,
    /// What was removed; under `--dry-run`, what would be removed.
    pub removed: Vec<PathBuf>,
}

/// The `skillherd drop` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectDropReport {
    pub project: PathBuf,
    pub dropped: Vec<ProjectDrop>,
    pub refused: Vec<Refusal>,
    pub plan: Vec<PlanOp>,
}

/// The three project roots `add` and `drop` know about, in link order.
pub fn project_surfaces(project: &Path) -> [PathBuf; 3] {
    [
        project.join(".claude/skills"),
        project.join(".agents/skills"),
        project.join(".codex/skills"),
    ]
}

/// The two project roots `add` serves, as harness rows: `.claude/skills` gets
/// the Claude variant and Claude's harness blocks; `.agents/skills`, shared by
/// several harnesses, gets the universal copy with every harness block
/// dropped. The legacy `.codex/skills` is only ever dropped, never added.
pub fn project_rows(project: &Path) -> [HarnessRow; 2] {
    let [claude, agents, _codex] = project_surfaces(project);
    [
        HarnessRow {
            name: "claude".to_string(),
            tier: Tier::Main,
            surface: claude,
            variant_key: Some("claude".to_string()),
            reads: vec![ReadRoot::Surface],
        },
        HarnessRow {
            name: "agents".to_string(),
            tier: Tier::Lane,
            surface: agents,
            variant_key: None,
            reads: vec![ReadRoot::Surface],
        },
    ]
}

/// What a `drop` did to one name's planned links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedLinks {
    /// The entries that were removed (or, under `--dry-run`, would be).
    pub removed: Vec<PathBuf>,
    /// The operations, for a dry run's plan.
    pub plan: Vec<PlanOp>,
    /// One reason per entry refused: it was no longer what the read recorded.
    pub refused: Vec<String>,
}

/// Remove the project entries a `drop` read.
///
/// Every entry is re-proved against what the read recorded, immediately before
/// it is removed: a path that now holds something else — a real user directory
/// where a managed link was — is refused untouched, reported, and the rest
/// still go.
pub fn remove_links(
    paths: &Paths,
    planned: &[(PathBuf, Expected)],
    dry_run: bool,
) -> Result<DroppedLinks> {
    let mut removed = Vec::new();
    let mut plan = Vec::new();
    let mut refused = Vec::new();
    for (link, expected) in planned {
        match managed::verify_expected(link, paths, expected)? {
            // Gone already: the wanted state.
            Proven::Nothing => {}
            Proven::Foreign { reason } => refused.push(reason),
            Proven::Matches(entry) => {
                let op = PlanOp {
                    op: if entry.kind == EntryKind::Dir {
                        "remove"
                    } else {
                        "unlink"
                    }
                    .into(),
                    path: link.clone(),
                    target: None,
                };
                if !dry_run && let Removal::Refused(reason) = managed::remove_entry(&entry, paths)?
                {
                    refused.push(reason);
                    continue;
                }
                plan.push(op);
                removed.push(link.clone());
            }
        }
    }
    Ok(DroppedLinks {
        removed,
        plan,
        refused,
    })
}

/// The project a command acts on: `--project DIR`, else the nearest git root
/// above the cwd, else the cwd itself.
pub fn project_dir(project: Option<&Path>) -> Result<PathBuf> {
    let cwd =
        std::env::current_dir().map_err(|err| Error::io("reading the working directory", &err))?;
    let dir = match project {
        Some(path) => lexical_abs(&cwd, path),
        None => git_root(&cwd).unwrap_or(cwd),
    };
    if !dir.is_dir() {
        return Err(Error::input_not_found_reason(&dir, "is not a directory"));
    }
    Ok(dir)
}

fn git_root(from: &Path) -> Option<PathBuf> {
    let mut dir = from.to_path_buf();
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// Expand `@preset` arguments into skill names.
pub fn expand_names(paths: &Paths, args: &[String]) -> Result<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    for arg in args {
        let Some(preset) = arg.strip_prefix('@') else {
            validate_name(arg)?;
            if !names.iter().any(|name| name == arg) {
                names.push(arg.clone());
            }
            continue;
        };
        validate_name(preset)?;
        let file = paths.library.join(".globals/presets").join(preset);
        let text = read_preset(&paths.library, preset, &file)?;
        for (index, raw) in text.lines().enumerate() {
            let name = raw.split('#').next().unwrap_or("").trim();
            if name.is_empty() {
                continue;
            }
            // The line is never echoed: a preset is a file anyone with write
            // access to the pool controls, and an error is printed and logged.
            if validate_name(name).is_err() {
                return Err(Error::InvalidName {
                    message: format!(
                        "{} line {} is not a skill name; presets hold one skill name per line",
                        file.display(),
                        index + 1
                    ),
                    details: json!({ "preset": preset, "path": file, "line": index + 1 }),
                    suggested_fix: Some(
                        "fix that line of the preset: one name matching ^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$, `#` starts a comment".into(),
                    ),
                });
            }
            if !names.iter().any(|existing| existing == name) {
                names.push(name.to_string());
            }
        }
    }
    Ok(names)
}

/// The largest preset read; a preset is a short list of names.
const MAX_PRESET_BYTES: u64 = 64 * 1024;

/// Read `<library>/.globals/presets/<preset>` without following a link in
/// any component below the library: `.globals`, `presets` and the preset are
/// each opened relative to the one before with `O_NOFOLLOW`, so a preset (or
/// a presets directory) linked to a secrets file is refused, not read.
fn read_preset(library: &Path, preset: &str, file: &Path) -> Result<String> {
    use std::ffi::CString;
    use std::io::Read;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let not_found = || {
        Error::input_not_found_reason(
            file,
            "does not exist; presets are one skill name per line under the pool",
        )
    };
    let refused = |what: &str| {
        Error::input_not_found_reason(
            file,
            &format!(
                "cannot be read: {what} is a symlink or not a regular entry; presets are read only from real files inside the pool"
            ),
        )
    };
    let open_at = |dir: Option<&OwnedFd>, name: &[u8], flags: i32| -> std::io::Result<OwnedFd> {
        let name = CString::new(name)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let fd = unsafe {
            match dir {
                Some(dir) => libc::openat(
                    std::os::fd::AsRawFd::as_raw_fd(dir),
                    name.as_ptr(),
                    flags | libc::O_CLOEXEC,
                ),
                None => libc::open(name.as_ptr(), flags | libc::O_CLOEXEC),
            }
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    };
    let classify = |err: std::io::Error, what: &str| match err.raw_os_error() {
        Some(libc::ENOENT) => not_found(),
        Some(libc::ELOOP) | Some(libc::ENOTDIR) => refused(what),
        _ => Error::io(&format!("reading {}", file.display()), &err),
    };

    // The library is the operator's chosen root; it is opened as given.
    let root = open_at(
        None,
        library.as_os_str().as_bytes(),
        libc::O_RDONLY | libc::O_DIRECTORY,
    )
    .map_err(|err| classify(err, "the library"))?;
    let dir_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
    let globals =
        open_at(Some(&root), b".globals", dir_flags).map_err(|err| classify(err, ".globals"))?;
    let presets = open_at(Some(&globals), b"presets", dir_flags)
        .map_err(|err| classify(err, ".globals/presets"))?;
    let fd = open_at(
        Some(&presets),
        preset.as_bytes(),
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
    )
    .map_err(|err| classify(err, "the preset"))?;
    let mut handle = fs::File::from(fd);
    let meta = handle
        .metadata()
        .map_err(|err| Error::io(&format!("reading {}", file.display()), &err))?;
    if !meta.is_file() {
        return Err(refused("the preset"));
    }
    if meta.len() > MAX_PRESET_BYTES {
        return Err(Error::input_not_found_reason(
            file,
            "is larger than 64 KiB; a preset is one skill name per line",
        ));
    }
    let mut bytes = Vec::new();
    handle
        .by_ref()
        .take(MAX_PRESET_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|err| Error::io(&format!("reading {}", file.display()), &err))?;
    String::from_utf8(bytes).map_err(|_| {
        Error::input_not_found_reason(
            file,
            "is not UTF-8 text; a preset is one skill name per line",
        )
    })
}

/// Every name must be a pool entry before anything is written.
pub fn require_library_entries(paths: &Paths, names: &[String]) -> Result<()> {
    let missing: Vec<&String> = names
        .iter()
        .filter(|name| fs::symlink_metadata(paths.library.join(name)).is_err())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let pool = library_entry_names(paths);
    let suggestions: BTreeMap<String, Vec<String>> = missing
        .iter()
        .map(|name| ((*name).clone(), closest_names(name, &pool, 3)))
        .collect();
    let listed: Vec<String> = missing.iter().map(|name| format!("{name:?}")).collect();
    Err(Error::InputNotFound {
        message: format!("the pool has no entry named {}", listed.join(", ")),
        details: json!({
            "names": missing,
            "suggestions": suggestions,
            "library": paths.library,
        }),
        suggested_fix: Some(
            "`skillherd list --all` shows the pool; `skillherd reconcile` imports an inbox entry into it"
                .into(),
        ),
    })
}

/// The pool's non-dot entry names.
fn library_entry_names(paths: &Paths) -> Vec<String> {
    let Ok(read) = fs::read_dir(&paths.library) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .filter_map(|dirent| dirent.ok())
        .map(|dirent| dirent.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// The nearest pool names to `target` (case-insensitive, at most 4 edits
/// away), for a "did you mean".
pub fn closest_names(target: &str, pool: &[String], limit: usize) -> Vec<String> {
    let wanted = target.to_lowercase();
    let mut scored: Vec<(usize, &str)> = pool
        .iter()
        .map(|name| (edit_distance(&wanted, &name.to_lowercase()), name.as_str()))
        .collect();
    scored.sort();
    scored
        .into_iter()
        .filter(|(distance, _)| *distance <= 4)
        .take(limit)
        .map(|(_, name)| name.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closest_names_use_edit_distance() {
        let pool: Vec<String> = ["post", "grilling", "writing"]
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        assert_eq!(closest_names("pots", &pool, 3)[0], "post");
        assert_eq!(closest_names("GRILLNG", &pool, 3)[0], "grilling");
        assert!(closest_names("zzzzzzzz", &pool, 3).is_empty());
    }

    #[test]
    fn each_project_row_carries_its_own_harness_identity() {
        let [claude, agents] = project_rows(Path::new("/work/proj"));
        assert_eq!(claude.surface, Path::new("/work/proj/.claude/skills"));
        assert_eq!(claude.variant_key.as_deref(), Some("claude"));
        assert_eq!(agents.surface, Path::new("/work/proj/.agents/skills"));
        assert_eq!(agents.variant_key, None);
    }
}
