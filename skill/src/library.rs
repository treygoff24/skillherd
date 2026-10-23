//! The library catalog: describe what the pool holds (lane T2).
//!
//! `catalog` answers "what is in the pool". The harness views below
//! ([`harness_roots`], [`source_for`], [`visible_in`]) turn a `Policy` into the
//! per-harness answers `list` and `info` print, and [`missing_skill`] is the one
//! "no such skill" error, so `info` and `on` teach the same way. The
//! frontmatter reader is a YAML *subset* on purpose: the crate ships no YAML
//! dependency, and skill frontmatter is `key: value` with the occasional block
//! scalar.

use std::fs;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hash::committed_tree_hash;
use crate::model::{NpxMeta, Platform, Policy, ReadRoot, SetRow, Tier};
use crate::names::validate_name;
use crate::output;
use crate::paths::Paths;
use crate::policy::{AGENTS, OMP, effective_set, visible_set};

/// The skill definition file every entry must hold.
pub const SKILL_FILE: &str = "SKILL.md";

/// Where per-harness overrides live inside an entry.
pub const VARIANTS_DIR: &str = ".variants";

/// Where the import records live, beside the entries.
pub const NPX_DIR: &str = ".npx";

/// How many close names an unknown-skill error offers.
const CLOSE_NAMES: usize = 3;

/// One library entry as the catalog reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillEntry {
    pub name: String,
    /// The entry in the pool: a real directory, or a link to one.
    pub path: PathBuf,
    /// The frontmatter `description`, else the first body paragraph.
    pub description: String,
    /// True when the entry is a link rather than a directory in the pool.
    pub is_symlink: bool,
    /// The link text, for a linked entry (`post` → a development repo).
    pub link_target: Option<PathBuf>,
    /// Harness variant keys present under `.variants/`.
    pub variants: Vec<String>,
    /// The import record, when the entry came from npx.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub npx: Option<NpxMeta>,
    /// True when the entry's tree hash differs from its import record.
    pub local_edit: bool,
}

/// Scan `library/*/SKILL.md`, skipping dot-entries.
///
/// A linked entry is followed to its target, so the five edit-in-project links
/// read as the skills they are.
pub fn catalog(paths: &Paths) -> Result<Vec<SkillEntry>> {
    let read = match fs::read_dir(&paths.library) {
        Ok(read) => read,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(Error::io(
                &format!("reading library {}", paths.library.display()),
                &err,
            ));
        }
    };

    let mut names: Vec<String> = Vec::new();
    for dirent in read {
        let dirent = dirent.map_err(|err| Error::io("reading a library entry", &err))?;
        let name = dirent.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if !dirent.path().join(SKILL_FILE).is_file() {
            continue;
        }
        if validate_name(&name).is_err() {
            output::warn(
                "library_entry_invalid_name",
                format!(
                    "{} is not a usable skill name; skipping it",
                    dirent.path().display()
                ),
            );
            continue;
        }
        names.push(name);
    }
    names.sort();

    let mut entries = Vec::with_capacity(names.len());
    for name in names {
        entries.push(read_entry(paths, &name)?);
    }
    Ok(entries)
}

fn read_entry(paths: &Paths, name: &str) -> Result<SkillEntry> {
    let path = paths.library.join(name);
    let meta = fs::symlink_metadata(&path)
        .map_err(|err| Error::io(&format!("reading entry {}", path.display()), &err))?;
    let is_symlink = meta.file_type().is_symlink();
    let link_target = if is_symlink {
        fs::read_link(&path).ok()
    } else {
        None
    };
    let source = fs::read_to_string(path.join(SKILL_FILE)).unwrap_or_default();
    let npx = read_npx_record(paths, name);
    let local_edit = match &npx {
        Some(record) => entry_hash(paths, name, &path)
            .map(|hash| hash != record.imported_hash)
            .unwrap_or(false),
        None => false,
    };

    Ok(SkillEntry {
        name: name.to_string(),
        path: path.clone(),
        description: description_of(&source),
        is_symlink,
        link_target,
        variants: read_variants(&path),
        npx,
        local_edit,
    })
}

