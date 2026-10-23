//! The policy: `library/.globals/sets` plus the harness table.
//!
//! The two set functions here are pure and are the single source of truth for
//! render (T3), catalog (T2) and doctor (T5). `load`/`save` own the file
//! grammar: `sets` is a row per always-on skill, `harnesses` is the surface
//! table, and a save rewrites `sets` without disturbing a comment, a blank
//! line or the position of a row it did not change.

use crate::fsx as fs;
use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::json;

use crate::error::{Error, Result};
use crate::model::{HarnessRow, Platform, Policy, ReadRoot, SetRow, Tier};
use crate::names::validate_name;
use crate::output;
use crate::paths::Paths;

/// The harness names the table defines; `omp` is an alias for `agents`.
pub const KNOWN_HARNESSES: [&str; 6] = [
    HARNESS_REGISTRY[0].name,
    HARNESS_REGISTRY[1].name,
    HARNESS_REGISTRY[2].name,
    HARNESS_REGISTRY[3].name,
    HARNESS_REGISTRY[4].name,
    HARNESS_REGISTRY[5].name,
];

/// The `agents` row name, which is also the shared root's harness name.
pub const AGENTS: &str = "agents";

/// `omp` reads `agents_dir` only, so it is the visibility of `agents`.
pub const OMP: &str = "omp";

/// One built-in harness definition, shared by policy and local path validation.
pub struct HarnessSpec {
    pub name: &'static str,
    pub tier: Tier,
    pub surface: fn(&Paths) -> PathBuf,
    pub variant_key: Option<&'static str>,
    pub reads: &'static [ReadRoot],
    pub legacy_root: &'static str,
}

/// Destination identities live here; shared policy may select but not extend them.
pub const HARNESS_REGISTRY: [HarnessSpec; 6] = [
    HarnessSpec {
        name: "claude",
        tier: Tier::Main,
        surface: |p| p.claude_shared.join("skills"),
        variant_key: Some("claude"),
        reads: &[ReadRoot::Surface],
        legacy_root: "{claude_shared}/skills",
    },
    HarnessSpec {
        name: "codex",
        tier: Tier::Main,
        surface: |p| p.codex_home.join("skills"),
        variant_key: Some("codex"),
        reads: &[ReadRoot::Surface, ReadRoot::AgentsDir],
        legacy_root: "{codex_home}/skills",
    },
    HarnessSpec {
        name: AGENTS,
        tier: Tier::Lane,
        surface: |p| p.agents_dir.clone(),
        variant_key: None,
        reads: &[ReadRoot::Surface],
        legacy_root: "{agents_dir}",
    },
    HarnessSpec {
        name: "grok",
        tier: Tier::None,
        surface: |p| p.grok_home.join("skills"),
        variant_key: None,
        reads: &[ReadRoot::Surface, ReadRoot::AgentsDir],
        legacy_root: "{grok_home}/skills",
    },
    HarnessSpec {
        name: "pi",
        tier: Tier::Lane,
        surface: |p| p.pi_skills.clone(),
        variant_key: Some("pi"),
        reads: &[ReadRoot::Surface],
        legacy_root: "{pi_skills}",
    },
    HarnessSpec {
        name: "cursor",
        tier: Tier::None,
        surface: |p| p.cursor_skills.clone(),
        variant_key: Some("cursor"),
        reads: &[ReadRoot::Surface, ReadRoot::AgentsDir],
        legacy_root: "{cursor_skills}",
    },
];

/// The local harness table, with no set rows.
pub fn default_policy(paths: &Paths) -> Policy {
    Policy {
        rows: Vec::new(),
        harnesses: HARNESS_REGISTRY
            .iter()
            .map(|spec| HarnessRow {
                name: spec.name.to_owned(),
                tier: spec.tier,
                surface: (spec.surface)(paths),
                variant_key: spec.variant_key.map(str::to_owned),
                reads: spec.reads.to_vec(),
            })
            .collect(),
        raw_lines: Vec::new(),
    }
}

/// The names a harness's own surface serves.
///
/// The desired-set rule: tier match — a `main` surface serves the rows carrying
/// `main`, a `lane` surface the rows carrying `lane` (a lane row also carries
/// `main`, by the parse rule, so the lane set is a subset of the main set, never
/// a second list the main surface also serves); minus `!<platform>`; minus
/// `not:<harness>`; a row carrying `only:` is visible only on the surface of a
/// harness it names, and the shared `agents` root drops such rows entirely
/// because it cannot exclude one of its readers. `tier none` serves nothing.
/// `omp` is accepted as a harness name and answers `visible_set("agents")`.
pub fn effective_set(policy: &Policy, harness: &str, platform: Platform) -> Result<Vec<String>> {
    if harness == OMP {
        return visible_set(policy, AGENTS, platform);
    }
    let row = policy
        .harness(harness)
        .ok_or_else(|| Error::policy_unknown_harness(harness, None))?;
    if row.tier == Tier::None {
        return Ok(Vec::new());
    }

    let mut names: BTreeSet<&str> = BTreeSet::new();
    for set_row in &policy.rows {
        let tier_match = match row.tier {
            Tier::Main => set_row.main,
            Tier::Lane => set_row.lane,
            Tier::None => false,
        };
        if !tier_match {
            continue;
        }
        if set_row.not_platform.contains(&platform) {
            continue;
        }
        if set_row.not.iter().any(|name| name == harness) {
            continue;
        }
        if !set_row.only.is_empty()
            && (harness == AGENTS || !set_row.only.iter().any(|name| name == harness))
        {
            continue;
        }
        names.insert(set_row.name.as_str());
    }
    Ok(names.into_iter().map(str::to_owned).collect())
}

