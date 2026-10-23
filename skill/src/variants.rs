//! Per-harness upstream variants (lane T4).
//!
//! A variant is the copy of a skill an upstream repo ships for one harness
//! (`.claude/skills/<n>`, `.pi/skills/<n>`, …). Reconcile compares each against
//! the universal copy and stores the differing ones under
//! `library/<n>/.variants/<harness>/`, recording a hash per variant.
//!
//! The clone is shallow, single-branch and lives inside the library's
//! gitignored scratch dir (`library/.npx/tmp/`), so nothing of it reaches the
//! pool's history. A clone that cannot be made is not an import failure: it
//! leaves `variantsPending` set, and reconcile retries every pending record on
//! the next run.
//!
//! This module also owns [`copy_tree`], the one tree copy reconcile, the stage
//! and variant storage share.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::hash::committed_tree_hash;
use crate::model::{NpxMeta, PlanOp, Refusal};
use crate::npx::remove_path;
use crate::paths::Paths;

/// The upstream harness dirs a variant can come from, and the variant key each
/// one is stored under. `.agents` is absent on purpose: that tree is the
/// universal copy npx already installed.
pub const HARNESS_DIRS: [(&str, &str); 5] = [
    (".claude", "claude"),
    (".codex", "codex"),
    (".cursor", "cursor"),
    (".grok", "grok"),
    (".pi", "pi"),
];

/// Development-only trees an upstream skill may carry; never copied into the
/// pool.
pub const SKIPPED_TREES: [&str; 2] = ["tests", "node_modules"];

/// What one variant pass produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantOutcome {
    /// Harness key → tree hash of the stored variant, after the pass.
    pub variants: BTreeMap<String, String>,
    /// The upstream commit the variants came from; empty when unknown.
    pub revision: String,
    /// True when upstream could not be inspected and the record must be retried.
    pub pending: bool,
    /// Why the pass is pending.
    pub reason: Option<String>,
    /// Harness → tree hash of the stored upstream agents, after the pass
    /// (`agents::import_from_clone`, run from the same clone).
    pub harness_agents: BTreeMap<String, String>,
    /// Variants and agent trees left alone because they carry local edits.
    pub refusals: Vec<Refusal>,
    /// What a `--dry-run` would have done.
    pub plan: Vec<PlanOp>,
}

/// Keep an upstream URL out of git's option parser, even when it starts `-`.
fn clone_args(record: &NpxMeta, destination: &str) -> Vec<String> {
    let mut args = vec!["clone", "--depth", "1", "--quiet"];
    if let Some(reference) = record
        .r#ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        args.push("--branch");
        args.push(reference);
    }
    args.push("--");
    args.push(&record.source_url);
    args.push(destination);
    args.into_iter().map(str::to_string).collect()
}

fn symlink(target: &Path, link: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
            .map_err(|err| Error::io(&format!("linking {}", link.display()), &err))
    }
    #[cfg(not(unix))]
    {
        let _ = (target, link);
        Err(Error::internal(
            "copying a tree containing symlinks needs unix symlink support",
        ))
    }
}

/// Replace `to` with a byte-for-byte copy of `from`, skipping `skip` trees.
pub fn copy_tree(from: &Path, to: &Path, skip: &[&str]) -> Result<()> {
    remove_path(to)?;
    copy_dir(from, to, skip)
}

fn copy_dir(from: &Path, to: &Path, skip: &[&str]) -> Result<()> {
    let meta = fs::symlink_metadata(from).map_err(|_| Error::input_not_found(from))?;
    if !meta.is_dir() {
        return Err(Error::input_not_found_reason(from, "is not a directory"));
    }
    fs::create_dir_all(to).map_err(|err| Error::io(&format!("creating {}", to.display()), &err))?;

    let read = fs::read_dir(from)
        .map_err(|err| Error::io(&format!("reading {}", from.display()), &err))?;
    for dirent in read {
        let dirent = dirent.map_err(|err| Error::io("reading a tree entry", &err))?;
        let name = dirent.file_name().to_string_lossy().into_owned();
        if skip.contains(&name.as_str()) {
            continue;
        }
        let source = dirent.path();
        let target = to.join(&name);
        let meta = fs::symlink_metadata(&source)
            .map_err(|err| Error::io(&format!("reading {}", source.display()), &err))?;
        if meta.file_type().is_symlink() {
            let link = fs::read_link(&source).map_err(|err| {
                Error::io(&format!("reading the link {}", source.display()), &err)
            })?;
            symlink(&link, &target)?;
        } else if meta.is_dir() {
            copy_dir(&source, &target, skip)?;
        } else if meta.is_file() {
            fs::copy(&source, &target)
                .map_err(|err| Error::io(&format!("copying {}", source.display()), &err))?;
        } else {
            return Err(Error::internal(format!(
                "{} is neither a file, a directory nor a symlink",
                source.display()
            )));
        }
    }
    Ok(())
}

