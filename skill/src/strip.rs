//! The fence stripper: pure functions from a skill's source tree to the bytes
//! a surface serves. Nothing here writes; `render.rs` plans and
//! `surfaces::apply_ops` writes, behind the managed-entry re-proof.
//!
//! Two axes of text inside a skill's `.md` files are fenced with marker lines,
//! one per machine and one per harness:
//!
//! ```text
//! <!-- mac-only -->    ...  <!-- /mac-only -->
//! <!-- linux-only -->  ...  <!-- /linux-only -->
//! <!-- cell-only -->   ...  <!-- /cell-only --> (legacy Linux alias)
//! <!-- claude-only --> ...  <!-- /claude-only -->
//! ```
//!
//! A machine block is kept when its name is the render's platform; a harness
//! block is kept when its name is the surface's harness key, and dropped when
//! the surface has none (a surface several harnesses share gets neutral text).
//! The axes nest at most one level; a block of an axis already open is
//! malformed. A dropped outer block drops everything inside it. Marker lines
//! inside a fenced code block (``` or ~~~) are ordinary content. Removing a
//! block closes the seam it leaves: the blank run before it and the one after
//! it become one run, the larger of the two. A file with no markers comes back
//! byte for byte.
//!
//! This is a port of the Python `skill-render` stripper; its grammar, messages
//! and seam rule are kept exactly, and `tests/strip.rs` checks the port against
//! outputs the Python tool produced.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};
use crate::hash::Sha256;
use crate::model::Platform;

/// Bumped whenever the served bytes for an unchanged source can change: it is
/// a freshness key in every served copy's record. 3: the copy follows a link
/// only when it stays inside the skill or the library. 4: recognize `linux`
/// machine fences and retain `cell` as a legacy Linux alias.
pub const RENDER_VERSION: u64 = 4;

/// The machine names a marker may carry.
pub const MACHINE_NAMES: [&str; 3] = ["mac", "linux", "cell"];

/// The harness names a marker may carry. A surface whose harness key is not
/// one of these keeps no harness block, like a shared surface.
pub const HARNESS_NAMES: [&str; 6] = ["claude", "codex", "grok", "pi", "cursor", "lane"];

/// Which axis a marker belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Machine,
    Harness,
}

/// One marker line, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub axis: Axis,
    pub name: String,
    pub open: bool,
}

/// A malformed `.md` file: the 1-based line and the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerError {
    pub line: usize,
    pub reason: String,
}

impl std::fmt::Display for MarkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.reason)
    }
}

/// Python's `bytes.isspace` set, which `strip()` uses.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn trim(bytes: &[u8]) -> &[u8] {
    trim_end(trim_start(bytes))
}

fn trim_start(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !is_space(*byte))
        .unwrap_or(bytes.len());
    &bytes[start..]
}

fn trim_end(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|byte| !is_space(*byte))
        .map_or(0, |index| index + 1);
    &bytes[..end]
}

/// The line without its trailing `\r`/`\n` run.
fn without_eol(line: &[u8]) -> &[u8] {
    let end = line
        .iter()
        .rposition(|byte| *byte != b'\r' && *byte != b'\n')
        .map_or(0, |index| index + 1);
    &line[..end]
}

/// An empty line. A whitespace-only line is content, not blank.
fn is_blank(line: &[u8]) -> bool {
    without_eol(line).is_empty()
}

/// The marker a whole line holds, else `None`.
///
/// A marker is a line holding only `<!-- NAME-only -->` or its closing form,
/// NAME a machine or harness name; anything else (an unknown name, trailing
/// text, a marker inside prose) is ordinary content.
pub fn parse_marker(line: &[u8]) -> Option<Marker> {
    let stripped = trim(line);
    if !stripped.starts_with(b"<!--") || !stripped.ends_with(b"-->") {
        return None;
    }
    let inner = if stripped.len() >= 7 {
        trim(&stripped[4..stripped.len() - 3])
    } else {
        &[][..]
    };
    let (inner, open) = match inner.strip_prefix(b"/") {
        Some(rest) => (trim(rest), false),
        None => (inner, true),
    };
    let name = inner.strip_suffix(b"-only")?;
    if !name.is_ascii() {
        return None;
    }
    let name = std::str::from_utf8(name).ok()?;
    let axis = if MACHINE_NAMES.contains(&name) {
        Axis::Machine
    } else if HARNESS_NAMES.contains(&name) {
        Axis::Harness
    } else {
        return None;
    };
    Some(Marker {
        axis,
        name: name.to_string(),
        open,
    })
}

