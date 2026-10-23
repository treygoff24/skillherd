//! The managed-entry rule, implemented once and shared by render, reconcile,
//! doctor and drop.
//!
//! An entry in a surface is *managed* — ours, therefore fair game — when it is
//! a symlink whose **lexical** target (link text made absolute against the
//! link's own directory, `..` normalized, nothing resolved) lies under the
//! library, or a real directory carrying a valid served-copy record
//! (`.skill-render.json`) that names the directory and stands in for a link
//! into the library. A dangling managed link is still ours. Anything else is
//! foreign — including a link into the shared `agents_dir`, which other tools
//! (`npx skills add -g`, Codex, Cursor) write to — reported, never touched.

use std::fs;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::fsx;
use crate::model::Platform;
use crate::names::validate_name;
use crate::paths::{Paths, lexical_abs, normalize};

/// The record a served copy carries, and the only marker that can make a real
/// directory ours.
pub const RENDER_MARKER: &str = ".skill-render.json";

/// A served copy's record, as the renderer writes it.
///
/// `link_target` is the link text the copy stands in for; `source_dir` is the
/// tree it was rendered from. `harness` and `render_version` are optional so a
/// record written by an older renderer still parses (and is then refreshed by
/// the freshness check rather than dropped as foreign).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderMarker {
    pub name: String,
    pub platform: Platform,
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub render_version: Option<u64>,
    pub source_dir: PathBuf,
    pub link_target: PathBuf,
    pub source_hash: String,
    #[serde(default)]
    pub rendered_at: Option<String>,
}

/// Longest marker file read: a real record is a few hundred bytes.
const MAX_MARKER_BYTES: u64 = 64 * 1024;

/// The record of a served copy, when `dir` carries a valid one.
///
/// Valid means: a regular file (not a link) that parses, names `dir` itself,
/// records an absolute source and a SHA-256 source hash. Anything else — an
/// empty object, invalid JSON, a copy of another directory's record — is no
/// record at all.
pub fn read_marker(dir: &Path) -> Option<RenderMarker> {
    marker_naming(dir, dir.file_name()?.to_str()?)
}

/// The valid record in `dir` that names `name` (a copy moved aside keeps the
/// name of the path it was served at, not of where it now sits).
fn marker_naming(dir: &Path, name: &str) -> Option<RenderMarker> {
    let path = dir.join(RENDER_MARKER);
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.file_type().is_file() || meta.len() > MAX_MARKER_BYTES {
        return None;
    }
    let text = fs::read_to_string(&path).ok()?;
    let marker: RenderMarker = serde_json::from_str(&text).ok()?;
    let valid = marker.name == name
        && validate_name(&marker.name).is_ok()
        && marker.source_dir.is_absolute()
        && !marker.link_target.as_os_str().is_empty()
        && marker.source_hash.len() == 64
        && marker
            .source_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit());
    valid.then_some(marker)
}

/// True when a real directory carries a valid served-copy record.
pub fn has_marker(dir: &Path) -> bool {
    read_marker(dir).is_some()
}

/// True when `dir` carries a valid record that stands in for a link into the
/// library: the record names a link target that, read from `dir`'s parent like
/// the link it replaced, lies lexically under the library.
fn marker_is_ours(dir: &Path, paths: &Paths) -> bool {
    dir.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| copy_is_ours(dir, name, dir, paths))
}

/// True when `dir` holds our served copy of `name`, as served at `served_at`
/// (its own path, or the path it was moved aside from).
fn copy_is_ours(dir: &Path, name: &str, served_at: &Path, paths: &Paths) -> bool {
    marker_naming(dir, name).is_some_and(|marker| {
        let base = served_at.parent().unwrap_or_else(|| Path::new("."));
        lexical_abs(base, &marker.link_target).starts_with(&paths.library)
    })
}

/// What kind of filesystem entry a surface holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum EntryKind {
    /// `target` is the lexical absolute target, not a resolved path.
    Symlink {
        target: PathBuf,
        dangling: bool,
    },
    Dir,
    File,
}