fn git(args: &[&str]) -> std::io::Result<std::process::Output> {
    crate::git_command().args(args).output()
}

fn failure_line(output: &std::process::Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    let text = crate::npx::redact(&text);
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("git failed")
        .to_string()
}

/// The upstream skill directory name: the basename of the lock entry's
/// `skillPath` parent, falling back to the canonical name.
fn upstream_dir_name(record: &NpxMeta, name: &str) -> String {
    Path::new(&record.skill_path)
        .parent()
        .and_then(Path::file_name)
        .map(|part| part.to_string_lossy().into_owned())
        .filter(|part| !part.is_empty())
        .unwrap_or_else(|| name.to_string())
}

/// `<library>/<name>/.variants/<harness>`.
pub fn stored_variant(paths: &Paths, name: &str, harness: &str) -> PathBuf {
    paths.library.join(name).join(".variants").join(harness)
}

/// Locate `<clone>/<harness dir>/skills/<upstream dir>`, falling back to the
/// canonical name, and require a real skill (a `SKILL.md`).
///
/// Repos commonly publish a harness entry as a symlink to the universal copy
/// (for example, `.claude/skills/example -> ../../skills/example`).
/// Such an entry is resolved to the directory it names, so it hashes like that
/// tree (and a link back to the universal copy is no variant at all); a link
/// that resolves outside the clone is not upstream content and is ignored.
fn find_upstream(clone: &Path, harness_dir: &str, dir_name: &str, name: &str) -> Option<PathBuf> {
    let skills = clone.join(harness_dir).join("skills");
    let root = fs::canonicalize(clone).ok()?;
    [dir_name, name]
        .into_iter()
        .map(|candidate| skills.join(candidate))
        .filter(|candidate| candidate.join("SKILL.md").is_file())
        .find_map(|candidate| {
            let resolved = fs::canonicalize(&candidate).ok()?;
            (resolved.starts_with(&root) && resolved.is_dir()).then_some(resolved)
        })
}

fn is_stored(stored: &Path) -> bool {
    stored.is_dir()
}