/// The code fence a line opens, as `(char, run length)`: ``` or ~~~, any
/// indentation, any info string.
fn fence_open(line: &[u8]) -> Option<(u8, usize)> {
    let body = trim_start(line);
    let first = *body.first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let run = body.iter().take_while(|byte| **byte == first).count();
    (run >= 3).then_some((first, run))
}

/// True when the line closes the open fence: the same character, a run at
/// least as long, nothing but whitespace around it.
fn fence_closes(line: &[u8], (character, run): (u8, usize)) -> bool {
    let body = trim(line);
    let leading = body.iter().take_while(|byte| **byte == character).count();
    leading >= run && leading == body.len()
}

/// Split on `\n`, keeping each line's ending (Python's `readlines`).
fn lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (index, byte) in data.iter().enumerate() {
        if *byte == b'\n' {
            out.push(&data[start..=index]);
            start = index + 1;
        }
    }
    if start < data.len() {
        out.push(&data[start..]);
    }
    out
}

fn block_is_kept(axis: Axis, name: &str, keep: Platform, harness: Option<&str>) -> bool {
    match axis {
        Axis::Machine => name == keep.to_string() || (name == "cell" && keep == Platform::Linux),
        Axis::Harness => harness == Some(name),
    }
}

/// What stripping one file produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stripped {
    pub bytes: Vec<u8>,
    /// The file carried a marker of either axis; when false, `bytes` is the
    /// input unchanged.
    pub had_markers: bool,
}

/// Strip one file's text for a platform and an optional harness.
///
/// Errors on a block opened inside an open block of its own axis, a stray or
/// mismatched close, a block left open at the end, or any marker inside YAML
/// frontmatter. Whether a file is malformed does not depend on `keep` or
/// `harness`.
pub fn strip_bytes(
    data: &[u8],
    keep: Platform,
    harness: Option<&str>,
) -> std::result::Result<Stripped, MarkerError> {
    let lines = lines(data);
    let mut out: Vec<&[u8]> = Vec::with_capacity(lines.len());
    let mut had_markers = false;
    // Outermost first; at most one block per axis.
    let mut open_blocks: Vec<(Axis, String, usize)> = Vec::new();
    // Lines were just removed: collapse blank lines against the last kept one.
    let mut seam = false;
    let mut fence: Option<(u8, usize)> = None;
    let mut in_frontmatter = lines
        .first()
        .is_some_and(|line| without_eol(line) == b"---");

    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;
        if in_frontmatter && number > 1 && without_eol(line) == b"---" {
            in_frontmatter = false;
            out.push(line);
            continue;
        }
        let mut marker = None;
        if in_frontmatter {
            marker = parse_marker(line);
        } else if let Some(open) = fence {
            if fence_closes(line, open) {
                fence = None;
            }
        } else {
            fence = fence_open(line);
            if fence.is_none() {
                marker = parse_marker(line);
            }
        }

        let Some(marker) = marker else {
            let kept = open_blocks
                .iter()
                .all(|(axis, name, _)| block_is_kept(*axis, name, keep, harness));
            if kept {
                if seam {
                    if is_blank(line) && out.last().is_some_and(|last| is_blank(last)) {
                        continue;
                    }
                    seam = false;
                }
                out.push(line);
            }
            continue;
        };

        had_markers = true;
        if in_frontmatter {
            return Err(MarkerError {
                line: number,
                reason: "marker inside YAML frontmatter".to_string(),
            });
        }
        if marker.open {
            if let Some((_, open_name, open_line)) =
                open_blocks.iter().find(|(axis, _, _)| *axis == marker.axis)
            {
                return Err(MarkerError {
                    line: number,
                    reason: format!(
                        "<!-- {}-only --> opened while <!-- {open_name}-only --> block from line {open_line} is still open (nesting is not allowed)",
                        marker.name
                    ),
                });
            }
            open_blocks.push((marker.axis, marker.name, number));
        } else {
            let Some((open_axis, open_name, open_line)) = open_blocks.last() else {
                return Err(MarkerError {
                    line: number,
                    reason: format!("<!-- /{}-only --> closes nothing", marker.name),
                });
            };
            if *open_axis != marker.axis || *open_name != marker.name {
                return Err(MarkerError {
                    line: number,
                    reason: format!(
                        "<!-- /{}-only --> closes a <!-- {open_name}-only --> block opened on line {open_line}",
                        marker.name
                    ),
                });
            }
            open_blocks.pop();
        }
        seam = true;
    }

    if let Some((_, name, opened)) = open_blocks.first() {
        return Err(MarkerError {
            line: lines.len() + 1,
            reason: format!("<!-- {name}-only --> block opened on line {opened} is never closed"),
        });
    }
    if !had_markers {
        return Ok(Stripped {
            bytes: data.to_vec(),
            had_markers: false,
        });
    }
    Ok(Stripped {
        bytes: out.concat(),
        had_markers: true,
    })
}