/// The hash refuses a symlink at its root (it stats with `symlink_metadata`),
/// so a linked entry is hashed through the directory it points at, as the
/// pool commits it at `name`.
fn entry_hash(paths: &Paths, name: &str, entry: &Path) -> Result<String> {
    let resolved = fs::canonicalize(entry)
        .map_err(|err| Error::io(&format!("resolving {}", entry.display()), &err))?;
    committed_tree_hash(&paths.library, name, &resolved, &[])
}

fn read_variants(entry: &Path) -> Vec<String> {
    let Ok(read) = fs::read_dir(entry.join(VARIANTS_DIR)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .filter_map(|dirent| dirent.ok())
        .filter(|dirent| dirent.path().is_dir())
        .map(|dirent| dirent.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// A malformed or unreadable record is a warning, never a failed listing: the
/// entry is still in the pool and `doctor` reports the record.
fn read_npx_record(paths: &Paths, name: &str) -> Option<NpxMeta> {
    let path = paths.library.join(NPX_DIR).join(format!("{name}.json"));
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            output::warn("npx_record_invalid", format!("{}: {err}", path.display()));
            return None;
        }
    };
    match serde_json::from_str::<NpxMeta>(&text) {
        Ok(record) => Some(record),
        Err(err) => {
            output::warn("npx_record_invalid", format!("{}: {err}", path.display()));
            None
        }
    }
}

/// One read root of a harness, with the names it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessRoot {
    /// `surface` or `agents_dir`, as the harness row declares it.
    pub via: &'static str,
    /// The directory this root reads.
    pub path: PathBuf,
    /// The variant key the root serves, when it has one.
    pub variant_key: Option<String>,
    /// The names this root contributes, sorted.
    pub names: Vec<String>,
}

/// Every root a harness reads, in row order. `omp` reads the shared dir only.
pub fn harness_roots(policy: &Policy, paths: &Paths, harness: &str) -> Result<Vec<HarnessRoot>> {
    if harness == OMP {
        let names = effective_set(policy, AGENTS, paths.platform)?;
        return Ok(vec![HarnessRoot {
            via: "agents_dir",
            path: paths.agents_dir.clone(),
            variant_key: None,
            names,
        }]);
    }

    let row = policy
        .harness(harness)
        .ok_or_else(|| Error::policy_unknown_harness(harness, None))?;
    let mut roots = Vec::new();
    for read in &row.reads {
        roots.push(match read {
            ReadRoot::Surface => HarnessRoot {
                via: "surface",
                path: row.surface.clone(),
                variant_key: row.variant_key.clone(),
                names: effective_set(policy, &row.name, paths.platform)?,
            },
            ReadRoot::AgentsDir => HarnessRoot {
                via: "agents_dir",
                path: paths.agents_dir.clone(),
                variant_key: None,
                names: effective_set(policy, AGENTS, paths.platform)?,
            },
        });
    }
    Ok(roots)
}

/// The library path a root serves for one skill: its variant tree when the root
/// has a variant key and that variant exists inside the skill, else the
/// universal copy (the renderer's rule, `surfaces::source_for`).
pub fn source_for(root: &HarnessRoot, paths: &Paths, name: &str) -> PathBuf {
    let base = paths.library.join(name);
    if let Some(key) = &root.variant_key
        && let crate::surfaces::VariantTree::Inside(variant) =
            crate::surfaces::variant_tree(&base, key)
    {
        return variant;
    }
    base
}

/// Every harness the table defines, in table order, then `omp`.
pub fn harness_names(policy: &Policy) -> Vec<String> {
    let mut names: Vec<String> = policy
        .harnesses
        .iter()
        .map(|row| row.name.clone())
        .collect();
    if !names.iter().any(|name| name == OMP) {
        names.push(OMP.to_string());
    }
    names
}

/// The harnesses whose visible set contains `name`, in table order.
pub fn visible_in(policy: &Policy, name: &str, platform: Platform) -> Vec<String> {
    harness_names(policy)
        .into_iter()
        .filter(|harness| {
            visible_set(policy, harness, platform)
                .map(|set| set.iter().any(|skill| skill == name))
                .unwrap_or(false)
        })
        .collect()
}

/// Compute visibility once per harness for a whole catalog, preserving table
/// order. Recomputing `visible_in` for each entry makes `list --all` quadratic.
pub fn visibility_index(
    policy: &Policy,
    platform: Platform,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut index: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for harness in harness_names(policy) {
        if let Ok(names) = visible_set(policy, &harness, platform) {
            for name in names {
                index.entry(name).or_default().push(harness.clone());
            }
        }
    }
    index
}