/// Everything a harness actually reads: the union over its read roots, where
/// `agents_dir` contributes `effective_set("agents")`.
pub fn visible_set(policy: &Policy, harness: &str, platform: Platform) -> Result<Vec<String>> {
    let name = if harness == OMP { AGENTS } else { harness };
    let row = policy
        .harness(name)
        .ok_or_else(|| Error::policy_unknown_harness(harness, None))?
        .clone();

    let mut names: BTreeSet<String> = BTreeSet::new();
    for root in &row.reads {
        let contribution = match root {
            ReadRoot::Surface => effective_set(policy, name, platform)?,
            ReadRoot::AgentsDir => effective_set(policy, AGENTS, platform)?,
        };
        names.extend(contribution);
    }
    Ok(names.into_iter().collect())
}

/// The `sets` file inside the library.
fn sets_path(paths: &Paths) -> PathBuf {
    paths.library.join(".globals/sets")
}

/// The harness table inside the library.
fn harnesses_path(paths: &Paths) -> PathBuf {
    paths.library.join(".globals/harnesses")
}

/// Parse `library/.globals/sets` and `library/.globals/harnesses`.
///
/// Missing files are not an error: no `sets` means no always-on rows, no
/// `harnesses` means the built-in six-row table. Both files are kept verbatim
/// in `raw_lines`/`lines` so `save` can round-trip them.
pub fn load(paths: &Paths) -> Result<Policy> {
    load_inner(paths, false)
}

/// Read-only commands must be able to diagnose an old linked-source layout.
/// All policy and destination checks still apply; source overlaps are warnings.
pub fn load_for_inspection(paths: &Paths) -> Result<Policy> {
    load_inner(paths, true)
}

fn load_inner(paths: &Paths, inspection: bool) -> Result<Policy> {
    let defaults = default_policy(paths);

    let harnesses = match fs::read_to_string(&harnesses_path(paths)) {
        Ok(text) => {
            let rows = parse_harnesses(&text, paths)?;
            if rows.is_empty() {
                output::warn(
                    "policy_harnesses_empty",
                    format!(
                        "{} holds no harness rows; using the built-in six-row table",
                        harnesses_path(paths).display()
                    ),
                );
                defaults.harnesses.clone()
            } else {
                rows
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => defaults.harnesses.clone(),
        Err(err) => {
            return Err(crate::paths::metadata_error(&harnesses_path(paths), &err));
        }
    };

    let raw_lines = match fs::read_to_string(&sets_path(paths)) {
        Ok(text) => text.split('\n').map(str::to_owned).collect::<Vec<String>>(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            return Err(crate::paths::metadata_error(&sets_path(paths), &err));
        }
    };

    // Keep this check here as well as main's default-root preflight: callers
    // can load a policy directly, and a loaded table may reuse a local root.
    paths.validate_surfaces(
        harnesses
            .iter()
            .map(|row| (row.name.as_str(), row.surface.as_path())),
    )?;
    if inspection {
        for message in paths.source_overlaps(&harnesses)? {
            output::warn("source_surface_overlap", message);
        }
    } else {
        paths.validate_sources(&harnesses)?;
    }
    let rows = parse_sets(&raw_lines, &harnesses)?;
    Ok(Policy {
        rows,
        harnesses,
        raw_lines,
    })
}

/// Rewrite `library/.globals/sets`, preserving comments and row order.
///
/// A row whose parsed form equals what the file already says is copied
/// byte-for-byte, so `load` → `save` with no change rewrites nothing at all.
/// An empty policy with no file yet writes nothing either: an absent `sets`
/// file and an empty one mean the same thing.
pub fn save(paths: &Paths, policy: &Policy) -> Result<()> {
    for row in &policy.rows {
        check_row_against(&policy.harnesses, row)?;
    }

    let path = sets_path(paths);
    let content = render_sets(policy);
    let existing = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(crate::paths::metadata_error(&path, &err)),
    };
    if existing.as_deref() == Some(content.as_str()) {
        return Ok(());
    }
    if existing.is_none() && content.is_empty() {
        return Ok(());
    }
    // A file `save` creates gets a trailing newline; a file that already exists
    // keeps its bytes, so `load` → `save` stays byte-identical.
    let content = if existing.is_none() && !content.ends_with('\n') {
        format!("{content}\n")
    } else {
        content
    };
    fs::atomic_write(&path, content.as_bytes())
        .map_err(|err| crate::paths::metadata_error(&path, &err))
}

/// Refuse a row whose tokens ask for something no set of surfaces can express.
///
/// Shared by `load` (a hand-edited file) and `on` (a CLI combination): a caller
/// must be able to refuse an unrepresentable row *before* writing it.
pub fn check_row(policy: &Policy, row: &SetRow) -> Result<()> {
    check_row_against(&policy.harnesses, row)
}

/// The one canonical spelling of a row, as `save` writes it.
pub fn row_line(row: &SetRow) -> String {
    let mut parts = vec![row.name.clone()];
    if row.main {
        parts.push("main".to_string());
    }
    if row.lane {
        parts.push("lane".to_string());
    }
    for platform in &row.not_platform {
        parts.push(format!("!{platform}"));
    }
    for harness in &row.only {
        parts.push(format!("only:{harness}"));
    }
    for harness in &row.not {
        parts.push(format!("not:{harness}"));
    }
    parts.join(" ")
}

/// One `sets` line per row, keeping every other line of the file as it stands.
fn render_sets(policy: &Policy) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut used = vec![false; policy.rows.len()];

    for (index, raw) in policy.raw_lines.iter().enumerate() {
        let Some(parsed) = parse_raw_row(raw, index + 1) else {
            lines.push(raw.clone());
            continue;
        };
        let matched = policy
            .rows
            .iter()
            .enumerate()
            .find(|(position, row)| !used[*position] && row.name == parsed.name);
        if let Some((position, row)) = matched {
            used[position] = true;
            if *row == parsed {
                lines.push(raw.clone());
            } else {
                lines.push(row_line(row));
            }
        }
    }
    let appended: Vec<&SetRow> = policy
        .rows
        .iter()
        .enumerate()
        .filter(|(position, _)| !used[*position])
        .map(|(_, row)| row)
        .collect();
    if !appended.is_empty() {
        // New rows go after a blank line, and a file that ended with a newline
        // still does: the appended rows borrow the file's own line ending.
        let trailing = lines.last().is_some_and(|line| line.is_empty());
        if trailing {
            lines.pop();
        }
        if lines.last().is_some_and(|line| !line.is_empty()) {
            lines.push(String::new());
        }
        for row in appended {
            lines.push(row_line(row));
        }
        if trailing {
            lines.push(String::new());
        }
    }
    lines.join("\n")
}

