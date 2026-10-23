//! SHA-256 and the deterministic tree hash.
//!
//! `tree_hash` fingerprints a directory by content, never by time: sorted
//! relative paths, a kind marker, the executable bit for files, and bytes.
//! Symlinks hash their target text; `.git`, `.variants` and `.harness-agents`
//! are skipped.
//!
//! `committed_tree_hash` is the fingerprint a pool tree keeps across machines:
//! the same hash minus every path the library's git would leave uncommitted.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use sha2::Digest;
use walkdir::WalkDir;

use crate::error::{Error, Result};

/// Streaming SHA-256 (the `sha2` crate), so hashing a tree never
/// concatenates its bytes first.
#[derive(Debug, Clone, Default)]
pub struct Sha256 {
    inner: sha2::Sha256,
}

impl Sha256 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, data: &[u8]) {
        Digest::update(&mut self.inner, data);
    }

    pub fn update_str(&mut self, data: &str) {
        self.update(data.as_bytes());
    }

    pub fn finish(self) -> [u8; 32] {
        self.inner.finalize().into()
    }

    pub fn hex(self) -> String {
        hex(&self.finish())
    }
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// SHA-256 of `data`, lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.hex()
}

/// Content hash of a directory tree. Timestamps never enter the hash.
pub fn tree_hash(dir: &Path) -> Result<String> {
    tree_hash_skipping(dir, &[])
}

/// `tree_hash` that also leaves out every entry named in `skip`, at any
/// depth. A tree copied by `variants::copy_tree` omits the same names, so
/// hashing its upstream source with the same `skip` gives the hash the copy
/// will have; hashing the source without it records a tree that never lands.
pub fn tree_hash_skipping(dir: &Path, skip: &[&str]) -> Result<String> {
    let entries = walk(dir, skip)?;
    hash_entries(dir, &entries)
}

/// The hash a pool tree carries between machines: `tree_hash_skipping` minus
/// every path the library's git would not commit — the root `.gitignore`, the
/// tree's own `.gitignore` files, and the index (a tracked file that matches a
/// pattern still counts, because git carries it). `as_at` is the tree's path
/// relative to the library root: its real place for a pool entry, the place it
/// will land for an arrival, because the rules are anchored there. A tree that
/// is ignored wholesale (a bridge name in the root `.gitignore`) hashes in
/// full: that rule says the entry is not the pool's, not that anything inside
/// it is junk. A library that is not a git checkout hashes everything. A
/// directory left with nothing in it is dropped as well: git never carries an
/// empty directory, so a clone has no entry for it.
///
/// Exclude generated fixtures and `__pycache__/` when git ignores them;
/// hashing files git never delivers would misreport another clone as edited.
pub fn committed_tree_hash(
    library: &Path,
    as_at: &str,
    dir: &Path,
    skip: &[&str],
) -> Result<String> {
    let entries = walk(dir, skip)?;
    let ignored = git_ignored(library, as_at, dir, &entries)?;
    let kept: Vec<PathBuf> = entries
        .into_iter()
        .filter(|path| !path.ancestors().any(|ancestor| ignored.contains(ancestor)))
        .collect();
    let occupied: Vec<&PathBuf> = kept
        .iter()
        .filter(|path| {
            !fs::symlink_metadata(path)
                .map(|meta| meta.is_dir())
                .unwrap_or(false)
        })
        .collect();
    let kept: Vec<PathBuf> = kept
        .iter()
        .filter(|path| {
            let is_dir = fs::symlink_metadata(path)
                .map(|meta| meta.is_dir())
                .unwrap_or(false);
            !is_dir || occupied.iter().any(|leaf| leaf.starts_with(path))
        })
        .cloned()
        .collect();
    hash_entries(dir, &kept)
}