// ---- source trees -------------------------------------------------------

/// One entry of a source tree, as a served copy would hold it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeEntry {
    Dir {
        rel: PathBuf,
    },
    /// A regular file; `path` is where to read it (possibly through a link
    /// the walk followed), `mode` its permission bits.
    File {
        rel: PathBuf,
        path: PathBuf,
        mode: u32,
    },
}

impl TreeEntry {
    pub fn rel(&self) -> &Path {
        match self {
            Self::Dir { rel } | Self::File { rel, .. } => rel,
        }
    }
}

/// Something in a source tree a served copy leaves out, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub rel: PathBuf,
    pub reason: String,
}

/// A source tree as a served copy sees it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceTree {
    /// Pre-order: a directory comes before what it holds.
    pub entries: Vec<TreeEntry>,
    pub skipped: Vec<Skipped>,
}

/// Walk `root` the way a served copy is built. `root` is the skill's library
/// entry `skill` or a variant tree inside it.
///
/// The symlink policy: a link inside the tree is followed (its target's bytes
/// are what a copy holds) only when its resolved target lies inside the
/// skill's own resolved tree or inside the resolved library, so a link to
/// `~/.ssh/id_ed25519` is never materialised into a surface. Such a link, a
/// directory link back to one of its own ancestors (every directory from the
/// walk root up through the skill and the library, so `up -> ..` never pulls
/// in the rest of the pool or of the skill), a directory link to the library
/// root, and anything that is neither a file nor a directory are left out and
/// reported in `skipped`. A dangling link is left out silently, as the Python
/// stripper's copy did. A walk root that resolves outside the skill and the
/// library is refused.
pub fn walk_source(root: &Path, skill: &Path, library: &Path) -> Result<SourceTree> {
    let real_root = fs::canonicalize(root)
        .map_err(|err| Error::io(&format!("resolving {}", root.display()), &err))?;
    let real_skill = fs::canonicalize(skill)
        .map_err(|err| Error::io(&format!("resolving {}", skill.display()), &err))?;
    let real_library = fs::canonicalize(library).ok();
    let mut allowed = vec![real_skill];
    allowed.extend(real_library.clone());
    if !allowed.iter().any(|root| real_root.starts_with(root)) {
        return Err(Error::internal(format!(
            "{} resolves to {}, outside its skill and the library; it is never walked",
            root.display(),
            real_root.display()
        )));
    }
    // Every ancestor of the walk root still inside an allowed root, and the
    // library root itself even when the skill lives elsewhere.
    let mut ancestors: Vec<PathBuf> = real_root
        .ancestors()
        .filter(|ancestor| allowed.iter().any(|root| ancestor.starts_with(root)))
        .map(Path::to_path_buf)
        .collect();
    if let Some(real_library) = real_library
        && !ancestors.contains(&real_library)
    {
        ancestors.push(real_library);
    }
    let mut tree = SourceTree::default();
    walk_dir(root, Path::new(""), &allowed, &mut ancestors, &mut tree)?;
    Ok(tree)
}