/// Read one `sets` line back into a row, leniently.
///
/// `save` needs to recognize the line whether or not its tokens are ones
/// `load` would accept, so it can replace it in place or drop it. A line
/// carrying anything unrecognized comes back with `line: 0`, which no loaded
/// row can equal — that forces the canonical rewrite instead of preserving
/// garbage.
fn parse_raw_row(raw: &str, line: usize) -> Option<SetRow> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let mut fields = trimmed.split_whitespace();
    let name = fields.next()?;
    if validate_name(name).is_err() {
        return None;
    }
    let mut row = SetRow {
        name: name.to_string(),
        main: false,
        lane: false,
        not_platform: Vec::new(),
        only: Vec::new(),
        not: Vec::new(),
        line,
    };
    let mut recognized = true;
    for token in fields {
        match token {
            "main" => row.main = true,
            "lane" => row.lane = true,
            "!mac" => push_platform(&mut row.not_platform, Platform::Mac),
            "!linux" => push_platform(&mut row.not_platform, Platform::Linux),
            _ => match token.split_once(':') {
                Some(("only", harness)) => row.only.push(canonical_harness(harness)),
                Some(("not", harness)) => row.not.push(canonical_harness(harness)),
                _ => recognized = false,
            },
        }
    }
    if !recognized {
        row.line = 0;
    }
    Some(row)
}

/// Parse every row line of `sets`, validating names, tokens and harnesses.
fn parse_sets(raw_lines: &[String], harnesses: &[HarnessRow]) -> Result<Vec<SetRow>> {
    let mut rows = Vec::new();
    for (index, raw) in raw_lines.iter().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        check_token_lengths(trimmed, line, "sets")?;
        let mut fields = trimmed.split_whitespace();
        let name = fields.next().unwrap_or_default();
        if let Err(err) = validate_name(name) {
            return Err(with_line(err, line, "sets"));
        }
        let mut row = SetRow {
            name: name.to_string(),
            main: false,
            lane: false,
            not_platform: Vec::new(),
            only: Vec::new(),
            not: Vec::new(),
            line,
        };
        for token in fields {
            match token {
                "main" => row.main = true,
                "lane" => row.lane = true,
                "!mac" => push_platform(&mut row.not_platform, Platform::Mac),
                "!linux" => push_platform(&mut row.not_platform, Platform::Linux),
                _ => {
                    if let Some(reference) = token.strip_prefix("only:") {
                        row.only
                            .push(harness_reference(reference, line, harnesses)?);
                    } else if let Some(reference) = token.strip_prefix("not:") {
                        row.not.push(harness_reference(reference, line, harnesses)?);
                    } else if token.starts_with('#') {
                        return Err(policy_bad_token(
                            token,
                            line,
                            format!(
                                "sets line {line}: inline comments are not supported ({token:?}); put the comment on its own line"
                            ),
                        ));
                    } else {
                        return Err(Error::policy_bad_token(token, line));
                    }
                }
            }
        }
        if row.lane && !row.main {
            return Err(Error::policy_lane_not_in_main(name, line));
        }
        if !row.main && !row.lane {
            output::warn(
                "policy_row_inert",
                format!(
                    "sets line {line}: {name} carries no tier token, so no surface serves it; add `main` or `lane`"
                ),
            );
        }
        check_row_against(harnesses, &row)?;
        rows.push(row);
    }
    Ok(rows)
}