/// The walked paths under `dir` (as `<as_at>/<relative>`) that `git
/// check-ignore` in `library` reports, as absolute paths under `dir`. Empty
/// when nothing is ignored, when `library` is not a git checkout, or when
/// `as_at` itself is ignored.
fn git_ignored(
    library: &Path,
    as_at: &str,
    dir: &Path,
    entries: &[PathBuf],
) -> Result<BTreeSet<PathBuf>> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    if entries.is_empty() || !library.join(".git").exists() {
        return Ok(BTreeSet::new());
    }
    let prefix = as_at.trim_end_matches('/');
    let mut stdin_bytes: Vec<u8> = Vec::new();
    stdin_bytes.extend_from_slice(prefix.as_bytes());
    stdin_bytes.push(0);
    for path in entries {
        let relative = path
            .strip_prefix(dir)
            .map_err(|_| Error::internal("walked path escaped its root"))?;
        stdin_bytes.extend_from_slice(prefix.as_bytes());
        stdin_bytes.push(b'/');
        stdin_bytes.extend_from_slice(relative.as_os_str().as_bytes());
        stdin_bytes.push(0);
    }

    let mut child = match crate::git_command()
        .arg("-C")
        .arg(library)
        .args(["check-ignore", "-z", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return Err(Error::io("running git check-ignore", &err)),
    };
    {
        let mut stdin = child.stdin.take().expect("piped stdin");
        // A closed pipe means git stopped reading (not a repository); the exit
        // status below says so.
        let _ = stdin.write_all(&stdin_bytes);
    }
    let output = child
        .wait_with_output()
        .map_err(|err| Error::io("waiting for git check-ignore", &err))?;
    match output.status.code() {
        Some(0) => {}
        // 1: nothing ignored. Anything else: not a git checkout, or a git
        // too old to answer; the tree then hashes in full.
        _ => return Ok(BTreeSet::new()),
    }

    let mut ignored = BTreeSet::new();
    for reported in output.stdout.split(|byte| *byte == 0) {
        if reported.is_empty() {
            continue;
        }
        if reported == prefix.as_bytes() {
            return Ok(BTreeSet::new());
        }
        let Some(rest) = reported.strip_prefix(prefix.as_bytes()) else {
            continue;
        };
        let Some(relative) = rest.strip_prefix(b"/") else {
            continue;
        };
        let relative = PathBuf::from(std::ffi::OsString::from_vec(relative.to_vec()));
        ignored.insert(dir.join(relative));
    }
    Ok(ignored)
}

/// Every path under `dir` except the skipped names, sorted.
fn walk(dir: &Path, skip: &[&str]) -> Result<Vec<PathBuf>> {
    let meta = fs::symlink_metadata(dir).map_err(|_| Error::input_not_found(dir))?;
    if !meta.is_dir() {
        return Err(Error::input_not_found_reason(dir, "is not a directory"));
    }

    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !is_skipped(name.as_ref()) && !skip.contains(&name.as_ref())
        })
    {
        let entry =
            entry.map_err(|err| Error::internal(format!("walking {}: {err}", dir.display())))?;
        let path = entry.path();
        if path == dir {
            continue;
        }
        entries.push(path.to_path_buf());
    }
    entries.sort();
    Ok(entries)
}

fn hash_entries(dir: &Path, entries: &[PathBuf]) -> Result<String> {
    let mut hasher = Sha256::new();
    for path in entries {
        let relative = path
            .strip_prefix(dir)
            .map_err(|_| Error::internal("walked path escaped its root"))?
            .to_string_lossy()
            .into_owned();
        let meta =
            fs::symlink_metadata(path).map_err(|err| Error::io("reading a tree entry", &err))?;
        if meta.file_type().is_symlink() {
            let target = fs::read_link(path).map_err(|err| Error::io("reading a link", &err))?;
            hasher.update_str("link\0");
            hasher.update_str(&relative);
            hasher.update_str("\0");
            hasher.update_str(&target.to_string_lossy());
            hasher.update_str("\0");
        } else if meta.is_dir() {
            hasher.update_str("dir\0");
            hasher.update_str(&relative);
            hasher.update_str("\0");
        } else if meta.is_file() {
            let bytes = fs::read(path).map_err(|err| Error::io("reading a file", &err))?;
            hasher.update_str("file\0");
            hasher.update_str(&relative);
            hasher.update_str("\0");
            hasher.update_str(if is_executable(&meta) { "x" } else { "-" });
            hasher.update_str("\0");
            hasher.update_str(&bytes.len().to_string());
            hasher.update_str("\0");
            hasher.update(&bytes);
            hasher.update_str("\0");
        } else {
            hasher.update_str("other\0");
            hasher.update_str(&relative);
            hasher.update_str("\0");
        }
    }
    Ok(hasher.hex())
}