fn walk_dir(
    dir: &Path,
    rel: &Path,
    allowed: &[PathBuf],
    ancestors: &mut Vec<PathBuf>,
    tree: &mut SourceTree,
) -> Result<()> {
    let read =
        fs::read_dir(dir).map_err(|err| Error::io(&format!("reading {}", dir.display()), &err))?;
    let mut children: Vec<(std::ffi::OsString, PathBuf)> = Vec::new();
    for dirent in read {
        let dirent =
            dirent.map_err(|err| Error::io(&format!("reading {}", dir.display()), &err))?;
        children.push((dirent.file_name(), dirent.path()));
    }
    children.sort();

    for (name, path) in children {
        let child_rel = rel.join(&name);
        let meta = fs::symlink_metadata(&path)
            .map_err(|err| Error::io(&format!("reading {}", path.display()), &err))?;
        if meta.file_type().is_symlink() {
            let Ok(real) = fs::canonicalize(&path) else {
                continue;
            };
            if !allowed.iter().any(|root| real.starts_with(root)) {
                tree.skipped.push(Skipped {
                    rel: child_rel,
                    reason: format!(
                        "a link to {}, outside the skill and the library",
                        real.display()
                    ),
                });
                continue;
            }
            let Ok(target) = fs::metadata(&path) else {
                continue;
            };
            if target.is_dir() {
                if ancestors.contains(&real) {
                    tree.skipped.push(Skipped {
                        rel: child_rel,
                        reason: format!("a link back to its own ancestor {}", real.display()),
                    });
                    continue;
                }
                tree.entries.push(TreeEntry::Dir {
                    rel: child_rel.clone(),
                });
                ancestors.push(real);
                walk_dir(&path, &child_rel, allowed, ancestors, tree)?;
                ancestors.pop();
            } else if target.is_file() {
                tree.entries.push(TreeEntry::File {
                    rel: child_rel,
                    path,
                    mode: target.permissions().mode() & 0o7777,
                });
            } else {
                tree.skipped.push(Skipped {
                    rel: child_rel,
                    reason: "a link to something that is neither a file nor a directory".into(),
                });
            }
        } else if meta.is_dir() {
            let real = fs::canonicalize(&path)
                .map_err(|err| Error::io(&format!("resolving {}", path.display()), &err))?;
            tree.entries.push(TreeEntry::Dir {
                rel: child_rel.clone(),
            });
            ancestors.push(real);
            walk_dir(&path, &child_rel, allowed, ancestors, tree)?;
            ancestors.pop();
        } else if meta.is_file() {
            tree.entries.push(TreeEntry::File {
                rel: child_rel,
                path,
                mode: meta.permissions().mode() & 0o7777,
            });
        } else {
            tree.skipped.push(Skipped {
                rel: child_rel,
                reason: "neither a file nor a directory".into(),
            });
        }
    }
    Ok(())
}

/// True for a file the stripper rewrites: a `.md` file not inside a hidden
/// directory (a hidden `.md` file itself is stripped).
///
/// Names are judged by their bytes, so a file whose name is not UTF-8 (legal
/// on Linux) is still a candidate when it ends in `.md`, as it was for the
/// Python stripper.
pub fn is_strip_candidate(rel: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let named_md = rel
        .file_name()
        .is_some_and(|name| name.as_bytes().ends_with(b".md"));
    let hidden_dir = rel.parent().is_some_and(|parent| {
        parent.components().any(|part| match part {
            Component::Normal(text) => text.as_bytes().starts_with(b"."),
            _ => false,
        })
    });
    named_md && !hidden_dir
}

/// One file of a served copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyItem {
    Dir {
        rel: PathBuf,
    },
    File {
        rel: PathBuf,
        bytes: Vec<u8>,
        mode: u32,
    },
}

/// A served copy, ready to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedCopy {
    pub items: Vec<CopyItem>,
    pub skipped: Vec<Skipped>,
    /// [`source_hash`] of the source, the copy's main freshness key.
    pub source_hash: String,
}

/// What a source needs on a surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prepared {
    /// No marker anywhere: the surface serves a link to the source.
    Plain,
    /// Markers: the surface serves this stripped copy.
    Fenced(ServedCopy),
}

/// One malformed file of a source: its path relative to the source, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    pub rel: PathBuf,
    pub error: MarkerError,
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.rel.display(), self.error)
    }
}