/// The tier tokens a row carries, in the order `save` writes them.
pub fn tiers_of(row: &SetRow) -> Vec<&'static str> {
    let mut tiers = Vec::new();
    if row.main {
        tiers.push("main");
    }
    if row.lane {
        tiers.push("lane");
    }
    tiers
}

/// The tier a harness row serves, as the JSON spells it.
pub fn tier_of(policy: &Policy, harness: &str) -> Option<Tier> {
    let name = if harness == OMP { AGENTS } else { harness };
    policy.harness(name).map(|row| row.tier)
}

/// The `sets` file the policy mutators read and write.
pub fn sets_path(paths: &Paths) -> PathBuf {
    paths.library.join(".globals/sets")
}

/// The one "no such skill" error, with the closest names the pool does hold.
pub fn missing_skill(paths: &Paths, policy: &Policy, catalog: &[SkillEntry], name: &str) -> Error {
    let mut candidates: Vec<String> = catalog
        .iter()
        .map(|entry| entry.name.clone())
        .chain(policy.rows.iter().map(|row| row.name.clone()))
        .collect();
    candidates.sort();
    candidates.dedup();
    let closest = closest_names(candidates.iter().map(String::as_str), name, CLOSE_NAMES);

    let reason = match policy.row(name) {
        Some(row) => format!(
            "has no library entry (the policy names it on sets line {})",
            row.line
        ),
        None => "has no library entry".to_string(),
    };
    let suggestion = if closest.is_empty() {
        "the pool holds no skills yet; `skillherd list --all` prints what it does hold".to_string()
    } else {
        format!(
            "did you mean {}? `skillherd list --all` prints the whole pool",
            closest
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    match Error::input_not_found_reason(&paths.library.join(name), &reason) {
        Error::InputNotFound {
            message, details, ..
        } => Error::InputNotFound {
            message,
            details,
            suggested_fix: Some(suggestion),
        },
        other => other,
    }
}

/// The closest names to `target`, nearest first, ties broken by name.
pub fn closest_names<'a>(
    names: impl Iterator<Item = &'a str>,
    target: &str,
    limit: usize,
) -> Vec<String> {
    let mut scored: Vec<(usize, &str)> = names
        .map(|name| (edit_distance(name, target), name))
        .collect();
    scored.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)));
    scored.dedup_by(|left, right| left.1 == right.1);
    scored
        .into_iter()
        .take(limit)
        .map(|(_, name)| name.to_string())
        .collect()
}