/// One entry of a surface directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub managed: bool,
}

impl Entry {
    pub fn is_symlink(&self) -> bool {
        matches!(self.kind, EntryKind::Symlink { .. })
    }

    pub fn symlink_target(&self) -> Option<&Path> {
        match &self.kind {
            EntryKind::Symlink { target, .. } => Some(target.as_path()),
            _ => None,
        }
    }

    pub fn is_dangling(&self) -> bool {
        matches!(self.kind, EntryKind::Symlink { dangling: true, .. })
    }
}

/// True when `entry_path` belongs to the tool.
///
/// A missing path is not managed (a caller that needs to refuse a foreign entry
/// treats "nothing there" the same way).
pub fn is_managed(entry_path: &Path, paths: &Paths) -> Result<bool> {
    let meta = match fs::symlink_metadata(entry_path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(Error::io("reading a surface entry", &err)),
    };

    if meta.file_type().is_symlink() {
        let link_text =
            fs::read_link(entry_path).map_err(|err| Error::io("reading a link", &err))?;
        let base = entry_path.parent().unwrap_or_else(|| Path::new("."));
        let target = lexical_abs(base, &link_text);
        Ok(target.starts_with(&paths.library))
    } else if meta.is_dir() {
        Ok(marker_is_ours(entry_path, paths))
    } else {
        Ok(false)
    }
}

/// The identity a plan recorded for a path it intends to remove or replace.
///
/// A plan's managed/foreign decision is a claim about one moment; the path can
/// hold something else by the time the mutation runs. Carrying what was seen —
/// not just "it was ours" — lets the mutation re-prove it immediately before it
/// acts and refuse anything it did not plan for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expected {
    /// Nothing was at the path.
    Absent,
    /// A managed symlink carrying exactly this lexical target.
    ManagedSymlink { target: PathBuf },
    /// A managed real directory (ours through its marker).
    ManagedDir,
}

impl Expected {
    /// The identity of an entry a plan saw.
    ///
    /// A plan only ever carries something of ours — a symlink under our roots or
    /// a marker-carrying directory. A plain file is never ours, so `None` says
    /// "nothing here is ours to plan for" instead of inventing an identity.
    pub fn of(entry: &Entry) -> Option<Self> {
        match &entry.kind {
            EntryKind::Symlink { target, .. } => Some(Self::ManagedSymlink {
                target: target.clone(),
            }),
            EntryKind::Dir => Some(Self::ManagedDir),
            EntryKind::File => None,
        }
    }
}

/// What a re-proof found at the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proven {
    /// The live entry is what the plan saw: it may be removed or replaced.
    Matches(Entry),
    /// The path holds nothing and the plan expected nothing: nothing to do.
    Nothing,
    /// The path holds something the plan did not see. Never touch it.
    Foreign { reason: String },
}

/// One entry's live state, `None` when nothing is at the path.
pub fn entry_at(entry_path: &Path, paths: &Paths) -> Result<Option<Entry>> {
    let meta = match fs::symlink_metadata(entry_path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(Error::io(
                &format!("reading {}", entry_path.display()),
                &err,
            ));
        }
    };
    let kind = if meta.file_type().is_symlink() {
        let link_text =
            fs::read_link(entry_path).map_err(|err| Error::io("reading a link", &err))?;
        let base = entry_path.parent().unwrap_or_else(|| Path::new("."));
        let target = lexical_abs(base, &link_text);
        let dangling = !target.exists();
        EntryKind::Symlink { target, dangling }
    } else if meta.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    Ok(Some(Entry {
        name: entry_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: entry_path.to_path_buf(),
        kind,
        managed: is_managed(entry_path, paths)?,
    }))
}