/// Store, refresh or drop every upstream variant of one pool entry.
///
/// `baseline` is the universal copy the variants are compared against: the
/// library entry on a real run, and the inbox arrival on a dry run that has not
/// copied it in yet.
pub fn import(
    paths: &Paths,
    name: &str,
    record: &NpxMeta,
    baseline: &Path,
    dry_run: bool,
) -> Result<VariantOutcome> {
    let mut outcome = VariantOutcome {
        variants: record.variants.clone(),
        revision: record.revision.clone(),
        harness_agents: record.harness_agents.clone(),
        pending: false,
        reason: None,
        refusals: Vec::new(),
        plan: Vec::new(),
    };

    if !baseline.is_dir() {
        outcome.pending = true;
        outcome.reason = Some(format!(
            "the universal copy {} is missing",
            baseline.display()
        ));
        return Ok(outcome);
    }
    if record.source_url.trim().is_empty() {
        outcome.pending = true;
        outcome.reason = Some("the lock entry carries no sourceUrl".to_string());
        return Ok(outcome);
    }

    let universal = committed_tree_hash(&paths.library, name, baseline, &SKIPPED_TREES)?;
    let scratch = if dry_run {
        std::env::temp_dir().join(format!(
            "skill-variants-{}-{}",
            std::process::id(),
            &crate::hash::sha256_hex(paths.library.as_os_str().as_encoded_bytes())[..12]
        ))
    } else {
        paths.library.join(".npx/tmp")
    };
    fs::create_dir_all(&scratch)
        .map_err(|err| Error::io(&format!("creating {}", scratch.display()), &err))?;
    let clone = scratch.join(format!("{name}.variants"));
    remove_path(&clone)?;

    let clone_arg = clone.to_string_lossy().into_owned();
    let args = clone_args(record, &clone_arg);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    match git(&argv) {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            outcome.pending = true;
            outcome.reason = Some(failure_line(&output));
            return Ok(outcome);
        }
        Err(err) => {
            outcome.pending = true;
            outcome.reason = Some(format!("failed to run git clone: {err}"));
            return Ok(outcome);
        }
    }

    let revision = git(&["-C", clone.to_string_lossy().as_ref(), "rev-parse", "HEAD"])
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default();
    if !revision.is_empty() {
        outcome.revision = revision;
    }

    let dir_name = upstream_dir_name(record, name);
    for (harness_dir, harness) in HARNESS_DIRS {
        let source = find_upstream(&clone, harness_dir, &dir_name, name);
        // A variant identical to the universal copy is not a variant at all.
        // Both sides are hashed as `copy_tree` will store them and as the pool
        // commits them, so the recorded hash is the stored tree's hash on
        // every clone and `doctor` agrees with it.
        let stored_at = format!("{name}/.variants/{harness}");
        let desired = match &source {
            Some(dir) => {
                let hash = committed_tree_hash(&paths.library, &stored_at, dir, &SKIPPED_TREES)?;
                (hash != universal).then_some(hash)
            }
            None => None,
        };
        let stored = stored_variant(paths, name, harness);
        let current = if is_stored(&stored) {
            Some(committed_tree_hash(
                &paths.library,
                &stored_at,
                &stored,
                &[],
            )?)
        } else {
            None
        };

        match (source, desired) {
            (Some(source), Some(hash)) => {
                if current.as_deref() == Some(hash.as_str()) {
                    outcome.variants.insert(harness.to_string(), hash);
                    continue;
                }
                if let Some(current) = current {
                    if record.variants.get(harness) != Some(&current) {
                        outcome.refusals.push(variant_refusal(name, harness));
                        continue;
                    }
                    outcome.plan.push(PlanOp {
                        op: "refresh".to_string(),
                        path: stored.clone(),
                        target: Some(source.clone()),
                    });
                } else {
                    outcome.plan.push(PlanOp {
                        op: "copy".to_string(),
                        path: stored.clone(),
                        target: Some(source.clone()),
                    });
                }
                if !dry_run {
                    copy_tree(&source, &stored, &SKIPPED_TREES)?;
                }
                outcome.variants.insert(harness.to_string(), hash);
            }
            (None, _) | (Some(_), None) => {
                let Some(current) = current else {
                    continue;
                };
                if record.variants.get(harness) != Some(&current) {
                    outcome.refusals.push(variant_refusal(name, harness));
                    continue;
                }
                outcome.plan.push(PlanOp {
                    op: "remove".to_string(),
                    path: stored.clone(),
                    target: None,
                });
                if !dry_run {
                    remove_path(&stored)?;
                }
                outcome.variants.remove(harness);
            }
        }
    }

    // Upstream agents come from the same clone and carry the same edit guard.
    let agents = crate::agents::import_from_clone(
        paths, name, &dir_name, record, &clone, &scratch, dry_run,
    )?;
    outcome.harness_agents = agents.recorded;
    outcome.refusals.extend(agents.refusals);
    outcome.plan.extend(agents.plan);

    remove_path(&clone)?;
    Ok(outcome)
}