/// Parse the harness table; an empty table is reported, not fatal (`load`).
fn parse_harnesses(text: &str, paths: &Paths) -> Result<Vec<HarnessRow>> {
    let mut rows = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        check_token_lengths(trimmed, line, "harnesses")?;
        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(harnesses_error(
                fields.first().copied().unwrap_or_default(),
                line,
                format!(
                    "harnesses line {line} has {} fields; a row is `harness tier surface variant-key reads`, and an inline `#` comment counts as fields",
                    fields.len()
                ),
            ));
        }
        let (name, tier, surface, variant, reads) =
            (fields[0], fields[1], fields[2], fields[3], fields[4]);
        if let Err(err) = validate_name(name) {
            return Err(with_line(err, line, "harnesses"));
        }
        if name == OMP {
            return Err(harnesses_error(
                name,
                line,
                format!(
                    "harnesses line {line}: `omp` is an alias for the `agents` row, not a row of its own"
                ),
            ));
        }
        let tier = match tier {
            "main" => Tier::Main,
            "lane" => Tier::Lane,
            "none" => Tier::None,
            _ => {
                return Err(harnesses_error(
                    tier,
                    line,
                    format!(
                        "harnesses line {line}: unknown tier {tier:?}; tiers are `main`, `lane`, `none`"
                    ),
                ));
            }
        };
        let surface = resolve_surface(paths, surface, line)?;
        if rows.iter().any(|row: &HarnessRow| row.name == name) {
            return Err(harnesses_error(
                name,
                line,
                format!("harnesses line {line}: duplicate harness {name}"),
            ));
        }
        let variant_key = if variant == "-" {
            None
        } else {
            if let Err(err) = validate_name(variant) {
                return Err(with_line(err, line, "harnesses"));
            }
            Some(variant.to_string())
        };
        rows.push(HarnessRow {
            name: name.to_string(),
            tier,
            surface,
            variant_key,
            reads: parse_reads(reads, line)?,
        });
    }
    Ok(rows)
}

/// `surface,agents_dir` (or `-` for a harness that reads nothing).
fn parse_reads(field: &str, line: usize) -> Result<Vec<ReadRoot>> {
    if field == "-" {
        return Ok(Vec::new());
    }
    let mut reads = Vec::new();
    for part in field.split(',') {
        let root = match part {
            "surface" => ReadRoot::Surface,
            "agents_dir" => ReadRoot::AgentsDir,
            _ => {
                return Err(harnesses_error(
                    part,
                    line,
                    format!(
                        "harnesses line {line}: unknown read root {part:?}; roots are `surface` and `agents_dir`, comma-separated (`-` for none)"
                    ),
                ));
            }
        };
        if !reads.contains(&root) {
            reads.push(root);
        }
    }
    Ok(reads)
}

/// Shared policy can select a local root, never construct a destination.
/// Legacy root spellings are accepted only as exact aliases for those roots.
fn resolve_surface(paths: &Paths, field: &str, line: usize) -> Result<PathBuf> {
    let spec = HARNESS_REGISTRY.iter().find(|spec| field == spec.name || field == spec.legacy_root)
        .ok_or_else(|| harnesses_error(field, line, format!(
            "harnesses line {line}: {field:?} is not a local surface name; accepted surface names are {}; only built-in destinations are supported. Set a destination locally in ~/.config/skillherd/config.json (for example, claude_home or codex_home), not in shared policy",
            KNOWN_HARNESSES.join(", ")
        )))?;
    Ok((spec.surface)(paths))
}

/// The canonical harness a token may name: `omp` is the `agents` row's alias.
fn canonical_harness(name: &str) -> String {
    if name == OMP {
        AGENTS.to_string()
    } else {
        name.to_string()
    }
}

/// Resolve a `only:`/`not:` reference against the table, with the line number.
fn harness_reference(name: &str, line: usize, harnesses: &[HarnessRow]) -> Result<String> {
    let canonical = canonical_harness(name);
    if canonical != AGENTS && !harnesses.iter().any(|row| row.name == canonical) {
        return Err(Error::policy_unknown_harness(name, Some(line)));
    }
    Ok(canonical)
}

/// The representability rule: what the tokens ask must be expressible by the
/// table's read roots.
fn check_row_against(harnesses: &[HarnessRow], row: &SetRow) -> Result<()> {
    for harness in &row.only {
        if harness == AGENTS {
            return Err(unrepresentable(
                row,
                "`only:agents` (and its alias `only:omp`) cannot hold: the shared agents dir serves every reader of it, so no surface owns the row alone".to_string(),
            ));
        }
    }
    for harness in &row.not {
        let reads_shared = harnesses
            .iter()
            .any(|row| row.name == *harness && row.reads.contains(&ReadRoot::AgentsDir));
        if reads_shared {
            return Err(unrepresentable(
                row,
                format!(
                    "`not:{harness}` cannot hold: {harness} reads the shared agents dir, which serves this row to every reader of it"
                ),
            ));
        }
    }
    Ok(())
}

fn unrepresentable(row: &SetRow, reason: String) -> Error {
    let err = Error::policy_unrepresentable(&row.name, &reason);
    if row.line == 0 {
        err
    } else {
        with_line(err, row.line, "sets")
    }
}

fn push_platform(platforms: &mut Vec<Platform>, platform: Platform) {
    if !platforms.contains(&platform) {
        platforms.push(platform);
    }
}