/// Re-prove an entry against the identity a plan recorded for it.
///
/// The destructive helpers call this immediately before they remove or replace a
/// path: a mismatch is reported, never deleted. Real IO failures are errors; a
/// mismatch is [`Proven::Foreign`], which is a refusal, not a failure.
pub fn verify_expected(entry_path: &Path, paths: &Paths, expected: &Expected) -> Result<Proven> {
    let Some(entry) = entry_at(entry_path, paths)? else {
        return Ok(match expected {
            Expected::Absent => Proven::Nothing,
            _ => Proven::Foreign {
                reason: format!(
                    "{} is gone; the plan expected {}",
                    entry_path.display(),
                    describe(expected)
                ),
            },
        });
    };

    if !entry.managed {
        return Ok(Proven::Foreign {
            reason: format!(
                "{} is no longer ours ({}{}), so nothing of ours may be replaced or removed",
                entry_path.display(),
                describe_actual(&entry),
                marker_note(&entry)
            ),
        });
    }

    match (expected, &entry.kind) {
        (Expected::Absent, _) => Ok(Proven::Foreign {
            reason: format!(
                "{} was empty when the operation was planned, but now holds {}",
                entry_path.display(),
                describe_actual(&entry)
            ),
        }),
        (Expected::ManagedSymlink { target }, EntryKind::Symlink { target: live, .. })
            if normalize(live) == normalize(target) =>
        {
            Ok(Proven::Matches(entry))
        }
        (Expected::ManagedSymlink { target }, EntryKind::Symlink { target: live, .. }) => {
            Ok(Proven::Foreign {
                reason: format!(
                    "{} now links to {} instead of the planned {}; relink it by hand or rerun the operation",
                    entry_path.display(),
                    live.display(),
                    target.display()
                ),
            })
        }
        (Expected::ManagedDir, EntryKind::Dir) => Ok(Proven::Matches(entry)),
        (expected, _) => Ok(Proven::Foreign {
            reason: format!(
                "{} now holds {}, not the {} the plan saw",
                entry_path.display(),
                describe_actual(&entry),
                describe(expected)
            ),
        }),
    }
}

fn describe(expected: &Expected) -> String {
    match expected {
        Expected::Absent => "nothing".to_string(),
        Expected::ManagedSymlink { target } => format!("a link to {}", target.display()),
        Expected::ManagedDir => "a managed directory".to_string(),
    }
}

fn describe_actual(entry: &Entry) -> String {
    match &entry.kind {
        EntryKind::Symlink { target, .. } => format!("a link to {}", target.display()),
        EntryKind::Dir => "a real directory".to_string(),
        EntryKind::File => "a real file".to_string(),
    }
}

/// What an entry is, for a refusal's message.
pub fn describe_entry(entry: &Entry) -> String {
    describe_actual(entry)
}

/// Why a real entry is not ours, for the refusal's message.
fn marker_note(entry: &Entry) -> String {
    match entry.kind {
        EntryKind::Dir => format!(" and carries no valid {RENDER_MARKER} record"),
        _ => String::new(),
    }
}

/// What a removal did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    Removed,
    /// The path no longer held what the re-proof saw; nothing was deleted.
    Refused(String),
}

/// Remove the entry a re-proof returned.
///
/// A link is unlinked, never followed (`unlink(2)` cannot remove a directory
/// that took its place). A served copy is first moved aside with a no-replace
/// rename to a hidden sibling and its record is read again there: a directory
/// that took the path between the re-proof and the move is put back and
/// refused, so a recursive delete only ever runs on a tree that was proven
/// ours after it stopped being reachable at its served path.
pub fn remove_entry(entry: &Entry, paths: &Paths) -> Result<Removal> {
    match entry.kind {
        EntryKind::Dir => {
            if let Some(source) = served_copy_is_its_source(entry, paths) {
                return Ok(Removal::Refused(format!(
                    "{} is a served copy, but the pool entry {} resolves to {}, inside it: removing it would delete the only tree; it is untouched",
                    entry.path.display(),
                    paths.library.join(&entry.name).display(),
                    source.display()
                )));
            }
            let aside = move_aside(&entry.path)?;
            if !copy_is_ours(&aside, &entry.name, &entry.path, paths) {
                let reason = match fsx::rename_noreplace(&aside, &entry.path) {
                    Ok(()) => format!(
                        "{} changed while it was being removed; it was put back untouched",
                        entry.path.display()
                    ),
                    Err(err) => format!(
                        "{} changed while it was being removed and could not be put back ({err}); it is untouched at {}",
                        entry.path.display(),
                        aside.display()
                    ),
                };
                return Ok(Removal::Refused(reason));
            }
            fs::remove_dir_all(&aside)
                .map_err(|err| Error::io(&format!("removing {}", aside.display()), &err))?;
            Ok(Removal::Removed)
        }
        _ => {
            fs::remove_file(&entry.path)
                .map_err(|err| Error::io(&format!("removing {}", entry.path.display()), &err))?;
            Ok(Removal::Removed)
        }
    }
}