/// Decide what `source` needs on a surface rendered for `keep` and `harness`,
/// and build the stripped copy when it needs one. `skill` is the skill's
/// library entry; `source` is that entry or a variant tree inside it.
///
/// `Ok(Err(errors))` lists every malformed file (one error per file, sorted by
/// path); nothing is served for such a source.
pub fn prepare(
    source: &Path,
    skill: &Path,
    library: &Path,
    keep: Platform,
    harness: Option<&str>,
) -> Result<std::result::Result<Prepared, Vec<FileError>>> {
    let tree = walk_source(source, skill, library)?;
    let mut errors: Vec<FileError> = Vec::new();
    let mut stripped: Vec<Option<Vec<u8>>> = vec![None; tree.entries.len()];
    let mut fenced = false;
    for (index, entry) in tree.entries.iter().enumerate() {
        let TreeEntry::File { rel, path, .. } = entry else {
            continue;
        };
        if !is_strip_candidate(rel) {
            continue;
        }
        let data = fs::read(path)
            .map_err(|err| Error::io(&format!("reading {}", path.display()), &err))?;
        match strip_bytes(&data, keep, harness) {
            Ok(result) => {
                fenced |= result.had_markers;
                stripped[index] = Some(result.bytes);
            }
            Err(error) => errors.push(FileError {
                rel: rel.clone(),
                error,
            }),
        }
    }
    if !errors.is_empty() {
        errors.sort_by(|left, right| left.rel.cmp(&right.rel));
        return Ok(Err(errors));
    }
    if !fenced {
        return Ok(Ok(Prepared::Plain));
    }

    let mut items = Vec::with_capacity(tree.entries.len());
    for (entry, stripped) in tree.entries.into_iter().zip(stripped) {
        match entry {
            TreeEntry::Dir { rel } => items.push(CopyItem::Dir { rel }),
            TreeEntry::File { rel, path, mode } => {
                let bytes = match stripped {
                    Some(bytes) => bytes,
                    None => fs::read(&path)
                        .map_err(|err| Error::io(&format!("reading {}", path.display()), &err))?,
                };
                items.push(CopyItem::File { rel, bytes, mode });
            }
        }
    }
    Ok(Ok(Prepared::Fenced(ServedCopy {
        items,
        skipped: tree.skipped,
        source_hash: source_hash(source)?,
    })))
}