/// A policy error that already names a file, given the line it came from.
fn with_line(err: Error, line: usize, file: &str) -> Error {
    match err {
        Error::InvalidName {
            message,
            mut details,
            suggested_fix,
        } => {
            details["line"] = json!(line);
            Error::InvalidName {
                message: format!("{message} ({file} line {line})"),
                details,
                suggested_fix,
            }
        }
        Error::PolicyUnrepresentable {
            message,
            mut details,
            suggested_fix,
        } => {
            details["line"] = json!(line);
            Error::PolicyUnrepresentable {
                message: format!("{message} ({file} line {line})"),
                details,
                suggested_fix,
            }
        }
        other => other,
    }
}

/// No valid token in either grammar needs more than 128 bytes. Refuse long
/// file-derived tokens before any error constructor can echo them verbatim.
/// Keep a UTF-8-safe prefix, never a whole malformed row or its other fields.
fn check_token_lengths(raw: &str, line: usize, file: &str) -> Result<()> {
    if let Some(token) = raw.split_whitespace().find(|token| token.len() > 128) {
        let mut end = 125;
        while !token.is_char_boundary(end) {
            end -= 1;
        }
        let shown = format!("{}…", &token[..end]);
        let message = format!("{file} line {line}: token {shown:?} exceeds 128 bytes");
        return Err(if file == "harnesses" {
            harnesses_error(&shown, line, message)
        } else {
            policy_bad_token(&shown, line, message)
        });
    }
    Ok(())
}

/// `policy_bad_token` with a message of our own (the constructor's wording is
/// about a lone token, and a whole row or a field needs more).
fn policy_bad_token(token: &str, line: usize, message: String) -> Error {
    Error::PolicyBadToken {
        message,
        details: json!({ "token": token, "line": line }),
        suggested_fix: Some(
            "a sets row is `name token…` with tokens `main`, `lane`, `!mac`, `!linux`, `only:<harness>` and `not:<harness>`"
                .to_string(),
        ),
    }
}