/// The resolved pool entry of a served copy when it lies inside that copy (the
/// pool entry was replaced by a link to the served directory): the copy is
/// then the only tree, and deleting it deletes the skill. A pool entry that
/// links to a separate development repo resolves elsewhere and is `None`.
fn served_copy_is_its_source(entry: &Entry, paths: &Paths) -> Option<PathBuf> {
    let served = fs::canonicalize(&entry.path).ok()?;
    let source = fs::canonicalize(paths.library.join(&entry.name)).ok()?;
    source.starts_with(&served).then_some(source)
}

/// Move `path` to a fresh hidden sibling with a no-replace rename.
pub fn move_aside(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    for attempt in 0..1000u32 {
        let aside = parent.join(format!(
            ".skill-render-old-{name}-{}-{attempt}",
            std::process::id()
        ));
        match fsx::rename_noreplace(path, &aside) {
            Ok(()) => return Ok(aside),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(Error::io(&format!("moving {} aside", path.display()), &err));
            }
        }
    }
    Err(Error::internal(format!(
        "no free name to move {} aside",
        path.display()
    )))
}

/// List a surface, skipping dot-entries, sorted by name.
///
/// A surface that does not exist yet is an empty surface, not an error.
pub fn list_entries(surface: &Path, paths: &Paths) -> Result<Vec<Entry>> {
    let read = match fs::read_dir(surface) {
        Ok(read) => read,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(Error::io(
                &format!("reading surface {}", surface.display()),
                &err,
            ));
        }
    };

    let mut entries = Vec::new();
    for dirent in read {
        let dirent = dirent.map_err(|err| Error::io("reading a surface entry", &err))?;
        let name = dirent.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if let Some(entry) = entry_at(&dirent.path(), paths)? {
            entries.push(entry);
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    struct Fixture {
        dir: TempDir,
        paths: Paths,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let paths = Paths::fixture(dir.path());
            fs::create_dir_all(&paths.library).unwrap();
            fs::create_dir_all(&paths.agents_dir).unwrap();
            Self { dir, paths }
        }

        fn surface(&self) -> PathBuf {
            let surface = self.paths.claude_shared.join("skills");
            fs::create_dir_all(&surface).unwrap();
            surface
        }
    }

    #[test]
    fn relative_link_into_the_library_is_managed() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        fs::create_dir_all(fixture.paths.library.join("post")).unwrap();
        let link = surface.join("post");
        symlink("../../.agents/skill-library/post", &link).unwrap();

        assert!(is_managed(&link, &fixture.paths).unwrap());
        let entry = list_entries(&surface, &fixture.paths).unwrap().remove(0);
        assert_eq!(entry.name, "post");
        assert!(entry.managed);
        assert!(!entry.is_dangling());
        assert_eq!(
            entry.symlink_target().unwrap(),
            fixture.paths.library.join("post"),
            "the lexical target is under the library"
        );
    }

    /// `npx skills add -g` links `~/.claude/skills/<name>` into the shared
    /// `~/.agents/skills`. That link is another tool's: only a link into the
    /// library is ours, whatever folder the shared root happens to be.
    #[test]
    fn a_link_into_agents_dir_is_foreign() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        fs::create_dir_all(fixture.paths.agents_dir.join("impeccable")).unwrap();
        let absolute = surface.join("impeccable");
        symlink(fixture.paths.agents_dir.join("impeccable"), &absolute).unwrap();
        assert!(!is_managed(&absolute, &fixture.paths).unwrap());

        let relative = surface.join("web-design-guidelines");
        symlink("../../.agents/skills/web-design-guidelines", &relative).unwrap();
        assert!(!is_managed(&relative, &fixture.paths).unwrap());
        let entry = entry_at(&relative, &fixture.paths).unwrap().unwrap();
        assert!(!entry.managed && entry.is_dangling());
    }

    #[test]
    fn a_link_into_a_development_repo_is_foreign() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let repo = fixture.dir.path().join("Code/x/skills/thing");
        fs::create_dir_all(&repo).unwrap();
        let link = surface.join("thing");
        symlink(&repo, &link).unwrap();
        assert!(!is_managed(&link, &fixture.paths).unwrap());
        let entry = list_entries(&surface, &fixture.paths).unwrap().remove(0);
        assert!(!entry.managed);
        assert!(entry.is_symlink());
    }

    #[test]
    fn a_library_link_through_a_development_repo_is_still_ours() {
        // `library/post` is itself a link into a repo; the surface link's
        // lexical target is under the library, so it is managed.
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let repo = fixture.dir.path().join("Code/post/skills/post");
        fs::create_dir_all(&repo).unwrap();
        symlink(&repo, fixture.paths.library.join("post")).unwrap();

        let link = surface.join("post");
        symlink("../../.agents/skill-library/post", &link).unwrap();
        assert!(
            is_managed(&link, &fixture.paths).unwrap(),
            "the lexical target is library/post even though it resolves into a repo"
        );
    }

    /// A marker the renderer would write for `<surface>/<name>`, standing in
    /// for the link `link_target`.
    fn marker(name: &str, link_target: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "platform": "mac",
            "harness": "claude",
            "render_version": 2,
            "source_dir": "/sandbox/user/.agents/skill-library/x",
            "link_target": link_target,
            "source_hash": "0".repeat(64),
            "rendered_at": "2026-10-22T00:00:00+00:00",
        })
    }

    fn served_copy(surface: &Path, name: &str, marker_text: &str) -> PathBuf {
        let dir = surface.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "text").unwrap();
        fs::write(dir.join(RENDER_MARKER), marker_text).unwrap();
        dir
    }

    #[test]
    fn a_real_dir_with_a_valid_render_marker_is_managed() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let rendered = served_copy(
            &surface,
            "agent-browser",
            &marker("agent-browser", "../../.agents/skill-library/agent-browser").to_string(),
        );
        assert!(has_marker(&rendered));
        assert!(is_managed(&rendered, &fixture.paths).unwrap());
    }

    /// The marker's file name alone proves nothing: a directory is ours only
    /// when the record parses, names this directory, and stands in for a link
    /// into the library. The retired `.skill-managed.json` name, which nothing
    /// in this tool writes, is no proof either.
    #[test]
    fn a_marker_is_accepted_on_its_contents_not_its_name() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let library_link = "../../.agents/skill-library/x";

        let cases: Vec<(&str, String)> = vec![
            ("empty-object", "{}".to_string()),
            ("not-json", "this is not json".to_string()),
            ("array", "[]".to_string()),
            (
                "wrong-name",
                marker("someone-else", library_link).to_string(),
            ),
            (
                "outside",
                marker("outside", "/sandbox/other/Code/x").to_string(),
            ),
            (
                "into-agents",
                marker("into-agents", "../../.agents/skills/x").to_string(),
            ),
            ("bad-hash", {
                let mut value = marker("bad-hash", library_link);
                value["source_hash"] = serde_json::json!("not-a-hash");
                value.to_string()
            }),
            ("no-source", {
                let mut value = marker("no-source", library_link);
                value.as_object_mut().unwrap().remove("source_dir");
                value.to_string()
            }),
        ];
        for (name, text) in &cases {
            let dir = served_copy(&surface, name, text);
            assert!(
                !is_managed(&dir, &fixture.paths).unwrap(),
                "{name}: {text} must not make a directory ours"
            );
        }

        let legacy = surface.join("legacy");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join(".skill-managed.json"), "{}").unwrap();
        assert!(!has_marker(&legacy));
        assert!(!is_managed(&legacy, &fixture.paths).unwrap());

        // A marker that is itself a link is not a record the renderer wrote.
        let linked = surface.join("linked-marker");
        fs::create_dir_all(&linked).unwrap();
        let real = fixture.dir.path().join("real-marker.json");
        fs::write(&real, marker("linked-marker", library_link).to_string()).unwrap();
        symlink(&real, linked.join(RENDER_MARKER)).unwrap();
        assert!(!is_managed(&linked, &fixture.paths).unwrap());
    }

    #[test]
    fn a_real_dir_without_a_marker_is_foreign() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let hand_placed = surface.join("writing-plans");
        fs::create_dir_all(&hand_placed).unwrap();
        fs::write(hand_placed.join("SKILL.md"), "text").unwrap();
        assert!(!is_managed(&hand_placed, &fixture.paths).unwrap());

        let entry = list_entries(&surface, &fixture.paths).unwrap().remove(0);
        assert_eq!(entry.kind, EntryKind::Dir);
        assert!(!entry.managed);
    }

    #[test]
    fn a_dangling_link_into_the_library_is_managed_and_dangling() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        let link = surface.join("ghost");
        symlink("../../.agents/skill-library/ghost", &link).unwrap();
        assert!(is_managed(&link, &fixture.paths).unwrap());
        let entry = list_entries(&surface, &fixture.paths).unwrap().remove(0);
        assert!(entry.managed);
        assert!(entry.is_dangling());
        assert_eq!(
            entry.symlink_target().unwrap(),
            fixture.paths.library.join("ghost")
        );
    }

    #[test]
    fn a_dangling_link_outside_our_roots_is_foreign() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        symlink("/no/such/place", surface.join("gone")).unwrap();
        let entry = list_entries(&surface, &fixture.paths).unwrap().remove(0);
        assert!(!entry.managed);
        assert!(entry.is_dangling());
    }

    #[test]
    fn dot_entries_are_skipped() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        fs::create_dir_all(surface.join(".system")).unwrap();
        fs::write(surface.join(".system/SKILL.md"), "text").unwrap();
        symlink(
            "../../.agents/skill-library/.hidden",
            surface.join(".hidden"),
        )
        .unwrap();
        fs::write(surface.join("visible"), "text").unwrap();

        let entries = list_entries(&surface, &fixture.paths).unwrap();
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["visible"]);
        assert_eq!(entries[0].kind, EntryKind::File);
        assert!(!entries[0].managed);
    }

    #[test]
    fn entries_are_sorted_by_name() {
        let fixture = Fixture::new();
        let surface = fixture.surface();
        for name in ["zeta", "alpha", "mid"] {
            fs::create_dir_all(fixture.paths.library.join(name)).unwrap();
            symlink(
                format!("../../.agents/skill-library/{name}"),
                surface.join(name),
            )
            .unwrap();
        }
        let names: Vec<String> = list_entries(&surface, &fixture.paths)
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, ["alpha", "mid", "zeta"]);
    }

    #[test]
    fn a_missing_surface_is_empty_and_a_missing_entry_is_not_managed() {
        let fixture = Fixture::new();
        let missing = fixture.dir.path().join("nope");
        assert_eq!(list_entries(&missing, &fixture.paths).unwrap(), Vec::new());
        assert!(!is_managed(&missing.join("x"), &fixture.paths).unwrap());
    }
}