/// SHA-256 over the sorted `(relative path, kind, content | link text)` of a
/// tree, links recorded by their text and never followed, hidden files
/// included: the Python stripper's freshness hash, byte for byte.
pub fn source_hash(root: &Path) -> Result<String> {
    let mut entries: Vec<(Vec<u8>, &'static str, Vec<u8>)> = Vec::new();
    hash_walk(root, Path::new(""), &mut entries)?;
    entries.sort();
    let mut hasher = Sha256::new();
    for (rel, kind, payload) in &entries {
        hasher.update(format!("{}:", rel.len()).as_bytes());
        hasher.update(rel);
        hasher.update(b"\0");
        hasher.update(kind.as_bytes());
        hasher.update(b"\0");
        hasher.update(format!("{}:", payload.len()).as_bytes());
        hasher.update(payload);
    }
    Ok(hasher.hex())
}

fn hash_walk(
    dir: &Path,
    rel: &Path,
    entries: &mut Vec<(Vec<u8>, &'static str, Vec<u8>)>,
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let Ok(read) = fs::read_dir(dir) else {
        return Ok(());
    };
    if !rel.as_os_str().is_empty() {
        entries.push((rel.as_os_str().as_bytes().to_vec(), "dir", Vec::new()));
    }
    let mut children: Vec<(std::ffi::OsString, PathBuf)> = read
        .filter_map(|dirent| dirent.ok())
        .map(|dirent| (dirent.file_name(), dirent.path()))
        .collect();
    children.sort();
    for (name, path) in children {
        let child_rel = rel.join(&name);
        let key = child_rel.as_os_str().as_bytes().to_vec();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            let text = fs::read_link(&path)
                .map(|text| text.as_os_str().as_bytes().to_vec())
                .unwrap_or_default();
            entries.push((key, "symlink", text));
        } else if meta.is_dir() {
            hash_walk(&path, &child_rel, entries)?;
        } else if meta.is_file() {
            match fs::read(&path) {
                Ok(bytes) => entries.push((key, "file", bytes)),
                Err(_) => entries.push((key, "unreadable", Vec::new())),
            }
        } else {
            entries.push((key, "unreadable", Vec::new()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(input: &str, keep: Platform, harness: Option<&str>) -> String {
        String::from_utf8(strip_bytes(input.as_bytes(), keep, harness).unwrap().bytes).unwrap()
    }

    #[test]
    fn markers_parse_only_as_whole_lines_of_known_names() {
        assert_eq!(
            parse_marker(b"  <!-- mac-only -->\n"),
            Some(Marker {
                axis: Axis::Machine,
                name: "mac".into(),
                open: true
            })
        );
        assert_eq!(
            parse_marker(b"<!--/ claude-only-->"),
            Some(Marker {
                axis: Axis::Harness,
                name: "claude".into(),
                open: false
            })
        );
        for line in [
            &b"see <!-- mac-only --> inline"[..],
            b"<!-- gemini-only -->",
            b"<!-- mac-only --> trailing",
            b"<!-->",
            b"<!-- mac -->",
            b"<!-- \xc3\xa9-only -->",
        ] {
            assert_eq!(
                parse_marker(line),
                None,
                "{}",
                String::from_utf8_lossy(line)
            );
        }
    }

    #[test]
    fn a_file_without_markers_is_returned_byte_for_byte() {
        let input = b"# A\r\n\n\n\n# B\n<!-- mac -->\n";
        let result = strip_bytes(input, Platform::Linux, None).unwrap();
        assert!(!result.had_markers);
        assert_eq!(result.bytes, input);
    }

    #[test]
    fn the_harness_axis_keeps_only_the_named_harness() {
        let input = "a\n<!-- claude-only -->\nC\n<!-- /claude-only -->\n<!-- codex-only -->\nX\n<!-- /codex-only -->\nz\n";
        assert_eq!(strip(input, Platform::Mac, Some("claude")), "a\nC\nz\n");
        assert_eq!(strip(input, Platform::Mac, Some("codex")), "a\nX\nz\n");
        assert_eq!(strip(input, Platform::Mac, None), "a\nz\n");
        assert_eq!(
            strip(input, Platform::Mac, Some("gemini")),
            "a\nz\n",
            "a harness key no marker can name keeps no harness block"
        );
    }

    #[test]
    fn strip_candidates_are_md_files_outside_hidden_directories() {
        assert!(is_strip_candidate(Path::new("SKILL.md")));
        assert!(is_strip_candidate(Path::new("references/x.md")));
        assert!(is_strip_candidate(Path::new(".hidden.md")));
        assert!(!is_strip_candidate(Path::new(".variants/claude/SKILL.md")));
        assert!(!is_strip_candidate(Path::new("a/.git/x.md")));
        assert!(!is_strip_candidate(Path::new("notes.txt")));
    }

    /// A name that is not UTF-8 is judged by its bytes: `caf\xe9.md` is a
    /// candidate, `caf\xe9.txt` is not, and a hidden non-UTF-8 directory
    /// still hides what it holds.
    #[test]
    fn a_non_utf8_md_name_is_a_strip_candidate() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        assert!(is_strip_candidate(Path::new(OsStr::from_bytes(
            b"caf\xe9.md"
        ))));
        assert!(is_strip_candidate(Path::new(OsStr::from_bytes(
            b"d\xff/x.md"
        ))));
        assert!(!is_strip_candidate(Path::new(OsStr::from_bytes(
            b"caf\xe9.txt"
        ))));
        assert!(!is_strip_candidate(Path::new(OsStr::from_bytes(
            b".\xff/x.md"
        ))));
    }

    /// On Linux such a file can exist: its markers are stripped in the served
    /// copy like any other `.md` file's. (macOS file systems refuse names
    /// that are not UTF-8, so this half runs on Linux only.)
    #[cfg(target_os = "linux")]
    #[test]
    fn a_non_utf8_md_file_is_stripped_in_the_served_copy() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let home = tempfile::TempDir::new().unwrap();
        let library = home.path().join("library");
        let skill = library.join("skill");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "# skill\n").unwrap();
        let name = OsStr::from_bytes(b"caf\xe9.md");
        fs::write(
            skill.join(name),
            "A\n<!-- claude-only -->\nC\n<!-- /claude-only -->\nB\n",
        )
        .unwrap();
        let Prepared::Fenced(copy) =
            prepare(&skill, &skill, &library, Platform::Mac, Some("codex"))
                .unwrap()
                .unwrap()
        else {
            panic!("the non-UTF-8 file's markers make the skill fenced");
        };
        let bytes = copy
            .items
            .iter()
            .find_map(|item| match item {
                CopyItem::File { rel, bytes, .. } if rel.as_os_str() == name => Some(bytes.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(bytes, b"A\nB\n");
    }
}