fn is_skipped(name: &str) -> bool {
    name == ".git" || name == ".variants" || name == crate::agents::AGENTS_DIR
}

#[cfg(unix)]
fn is_executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn sha256_matches_the_published_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sha256_hex(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        assert_eq!(
            sha256_hex(b"The quick brown fox jumps over the lazy dog"),
            "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
        );
        // 112 bytes: the two-block padding path (length lands past the buffer).
        assert_eq!(
            sha256_hex(
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
            ),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    #[test]
    fn streaming_matches_one_shot() {
        let data: Vec<u8> = (0..5000u32).map(|index| (index % 251) as u8).collect();
        let expected = sha256_hex(&data);
        for chunk in [1usize, 3, 63, 64, 65, 127] {
            let mut hasher = Sha256::new();
            for piece in data.chunks(chunk) {
                hasher.update(piece);
            }
            assert_eq!(hasher.hex(), expected, "chunk size {chunk}");
        }
    }

    #[test]
    fn creation_order_does_not_change_the_hash() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        let a = first.path().join("skill");
        let b = second.path().join("skill");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("SKILL.md"), "one").unwrap();
        fs::write(b.join("SKILL.md"), "one").unwrap();
        fs::create_dir_all(a.join("nested")).unwrap();
        fs::write(a.join("nested/x"), "x").unwrap();
        fs::create_dir_all(b.join("nested")).unwrap();
        fs::write(b.join("nested/x"), "x").unwrap();
        assert_eq!(tree_hash(&a).unwrap(), tree_hash(&b).unwrap());
    }

    #[test]
    fn a_skipped_tree_matches_the_copy_that_omits_it() {
        let dir = TempDir::new().unwrap();
        let full = dir.path().join("full");
        let trimmed = dir.path().join("trimmed");
        for root in [&full, &trimmed] {
            fs::create_dir_all(root.join("scripts")).unwrap();
            fs::write(root.join("SKILL.md"), "one").unwrap();
            fs::write(root.join("scripts/core.py"), "print()").unwrap();
        }
        fs::create_dir_all(full.join("scripts/tests")).unwrap();
        fs::write(full.join("scripts/tests/bench.md"), "noise").unwrap();
        assert_ne!(tree_hash(&full).unwrap(), tree_hash(&trimmed).unwrap());
        assert_eq!(
            tree_hash_skipping(&full, &["tests"]).unwrap(),
            tree_hash(&trimmed).unwrap()
        );
        assert_eq!(
            tree_hash_skipping(&trimmed, &["tests"]).unwrap(),
            tree_hash(&trimmed).unwrap(),
            "skipping a name that is absent changes nothing"
        );
    }

    #[test]
    fn one_byte_edit_changes_the_hash() {
        let dir = TempDir::new().unwrap();
        let skill = dir.path().join("skill");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "one").unwrap();
        let before = tree_hash(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "two").unwrap();
        assert_ne!(before, tree_hash(&skill).unwrap());
    }

    #[test]
    fn variants_agents_and_git_are_ignored() {
        let dir = TempDir::new().unwrap();
        let skill = dir.path().join("skill");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "one").unwrap();
        let before = tree_hash(&skill).unwrap();
        fs::create_dir_all(skill.join(".variants/claude")).unwrap();
        fs::write(skill.join(".variants/claude/SKILL.md"), "variant").unwrap();
        fs::create_dir_all(skill.join(".harness-agents/claude")).unwrap();
        fs::write(
            skill.join(".harness-agents/claude/skill-helper.md"),
            "agent",
        )
        .unwrap();
        fs::create_dir_all(skill.join(".git")).unwrap();
        fs::write(skill.join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        assert_eq!(before, tree_hash(&skill).unwrap());
        fs::write(skill.join("extra.md"), "extra").unwrap();
        assert_ne!(before, tree_hash(&skill).unwrap());
    }

    #[test]
    fn symlinks_hash_their_target_text() {
        let dir = TempDir::new().unwrap();
        let skill = dir.path().join("skill");
        fs::create_dir_all(&skill).unwrap();
        fs::write(dir.path().join("one"), "payload").unwrap();
        fs::write(dir.path().join("two"), "payload").unwrap();
        std::os::unix::fs::symlink("../one", skill.join("link")).unwrap();
        let before = tree_hash(&skill).unwrap();
        fs::remove_file(skill.join("link")).unwrap();
        std::os::unix::fs::symlink("../two", skill.join("link")).unwrap();
        assert_ne!(
            before,
            tree_hash(&skill).unwrap(),
            "a retargeted link changes the hash"
        );
        fs::remove_file(skill.join("link")).unwrap();
        std::os::unix::fs::symlink("../one", skill.join("link")).unwrap();
        assert_eq!(
            before,
            tree_hash(&skill).unwrap(),
            "target text is the sole input"
        );
    }

    #[test]
    fn the_executable_bit_changes_the_hash() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let skill = dir.path().join("skill");
        fs::create_dir_all(&skill).unwrap();
        let file = skill.join("run.sh");
        fs::write(&file, "exit 0\n").unwrap();
        let before = tree_hash(&skill).unwrap();
        let mut mode = fs::metadata(&file).unwrap().permissions();
        mode.set_mode(0o755);
        fs::set_permissions(&file, mode).unwrap();
        assert_ne!(before, tree_hash(&skill).unwrap());
    }

    fn git(repo: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// A library with a root `.gitignore` and a tracked entry `clean` whose
    /// tree the other trees are compared against.
    fn library(dir: &Path) -> PathBuf {
        let library = dir.join("library");
        fs::create_dir_all(&library).unwrap();
        fs::write(library.join(".gitignore"), "bridge\n__pycache__/\n*.pyc\n").unwrap();
        git(&library, &["init", "-q"]);
        library
    }

    fn plant(root: &Path, junk: bool) {
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("SKILL.md"), "one").unwrap();
        fs::write(root.join("scripts/run.py"), "print()").unwrap();
        fs::write(root.join(".gitignore"), "transcript.json\n").unwrap();
        if junk {
            fs::create_dir_all(root.join("scripts/__pycache__")).unwrap();
            fs::write(root.join("scripts/__pycache__/run.pyc"), "bytes").unwrap();
            fs::create_dir_all(root.join("fixtures")).unwrap();
            fs::write(root.join("fixtures/transcript.json"), "{}").unwrap();
        }
    }

    #[test]
    fn the_committed_hash_leaves_out_what_git_would_not_commit() {
        let dir = TempDir::new().unwrap();
        let library = library(dir.path());
        let clean = library.join("clean");
        plant(&clean, false);
        let entry = library.join("skill");
        plant(&entry, true);
        assert_ne!(
            tree_hash(&entry).unwrap(),
            tree_hash(&clean).unwrap(),
            "the junk is real content to the plain hash"
        );
        assert_eq!(
            committed_tree_hash(&library, "skill", &entry, &[]).unwrap(),
            tree_hash(&clean).unwrap(),
            "the root rule drops __pycache__ and the tree's own rule drops the fixture"
        );

        // An arrival outside the library, hashed as if it sat at `skill`.
        let arrival = dir.path().join("inbox/skill");
        plant(&arrival, true);
        assert_eq!(
            committed_tree_hash(&library, "skill", &arrival, &[]).unwrap(),
            tree_hash(&clean).unwrap(),
            "the arrival hashes as the pool copy will"
        );
        // …and as if it sat at a name the pool has never seen: the root rule
        // still applies, the tree's own `.gitignore` only once it is in the pool.
        let fresh = committed_tree_hash(&library, "never-seen", &arrival, &[]).unwrap();
        assert_ne!(
            fresh,
            tree_hash(&arrival).unwrap(),
            "__pycache__ is dropped"
        );
        assert_ne!(fresh, tree_hash(&clean).unwrap(), "the fixture is not");
    }

    #[test]
    fn a_tracked_file_that_matches_a_rule_still_counts() {
        let dir = TempDir::new().unwrap();
        let library = library(dir.path());
        let entry = library.join("skill");
        plant(&entry, true);
        git(&library, &["add", "-f", "skill/fixtures/transcript.json"]);
        let clean = library.join("clean");
        plant(&clean, false);
        fs::create_dir_all(clean.join("fixtures")).unwrap();
        fs::write(clean.join("fixtures/transcript.json"), "{}").unwrap();
        assert_eq!(
            committed_tree_hash(&library, "skill", &entry, &[]).unwrap(),
            tree_hash(&clean).unwrap()
        );
        fs::write(entry.join("fixtures/transcript.json"), "edited").unwrap();
        assert_ne!(
            committed_tree_hash(&library, "skill", &entry, &[]).unwrap(),
            tree_hash(&clean).unwrap(),
            "an edit to a force-added file is still an edit"
        );
    }

    #[test]
    fn an_entry_ignored_wholesale_hashes_in_full() {
        let dir = TempDir::new().unwrap();
        let library = library(dir.path());
        let bridge = library.join("bridge");
        plant(&bridge, true);
        assert_eq!(
            committed_tree_hash(&library, "bridge", &bridge, &[]).unwrap(),
            tree_hash(&bridge).unwrap()
        );
    }

    #[test]
    fn a_library_that_is_not_a_checkout_hashes_in_full() {
        let dir = TempDir::new().unwrap();
        let library = dir.path().join("plain");
        let entry = library.join("skill");
        plant(&entry, true);
        assert_eq!(
            committed_tree_hash(&library, "skill", &entry, &[]).unwrap(),
            tree_hash(&entry).unwrap()
        );
    }

    #[test]
    fn the_committed_hash_honours_skipped_names_too() {
        let dir = TempDir::new().unwrap();
        let library = library(dir.path());
        let entry = library.join("skill");
        plant(&entry, true);
        fs::create_dir_all(entry.join("tests")).unwrap();
        fs::write(entry.join("tests/t.md"), "t").unwrap();
        let clean = library.join("clean");
        plant(&clean, false);
        assert_eq!(
            committed_tree_hash(&library, "skill", &entry, &["tests"]).unwrap(),
            tree_hash(&clean).unwrap()
        );
    }

    #[test]
    fn an_emptied_directory_is_dropped_like_git_drops_it() {
        let dir = TempDir::new().unwrap();
        let library = library(dir.path());
        let entry = library.join("skill");
        plant(&entry, false);
        fs::create_dir_all(entry.join("empty")).unwrap();
        let clean = library.join("clean");
        plant(&clean, false);
        assert_ne!(tree_hash(&entry).unwrap(), tree_hash(&clean).unwrap());
        assert_eq!(
            committed_tree_hash(&library, "skill", &entry, &[]).unwrap(),
            tree_hash(&clean).unwrap()
        );
    }

    #[test]
    fn a_missing_directory_is_input_not_found() {
        let dir = TempDir::new().unwrap();
        let err = tree_hash(&dir.path().join("nope")).unwrap_err();
        assert_eq!(err.code(), "input_not_found");
    }
}