/// The `harnesses` grammar error: the fix text names *that* file's row shape,
/// never the `sets` row a caller of `policy_bad_token` would be handed, and says
/// that a trailing comment is not a comment.
fn harnesses_error(token: &str, line: usize, message: String) -> Error {
    Error::PolicyBadToken {
        message,
        details: json!({ "token": token, "line": line }),
        suggested_fix: Some(
            "a harnesses row is `harness tier surface variant-key reads` (`-` for a harness with no variant key, `surface,agents_dir` for the roots a harness reads); a `#` comment must stand on its own line, never after a row"
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SetRow;
    use tempfile::TempDir;

    fn row(name: &str, main: bool, lane: bool) -> SetRow {
        SetRow {
            name: name.to_string(),
            main,
            lane,
            not_platform: Vec::new(),
            only: Vec::new(),
            not: Vec::new(),
            line: 1,
        }
    }

    /// The hand-computed table from the plan: `!mac`, `not:grok`,
    /// `only:codex`, lane rows that are main rows too (`lane ⇒ main`), and a
    /// lane-tier surface that serves the lane rows only.
    fn table() -> (Policy, TempDir) {
        let dir = TempDir::new().unwrap();
        let mut policy = default_policy(&Paths::fixture(dir.path()));

        let mut beta = row("beta", true, false);
        beta.not_platform = vec![Platform::Mac];

        let mut gamma = row("gamma", true, false);
        gamma.not = vec!["grok".to_string()];

        let mut delta = row("delta", true, false);
        delta.only = vec!["codex".to_string()];

        let epsilon = row("epsilon", true, true);

        let mut zeta = row("zeta", true, true);
        zeta.not_platform = vec![Platform::Linux];

        policy.rows = vec![row("alpha", true, false), beta, gamma, delta, epsilon, zeta];
        (policy, dir)
    }

    #[test]
    fn load_validates_the_loaded_table_without_main_preflight() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        assert!(paths.validate().is_ok(), "the default roots are disjoint");
        std::fs::create_dir_all(paths.library.join(".globals")).unwrap();
        std::fs::write(
            paths.library.join(".globals/harnesses"),
            "claude main claude - surface\ncodex main claude - surface\n",
        )
        .unwrap();
        let error = load(&paths).unwrap_err();
        assert_eq!(error.code(), "config_error");
        assert!(
            error
                .to_string()
                .contains("surfaces claude and codex overlap")
                || error
                    .to_string()
                    .contains("surfaces codex and claude overlap"),
            "{error}"
        );
    }

    #[test]
    fn duplicate_harness_names_are_refused_even_for_disjoint_local_roots() {
        let paths = Paths::fixture("/sandbox/user");
        let error = parse_harnesses(
            "claude main claude claude surface\nclaude main codex codex surface\n",
            &paths,
        )
        .unwrap_err();
        assert!(error.to_string().contains("duplicate harness claude"));
    }

    #[test]
    fn the_default_table_is_the_six_plan_rows() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        let policy = default_policy(&paths);
        assert!(policy.rows.is_empty());
        let names: Vec<&str> = policy
            .harnesses
            .iter()
            .map(|row| row.name.as_str())
            .collect();
        assert_eq!(names, ["claude", "codex", "agents", "grok", "pi", "cursor"]);

        let claude = policy.harness("claude").unwrap();
        assert_eq!(claude.tier, Tier::Main);
        assert_eq!(claude.surface, paths.claude_shared.join("skills"));
        assert_eq!(claude.variant_key.as_deref(), Some("claude"));
        assert_eq!(claude.reads, vec![ReadRoot::Surface]);

        let codex = policy.harness("codex").unwrap();
        assert_eq!(codex.tier, Tier::Main);
        assert_eq!(codex.reads, vec![ReadRoot::Surface, ReadRoot::AgentsDir]);

        let agents = policy.harness("agents").unwrap();
        assert_eq!(agents.tier, Tier::Lane);
        assert_eq!(agents.surface, paths.agents_dir);
        assert_eq!(agents.variant_key, None);

        let grok = policy.harness("grok").unwrap();
        assert_eq!(grok.tier, Tier::None);
        assert_eq!(grok.reads, vec![ReadRoot::Surface, ReadRoot::AgentsDir]);

        let pi = policy.harness("pi").unwrap();
        assert_eq!(pi.tier, Tier::Lane);
        assert_eq!(pi.variant_key.as_deref(), Some("pi"));
        assert_eq!(pi.surface, paths.pi_skills);

        let cursor = policy.harness("cursor").unwrap();
        assert_eq!(cursor.tier, Tier::None);
        assert_eq!(cursor.variant_key.as_deref(), Some("cursor"));
        assert_eq!(cursor.reads, vec![ReadRoot::Surface, ReadRoot::AgentsDir]);
    }

    #[test]
    fn hand_computed_table_on_mac() {
        let (policy, _dir) = table();
        let mac = Platform::Mac;

        let hashtag = |items: Vec<&str>| items.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();

        assert_eq!(
            effective_set(&policy, "claude", mac).unwrap(),
            hashtag(vec!["alpha", "epsilon", "gamma", "zeta"]),
            "beta is !mac, delta is only:codex; the lane rows are main rows too"
        );
        assert_eq!(
            effective_set(&policy, "codex", mac).unwrap(),
            hashtag(vec!["alpha", "delta", "epsilon", "gamma", "zeta"]),
            "codex's own surface serves every main row, lane rows included"
        );
        assert_eq!(
            effective_set(&policy, "agents", mac).unwrap(),
            hashtag(vec!["epsilon", "zeta"]),
            "the shared root serves the lane rows only (and would drop any only: row)"
        );
        assert_eq!(
            effective_set(&policy, "grok", mac).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            visible_set(&policy, "grok", mac).unwrap(),
            hashtag(vec!["epsilon", "zeta"]),
            "tier none surface, reads agents_dir: grok sees the lane set"
        );
        assert_eq!(
            visible_set(&policy, "codex", mac).unwrap(),
            hashtag(vec!["alpha", "delta", "epsilon", "gamma", "zeta"]),
            "codex reads its own surface plus agents_dir"
        );
        assert_eq!(
            visible_set(&policy, "claude", mac).unwrap(),
            hashtag(vec!["alpha", "epsilon", "gamma", "zeta"])
        );
        assert_eq!(
            visible_set(&policy, "omp", mac).unwrap(),
            hashtag(vec!["epsilon", "zeta"]),
            "omp reads agents_dir only"
        );
        assert_eq!(
            effective_set(&policy, "omp", mac).unwrap(),
            hashtag(vec!["epsilon", "zeta"])
        );
        assert_eq!(
            effective_set(&policy, "pi", mac).unwrap(),
            hashtag(vec!["epsilon", "zeta"])
        );
        assert_eq!(
            effective_set(&policy, "cursor", mac).unwrap(),
            Vec::<String>::new(),
            "cursor is tier none: its own surface holds no managed entries"
        );
        assert_eq!(
            visible_set(&policy, "cursor", mac).unwrap(),
            hashtag(vec!["epsilon", "zeta"]),
            "cursor reads agents_dir at startup (V1), so it sees the lane set"
        );
    }

    #[test]
    fn hand_computed_table_on_linux() {
        let (policy, _dir) = table();
        let linux = Platform::Linux;
        assert_eq!(
            effective_set(&policy, "claude", linux).unwrap(),
            vec![
                "alpha".to_string(),
                "beta".to_string(),
                "epsilon".to_string(),
                "gamma".to_string()
            ],
            "beta is only excluded on mac; zeta is !linux"
        );
        assert_eq!(
            effective_set(&policy, "agents", linux).unwrap(),
            vec!["epsilon".to_string()],
            "the lane rows only: zeta is !linux"
        );
    }

    #[test]
    fn not_grok_is_subtracted_from_groks_own_surface() {
        let (mut policy, _dir) = table();
        for row in &mut policy.harnesses {
            if row.name == "grok" {
                row.tier = Tier::Main;
            }
        }
        let names = effective_set(&policy, "grok", Platform::Mac).unwrap();
        assert!(!names.contains(&"gamma".to_string()), "{names:?}");
        assert!(names.contains(&"alpha".to_string()));
    }

    #[test]
    fn only_codex_is_kept_for_codex_and_dropped_for_the_shared_root() {
        let (policy, _dir) = table();
        assert!(
            effective_set(&policy, "codex", Platform::Mac)
                .unwrap()
                .contains(&"delta".to_string())
        );
        for harness in ["claude", "agents", "pi", "cursor"] {
            assert!(
                !effective_set(&policy, harness, Platform::Mac)
                    .unwrap()
                    .contains(&"delta".to_string()),
                "{harness} must not serve an only:codex row"
            );
        }
    }

    #[test]
    fn tier_none_serves_nothing_whatever_the_rows_say() {
        let (policy, _dir) = table();
        assert!(
            effective_set(&policy, "grok", Platform::Linux)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn lane_rows_are_main_rows_and_the_lane_surface_serves_only_them() {
        let (policy, _dir) = table();
        let claude = effective_set(&policy, "claude", Platform::Mac).unwrap();
        assert!(claude.contains(&"epsilon".to_string()), "lane ⇒ main");
        let agents = effective_set(&policy, "agents", Platform::Mac).unwrap();
        assert!(agents.contains(&"epsilon".to_string()));
        assert!(
            !agents.contains(&"alpha".to_string()),
            "a main-only row never reaches the shared lane dir"
        );
    }

    #[test]
    fn empty_policy_has_empty_sets() {
        let dir = TempDir::new().unwrap();
        let policy = default_policy(&Paths::fixture(dir.path()));
        for harness in KNOWN_HARNESSES {
            assert!(
                effective_set(&policy, harness, Platform::Mac)
                    .unwrap()
                    .is_empty()
            );
            assert!(
                visible_set(&policy, harness, Platform::Mac)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn unknown_harness_names_the_option() {
        let dir = TempDir::new().unwrap();
        let policy = default_policy(&Paths::fixture(dir.path()));
        let err = effective_set(&policy, "nope", Platform::Mac).unwrap_err();
        assert_eq!(err.code(), "policy_unknown_harness");
        assert_eq!(err.exit_code(), 65);
        assert!(err.to_string().contains("nope"));
        assert!(err.suggested_fix().unwrap().contains("claude"));
        assert_eq!(
            visible_set(&policy, "nope", Platform::Mac)
                .unwrap_err()
                .code(),
            "policy_unknown_harness"
        );
    }

    fn fixture_sets() -> String {
        [
            "# curation history, carried over verbatim",
            "",
            "alpha  main",
            "beta main lane   !mac",
            "",
            "# lane",
            "gamma lane main",
        ]
        .join("\n")
            + "\n"
    }

    fn write_sets(dir: &TempDir, text: &str) {
        let globals = dir.path().join(".agents/skill-library/.globals");
        std::fs::create_dir_all(&globals).unwrap();
        std::fs::write(globals.join("sets"), text).unwrap();
        std::fs::write(
            globals.join("harnesses"),
            "claude main {claude_shared}/skills claude surface\n\
             agents lane {agents_dir} - surface\n",
        )
        .unwrap();
    }

    #[test]
    fn load_reads_rows_comments_and_the_harness_table() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        write_sets(&dir, &fixture_sets());

        let policy = load(&paths).unwrap();
        assert_eq!(policy.raw_lines.len(), 8, "{:?}", policy.raw_lines);
        let names: Vec<&str> = policy.rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["alpha", "beta", "gamma"]);
        assert_eq!(policy.rows[0].line, 3, "1-based line of the first row");
        assert_eq!(policy.rows[1].line, 4);
        assert_eq!(policy.rows[1].not_platform, vec![Platform::Mac]);
        assert!(policy.rows[2].lane && policy.rows[2].main);

        let harnesses: Vec<&str> = policy
            .harnesses
            .iter()
            .map(|row| row.name.as_str())
            .collect();
        assert_eq!(
            harnesses,
            ["claude", "agents"],
            "the file replaces the table"
        );
        assert_eq!(
            policy.harness("claude").unwrap().surface,
            paths.claude_shared.join("skills"),
            "{{claude_shared}} resolves from Paths"
        );
        assert_eq!(policy.harness("agents").unwrap().surface, paths.agents_dir);
        assert_eq!(policy.harness("agents").unwrap().variant_key, None);
    }

    #[test]
    fn a_missing_or_empty_library_loads_as_no_rows() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        let policy = load(&paths).unwrap();
        assert!(policy.rows.is_empty());
        assert!(policy.raw_lines.is_empty());
        assert_eq!(policy.harnesses.len(), 6, "no harnesses file: the table");
    }

    #[test]
    fn an_empty_harnesses_file_falls_back_to_the_table() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        write_sets(&dir, "");
        std::fs::write(
            dir.path().join(".agents/skill-library/.globals/harnesses"),
            "# every row removed\n",
        )
        .unwrap();
        let policy = load(&paths).unwrap();
        assert_eq!(policy.harnesses.len(), 6);
    }

    #[test]
    fn grammar_errors_name_the_line() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());

        write_sets(&dir, "alpha main\nbeta main bogus\n");
        let err = load(&paths).unwrap_err();
        assert_eq!(err.code(), "policy_bad_token");
        assert_eq!(err.details()["line"], 2);
        assert_eq!(err.exit_code(), 65);

        write_sets(&dir, "alpha main lane\n#\nbeta lane\n");
        let err = load(&paths).unwrap_err();
        assert_eq!(err.code(), "policy_lane_not_in_main");
        assert_eq!(err.details()["line"], 3);
        assert_eq!(err.details()["name"], "beta");

        write_sets(&dir, "alpha main only:nope\n");
        let err = load(&paths).unwrap_err();
        assert_eq!(err.code(), "policy_unknown_harness");
        assert_eq!(err.details()["line"], 1);

        write_sets(&dir, "alpha main\n../etc main\n");
        let err = load(&paths).unwrap_err();
        assert_eq!(err.code(), "invalid_name");
        assert_eq!(err.details()["line"], 2);
        assert!(err.to_string().contains("sets line 2"), "{err}");

        write_sets(&dir, "alpha main # notion\n");
        let err = load(&paths).unwrap_err();
        assert_eq!(err.code(), "policy_bad_token");
        assert_eq!(err.details()["line"], 1);

        write_sets(&dir, "alpha main lane not:agents\n");
        let policy = load(&paths).expect("the table's agents row reads its own surface only");
        assert_eq!(policy.rows[0].not, vec!["agents".to_string()]);
    }

    #[test]
    fn unrepresentable_rows_name_the_line() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        write_sets(&dir, &fixture_sets());
        std::fs::write(
            dir.path().join(".agents/skill-library/.globals/harnesses"),
            "codex main {codex_home}/skills codex surface,agents_dir\n\
             agents lane {agents_dir} - surface\n",
        )
        .unwrap();

        std::fs::write(
            dir.path().join(".agents/skill-library/.globals/sets"),
            "alpha main lane not:codex\n",
        )
        .unwrap();
        let err = load(&paths).unwrap_err();
        assert_eq!(err.code(), "policy_unrepresentable");
        assert_eq!(err.details()["line"], 1);
        assert!(err.to_string().contains("shared agents dir"), "{err}");

        for line in ["alpha main only:agents\n", "alpha main only:omp\n"] {
            std::fs::write(dir.path().join(".agents/skill-library/.globals/sets"), line).unwrap();
            let err = load(&paths).unwrap_err();
            assert_eq!(err.code(), "policy_unrepresentable", "{line}");
            assert_eq!(err.details()["line"], 1);
        }
    }

    #[test]
    fn omp_normalizes_to_the_agents_row() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        write_sets(&dir, "alpha main not:omp\n");
        let policy = load(&paths).unwrap();
        assert_eq!(policy.rows[0].not, vec!["agents".to_string()]);
    }

    #[test]
    fn load_save_round_trips_byte_identically() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        write_sets(&dir, &fixture_sets());
        let path = paths.library.join(".globals/sets");
        let before = std::fs::read_to_string(&path).unwrap();

        let policy = load(&paths).unwrap();
        save(&paths, &policy).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn save_keeps_untouched_rows_and_positions() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        write_sets(&dir, &fixture_sets());
        let mut policy = load(&paths).unwrap();

        policy.rows[0].not = vec!["grok".to_string()];
        policy.rows.remove(1); // beta goes away
        policy.rows.push(SetRow {
            name: "delta".to_string(),
            main: true,
            lane: true,
            not_platform: Vec::new(),
            only: Vec::new(),
            not: Vec::new(),
            line: 0,
        });
        save(&paths, &policy).unwrap();

        let text = std::fs::read_to_string(paths.library.join(".globals/sets")).unwrap();
        assert_eq!(
            text,
            "# curation history, carried over verbatim\n\nalpha main not:grok\n\n# lane\ngamma lane main\n\ndelta main lane\n",
            "edited in place, removed gone, appended after a blank line"
        );
    }

    #[test]
    fn save_writes_nothing_when_the_file_already_says_it() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        save(&paths, &default_policy(&paths)).unwrap();
        assert!(
            !paths.library.join(".globals/sets").exists(),
            "no rows and no file: nothing to write"
        );
    }

    #[test]
    fn save_refuses_an_unrepresentable_row_before_writing() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        let mut policy = default_policy(&paths);
        policy.rows.push(SetRow {
            name: "alpha".to_string(),
            main: false,
            lane: true,
            not_platform: Vec::new(),
            only: Vec::new(),
            not: vec!["codex".to_string()],
            line: 0,
        });
        let err = save(&paths, &policy).unwrap_err();
        assert_eq!(err.code(), "policy_unrepresentable");
        assert!(!paths.library.join(".globals/sets").exists());
    }

    #[test]
    fn check_row_reports_a_cli_combination_without_a_line() {
        let dir = TempDir::new().unwrap();
        let policy = default_policy(&Paths::fixture(dir.path()));
        let row = SetRow {
            name: "alpha".to_string(),
            main: true,
            lane: true,
            not_platform: Vec::new(),
            only: Vec::new(),
            not: vec!["grok".to_string()],
            line: 0,
        };
        let err = check_row(&policy, &row).unwrap_err();
        assert_eq!(err.code(), "policy_unrepresentable");
        assert_eq!(err.details()["name"], "alpha");
        assert!(err.details().get("line").is_none(), "no line for a CLI row");
        assert!(
            check_row(
                &policy,
                &SetRow {
                    not: vec!["claude".to_string()],
                    ..row
                }
            )
            .is_ok(),
            "claude reads its own surface only"
        );
    }

    #[test]
    fn row_lines_are_canonical() {
        let row = SetRow {
            name: "alpha".to_string(),
            main: true,
            lane: true,
            not_platform: vec![Platform::Linux],
            only: vec!["codex".to_string()],
            not: vec!["claude".to_string()],
            line: 7,
        };
        assert_eq!(
            row_line(&row),
            "alpha main lane !linux only:codex not:claude"
        );
    }
}