/// Levenshtein distance over characters, the usual two-row DP.
pub fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0usize; right.len() + 1];
    for (index, a) in left.iter().enumerate() {
        current[0] = index + 1;
        for (other, b) in right.iter().enumerate() {
            let substitute = previous[other] + usize::from(a != b);
            current[other + 1] = substitute
                .min(previous[other + 1] + 1)
                .min(current[other] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

/// A skill's description: the frontmatter `description` when it has one, else
/// the first paragraph of the body.
pub fn description_of(source: &str) -> String {
    let (frontmatter, body) = split_frontmatter(source);
    if let Some(block) = frontmatter
        && let Some(description) = frontmatter_description(block)
        && !description.is_empty()
    {
        return description;
    }
    first_paragraph(body)
}

/// Split a leading `---` fence off the file. A fence that never closes leaves
/// no body: the file said where its prose would start and never got there.
fn split_frontmatter(source: &str) -> (Option<&str>, &str) {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let Some(rest) = source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"))
    else {
        return (None, source);
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return (Some(&rest[..offset]), &rest[offset + line.len()..]);
        }
        offset += line.len();
    }
    (Some(rest), "")
}

/// The `description` of a YAML subset: scalars, `>-`/`|` block scalars, nested
/// maps tolerated and ignored, unknown keys ignored.
fn frontmatter_description(block: &str) -> Option<String> {
    let lines: Vec<&str> = block.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        index += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if is_indented(line) {
            // A continuation of a block we did not claim: ignore it.
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        if key.trim() != "description" {
            // Tolerate a nested map or a block scalar under any other key.
            while index < lines.len() && is_continuation(lines[index]) {
                index += 1;
            }
            continue;
        }
        let value = value.trim();
        if is_block_scalar(value) {
            let folded = value.starts_with('>');
            let mut parts: Vec<&str> = Vec::new();
            while index < lines.len() && is_continuation(lines[index]) {
                parts.push(lines[index].trim());
                index += 1;
            }
            while parts.last().is_some_and(|part| part.is_empty()) {
                parts.pop();
            }
            let text = if folded {
                parts
                    .iter()
                    .filter(|part| !part.is_empty())
                    .copied()
                    .collect::<Vec<&str>>()
                    .join(" ")
            } else {
                parts.join("\n")
            };
            let text = text.trim().to_string();
            if !text.is_empty() {
                return Some(text);
            }
        } else {
            let text = unquote(value);
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn is_indented(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

/// A line that belongs to the value above it: indented, or blank.
fn is_continuation(line: &str) -> bool {
    line.trim().is_empty() || is_indented(line)
}

fn is_block_scalar(value: &str) -> bool {
    matches!(value, ">" | ">-" | ">+" | "|" | "|-" | "|+")
}

/// Strip one layer of quotes, unescaping the two forms' escapes.
fn unquote(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let mut out = String::new();
        let mut chars = value[1..value.len() - 1].chars();
        while let Some(character) = chars.next() {
            if character != '\\' {
                out.push(character);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => {}
            }
        }
        return out;
    }
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return value[1..value.len() - 1].replace("''", "'");
    }
    value.to_string()
}

/// The first paragraph of a body, headings skipped.
fn first_paragraph(body: &str) -> String {
    let mut paragraph: Vec<&str> = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        paragraph.push(trimmed);
    }
    paragraph.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SetRow;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn write_skill(dir: &Path, name: &str, source: &str) {
        let entry = dir.join(name);
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join(SKILL_FILE), source).unwrap();
    }

    #[test]
    fn frontmatter_scalars() {
        assert_eq!(
            description_of("---\nname: post\ndescription: Publish letters\n---\n\nbody\n"),
            "Publish letters"
        );
        assert_eq!(
            description_of("---\ndescription: \"quoted: value\"\n---\nbody\n"),
            "quoted: value"
        );
        assert_eq!(
            description_of("---\ndescription: 'it''s fine'\n---\nbody\n"),
            "it's fine"
        );
        assert_eq!(
            description_of("---\ndescription: \"tab\\tend\"\n---\nbody\n"),
            "tab\tend"
        );
    }

    #[test]
    fn frontmatter_block_scalars_and_nested_keys() {
        let folded = "---\nname: post\nmetadata:\n  owner: user\n  tags:\n    - a\ndescription: >-\n  Publish\n  letters\n  well\nlicense: MIT\n---\n\nbody\n";
        assert_eq!(description_of(folded), "Publish letters well");

        let literal = "---\ndescription: |\n  line one\n  line two\n---\nbody\n";
        assert_eq!(description_of(literal), "line one\nline two");
    }

    #[test]
    fn missing_frontmatter_uses_the_first_body_paragraph() {
        assert_eq!(
            description_of("# Title\n\nFirst line\nsecond line\n\nOther\n"),
            "First line second line"
        );
        assert_eq!(
            description_of(
                "---\nname: post\nmetadata:\n  owner: user\n---\n\n# Title\n\nBody prose\nmore\n"
            ),
            "Body prose more",
            "a frontmatter with no description falls back to the body"
        );
        assert_eq!(description_of("---\nname: post\n"), "");
        assert_eq!(description_of(""), "");
    }

    #[test]
    fn the_catalog_skips_dot_entries_and_entries_without_a_skill_file() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        let library = &paths.library;
        write_skill(library, "post", "---\ndescription: Publish letters\n---\n");
        fs::create_dir_all(library.join(".git")).unwrap();
        fs::create_dir_all(library.join(".npx")).unwrap();
        fs::create_dir_all(library.join("not-a-skill")).unwrap();

        let entries = catalog(&paths).unwrap();
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["post"]);
        assert_eq!(entries[0].description, "Publish letters");
        assert!(!entries[0].is_symlink);
        assert_eq!(entries[0].link_target, None);
        assert!(entries[0].variants.is_empty());
        assert!(!entries[0].local_edit);

        assert!(
            catalog(&paths).is_ok(),
            "a missing library is an empty catalog"
        );
        let empty = Paths::fixture(dir.path().join("nowhere"));
        assert!(catalog(&empty).unwrap().is_empty());
    }

    #[test]
    fn a_linked_entry_is_followed_and_a_local_edit_is_flagged() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        let outside = dir.path().join("Code/deslop");
        write_skill(
            &dir.path().join("Code"),
            "deslop",
            "---\ndescription: Lint prose\n---\n",
        );
        fs::create_dir_all(&paths.library).unwrap();
        std::os::unix::fs::symlink(&outside, paths.library.join("deslop")).unwrap();

        write_skill(
            &paths.library,
            "post",
            "---\ndescription: Publish letters\n---\n",
        );
        let variant = paths.library.join("post").join(VARIANTS_DIR).join("claude");
        fs::create_dir_all(&variant).unwrap();
        fs::write(variant.join(SKILL_FILE), "claude copy").unwrap();
        fs::create_dir_all(paths.library.join(NPX_DIR)).unwrap();
        fs::write(
            paths.library.join(NPX_DIR).join("post.json"),
            format!(
                "{{\"source\":\"owner/repo\",\"sourceType\":\"github\",\
                  \"sourceUrl\":\"https://github.com/owner/repo\",\
                  \"skillPath\":\".agents/skills/post/SKILL.md\",\
                  \"skillFolderHash\":\"aaa\",\"importedHash\":\"{}\",\
                  \"importedAt\":\"2026-09-21T00:00:00Z\",\"revision\":\"{}\",\
                  \"variants\":{{}},\"variantsPending\":false}}",
                "0".repeat(64),
                "1".repeat(40)
            ),
        )
        .unwrap();

        let entries = catalog(&paths).unwrap();
        let by_name: BTreeMap<&str, &SkillEntry> = entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry))
            .collect();

        let deslop = by_name["deslop"];
        assert!(deslop.is_symlink);
        assert_eq!(deslop.link_target.as_deref(), Some(outside.as_path()));
        assert_eq!(deslop.description, "Lint prose");
        assert!(
            !deslop.local_edit,
            "no import record: nothing to differ from"
        );

        let post = by_name["post"];
        assert_eq!(post.variants, ["claude"]);
        assert!(post.local_edit, "the record's hash is not this tree's");
        assert_eq!(
            post.npx.as_ref().unwrap().imported_hash,
            "0".repeat(64),
            "the record round-trips"
        );
    }

    #[test]
    fn a_malformed_record_is_a_warning_not_a_failure() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        write_skill(&paths.library, "post", "body\n");
        fs::create_dir_all(paths.library.join(NPX_DIR)).unwrap();
        fs::write(paths.library.join(NPX_DIR).join("post.json"), "{ not json").unwrap();

        output::clear_warnings();
        let entries = catalog(&paths).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].npx.is_none());
        assert!(
            output::warnings()
                .iter()
                .any(|warning| warning.code == "npx_record_invalid")
        );
        output::clear_warnings();
    }

    #[test]
    fn edit_distance_and_close_names() {
        assert_eq!(edit_distance("post", "post"), 0);
        assert_eq!(edit_distance("post", "posts"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);

        let names = ["writing-plans", "writing", "deslop", "post"];
        let closest = closest_names(names.iter().copied(), "writing-plan", 2);
        assert_eq!(closest, ["writing-plans", "writing"]);
        assert_eq!(closest_names(names.iter().copied(), "post", 3).len(), 3);
    }

    #[test]
    fn tiers_and_missing_skill_errors() {
        let row = SetRow {
            name: "post".to_string(),
            main: true,
            lane: true,
            not_platform: Vec::new(),
            only: Vec::new(),
            not: Vec::new(),
            line: 4,
        };
        assert_eq!(tiers_of(&row), ["main", "lane"]);

        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        write_skill(&paths.library, "writing-plans", "body\n");
        let entries = catalog(&paths).unwrap();
        let policy = crate::policy::default_policy(&paths);
        let err = missing_skill(&paths, &policy, &entries, "writing-plan");
        assert_eq!(err.code(), "input_not_found");
        assert_eq!(err.exit_code(), 66);
        assert!(
            err.suggested_fix().unwrap().contains("writing-plans"),
            "{err}"
        );
    }
}