fn variant_refusal(name: &str, harness: &str) -> Refusal {
    Refusal {
        name: name.to_string(),
        code: "local_edit_refused".to_string(),
        message: format!(
            "{name} carries a locally edited .variants/{harness} copy, which no upstream variant will overwrite; `skillherd remove {name} --force` discards it"
        ),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn clone_failure_diagnostics_redact_url_credentials() {
        use std::os::unix::process::ExitStatusExt;
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: Vec::new(),
            stderr: b"fatal: https://user:password@example.invalid/repo denied".to_vec(),
        };
        let line = super::failure_line(&output);
        assert!(!line.contains("user:password"), "{line}");
        assert!(line.contains("example.invalid"));
    }

    use super::*;
    use crate::hash::tree_hash;
    use tempfile::TempDir;

    fn record() -> NpxMeta {
        NpxMeta {
            source: "owner/repo".into(),
            source_type: "github".into(),
            source_url: String::new(),
            r#ref: None,
            skill_path: ".agents/skills/post/SKILL.md".into(),
            skill_folder_hash: "abc".into(),
            imported_hash: "hash".into(),
            imported_at: "2026-09-21T00:00:00Z".into(),
            revision: "rev".into(),
            variants: BTreeMap::new(),
            variants_pending: true,
            harness_agents: BTreeMap::new(),
            release: None,
        }
    }

    #[test]
    fn clone_sources_are_never_parsed_as_options() {
        let mut record = record();
        record.source_url = "--upload-pack=unexpected-command".into();
        for reference in [None, Some("stable".into())] {
            record.r#ref = reference;
            let args = clone_args(&record, "destination");
            let source = args
                .iter()
                .position(|arg| arg == &record.source_url)
                .unwrap();
            assert_eq!(args[source - 1], "--", "{args:?}");
            assert_eq!(args.last().unwrap(), "destination");
        }
    }

    #[test]
    fn a_missing_entry_or_url_is_pending_not_fatal() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        let outcome = import(
            &paths,
            "post",
            &record(),
            &paths.library.join("post"),
            false,
        )
        .unwrap();
        assert!(outcome.pending);
        assert!(outcome.reason.unwrap().contains("missing"));
        assert_eq!(outcome.revision, "rev", "the previous revision survives");

        fs::create_dir_all(paths.library.join("post")).unwrap();
        let baseline = paths.library.join("post");
        let outcome = import(&paths, "post", &record(), &baseline, false).unwrap();
        assert!(outcome.pending);
        assert!(outcome.reason.unwrap().contains("sourceUrl"));
        assert_eq!(outcome.variants, record().variants);
    }

    #[test]
    fn a_broken_clone_url_is_pending() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        fs::create_dir_all(paths.library.join("post")).unwrap();
        fs::write(
            paths.library.join("post/SKILL.md"),
            "---\nname: post\n---\n",
        )
        .unwrap();
        let mut record = record();
        record.source_url = format!("file://{}/nope.git", dir.path().display());
        let baseline = paths.library.join("post");
        let outcome = import(&paths, "post", &record, &baseline, false).unwrap();
        assert!(outcome.pending);
        assert!(!outcome.reason.unwrap().is_empty());
        assert!(outcome.refusals.is_empty());
    }

    #[test]
    fn a_stored_variant_records_the_hash_of_what_was_stored() {
        // Upstream ships a `tests/` tree the pool never copies; the record must
        // carry the hash of the trimmed copy, or `doctor` reports drift on a
        // variant nobody edited.
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        let baseline = paths.library.join("post");
        fs::create_dir_all(&baseline).unwrap();
        fs::write(
            baseline.join("SKILL.md"),
            "---\nname: post\n---\nuniversal\n",
        )
        .unwrap();

        let upstream = dir.path().join("upstream");
        let flavour = upstream.join(".claude/skills/post");
        fs::create_dir_all(flavour.join("scripts/tests")).unwrap();
        fs::write(
            flavour.join("SKILL.md"),
            "---\nname: post\n---\nclaude flavour\n",
        )
        .unwrap();
        fs::write(flavour.join("scripts/core.py"), "print()\n").unwrap();
        fs::write(flavour.join("scripts/tests/bench.md"), "noise\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "-q",
                "-m",
                "upstream",
            ],
        ] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&upstream)
                .args(&args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        }

        let mut record = record();
        record.source_url = format!("file://{}", upstream.display());
        record.skill_path = ".claude/skills/post/SKILL.md".into();
        let outcome = import(&paths, "post", &record, &baseline, false).unwrap();
        assert!(!outcome.pending, "{:?}", outcome.reason);
        assert!(outcome.refusals.is_empty());

        let stored = stored_variant(&paths, "post", "claude");
        assert!(stored.join("scripts/core.py").is_file());
        assert!(
            !stored.join("scripts/tests").exists(),
            "tests/ is never stored"
        );
        assert_eq!(
            outcome.variants.get("claude").map(String::as_str),
            Some(tree_hash(&stored).unwrap().as_str()),
            "the record is the stored tree's hash"
        );
    }

    #[test]
    fn the_upstream_dir_name_comes_from_skill_path() {
        let mut record = record();
        assert_eq!(upstream_dir_name(&record, "post"), "post");
        record.skill_path = "skills/impeccable/SKILL.md".into();
        assert_eq!(upstream_dir_name(&record, "impeccable"), "impeccable");
        record.skill_path = String::new();
        assert_eq!(upstream_dir_name(&record, "post"), "post");
    }

    #[test]
    fn copy_tree_skips_development_trees_and_replaces() {
        let dir = TempDir::new().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::create_dir_all(from.join("tests")).unwrap();
        fs::create_dir_all(from.join("nested/node_modules")).unwrap();
        fs::write(from.join("SKILL.md"), "hello").unwrap();
        fs::write(from.join("tests/bench.md"), "noise").unwrap();
        fs::write(from.join("nested/node_modules/x.md"), "noise").unwrap();
        fs::write(from.join("nested/keep.md"), "kept").unwrap();

        copy_tree(&from, &to, &SKIPPED_TREES).unwrap();
        assert_eq!(fs::read_to_string(to.join("SKILL.md")).unwrap(), "hello");
        assert!(to.join("nested/keep.md").is_file());
        assert!(!to.join("tests").exists());
        assert!(!to.join("nested/node_modules").exists());

        fs::write(from.join("SKILL.md"), "changed").unwrap();
        copy_tree(&from, &to, &SKIPPED_TREES).unwrap();
        assert_eq!(fs::read_to_string(to.join("SKILL.md")).unwrap(), "changed");
        assert!(
            to.join("nested/keep.md").is_file(),
            "a copy is not additive"
        );
    }
}
