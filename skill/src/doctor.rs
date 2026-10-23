//! Doctor: findings about the pool, the policy and the surfaces.
//!
//! **Detect, then fix.** Every detector here is a pure read; the only writes in
//! this module live behind `run(paths, fix: true, …)`, and nothing it removes is
//! deleted — entries are *moved* into `$HOME/.agents/skill-park/doctor-<ts>/`
//! and reported as `quarantine`. A fix run refuses as a whole (exit 4) when a
//! path it would touch lies lexically outside `HOME`.
//!
//! `$CODEX_HOME/config.toml` and `$GROK_HOME/config.toml` are read textually
//! (no TOML crate): doctor only needs two keys out of them, and a hand-edited
//! config with comments must not become a parse error.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::agents::{self, AgentSurfaceReport};
use crate::error::{
    EXIT_DOCTOR_FINDINGS, EXIT_DOCTOR_FIX_FAILED, EXIT_DOCTOR_PARTIAL_FIX, EXIT_DOCTOR_REFUSED,
    Error, Result,
};
use crate::hash::committed_tree_hash;
use crate::managed::{self, EntryKind};
use crate::model::{NpxMeta, PlanOp, Policy, ReadRoot, Severity, Tier, now_iso8601};
use crate::names::validate_name;
use crate::paths::{Paths, lexical_abs, normalize};
use crate::render::{self, RenderOpts};
use crate::{output, policy};

/// The park root the fix run moves what it removes into.
pub const PARK_DIR: &str = ".agents/skill-park";

/// The finding code a refused `--fix` records.
pub const REFUSED_CODE: &str = "refused_outside_home";

/// One published finding code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindingCode {
    pub code: &'static str,
    pub severity: Severity,
    pub fixable: bool,
    /// The command (or the one edit) that resolves the code.
    pub remediation: &'static str,
}

/// Every finding code doctor can report, with its severity and fixability.
///
/// The scan, `--capabilities` and the exit arithmetic all read this table, so
/// they cannot disagree about a code.
pub const FINDING_CODES: [FindingCode; 20] = [
    FindingCode {
        code: "inbox_unreconciled",
        severity: Severity::Warn,
        fixable: false,
        remediation: "`skillherd reconcile` retries retained stage arrivals; `skillherd reconcile --adopt NAME` accepts one or `skillherd reconcile --discard NAME` discards it",
    },
    FindingCode {
        code: "dangling_link",
        severity: Severity::Warn,
        fixable: true,
        remediation: "`skillherd doctor --fix` moves it to skill-park (or `skillherd render` relinks it)",
    },
    FindingCode {
        code: "foreign_entry",
        severity: Severity::Info,
        fixable: false,
        remediation: "leave it if another tool owns {path}; otherwise `mv {path} <HOME>/.agents/skill-park/`",
    },
    FindingCode {
        code: "foreign_shadow",
        severity: Severity::Warn,
        fixable: false,
        remediation: "park or rename the foreign entry, then `skillherd render`",
    },
    FindingCode {
        code: "surface_drift",
        severity: Severity::Warn,
        fixable: true,
        remediation: "`skillherd render`",
    },
    FindingCode {
        code: "policy_name_missing",
        severity: Severity::Warn,
        fixable: false,
        remediation: "`skillherd off {name}` drops the row; `skillherd reconcile` adds a missing pool entry",
    },
    FindingCode {
        code: "codex_disable_entry",
        severity: Severity::Warn,
        fixable: false,
        remediation: "delete that `[[skills.config]]` table (backup first: `cp -f {path} {path}.bak`)",
    },
    FindingCode {
        code: "grok_compat_skills_on",
        severity: Severity::Warn,
        fixable: false,
        remediation: "add `skills = false` under the named `[compat.<harness>]` section of {path}",
    },
    FindingCode {
        code: "visibility_excess",
        severity: Severity::Info,
        fixable: false,
        remediation: "`skillherd list --harness {name}` shows the excess; turn the harness's cross-harness scans off",
    },
    FindingCode {
        code: "project_npx_install",
        severity: Severity::Info,
        fixable: false,
        remediation: "`skillherd add {name} --project {project}` then delete the npx copy",
    },
    FindingCode {
        code: "lock_drift",
        severity: Severity::Warn,
        fixable: false,
        remediation: "`SKILLHERD_SNAPSHOT_LOCK=1 skillherd reconcile` rewrites the pool's lock snapshot from the live lock",
    },
    FindingCode {
        code: "variant_drift",
        severity: Severity::Warn,
        fixable: false,
        remediation: "`skillherd reconcile --adopt {name}` takes upstream again; a local variant is a fork",
    },
    FindingCode {
        code: "variant_pending",
        severity: Severity::Warn,
        fixable: false,
        remediation: "`skillherd reconcile` retries pending variants whenever the network is up",
    },
    FindingCode {
        code: "variant_unserved",
        severity: Severity::Info,
        fixable: false,
        remediation: "no harness row serves that variant key; `skillherd list --harness {name}` shows what is served",
    },
    FindingCode {
        code: "local_edit",
        severity: Severity::Info,
        fixable: false,
        remediation: "`skillherd reconcile --adopt {name}` takes upstream, `--keep {name}` keeps the house copy",
    },
    FindingCode {
        code: "untracked_upstream",
        severity: Severity::Warn,
        fixable: false,
        remediation: "`skillherd install <source> --skill {name} --adopt {name}` takes upstream and starts tracking it; `--keep {name}` declares the pool copy house",
    },
    FindingCode {
        code: "unknown_provenance",
        // A skill written by hand is the normal case for a new pool, so this
        // is a note, not a failure: `doctor` stays 0 on the quick start.
        severity: Severity::Info,
        fixable: false,
        remediation: "`skillherd install <owner/repo> --skill {name} --adopt {name}` tracks its upstream; a skill written here goes in .globals/house",
    },
    FindingCode {
        code: "house_name_missing",
        severity: Severity::Warn,
        fixable: false,
        remediation: "delete the line from .globals/house, or restore the pool entry",
    },
    FindingCode {
        code: "agent_shadow",
        severity: Severity::Warn,
        fixable: false,
        remediation: "move {path} out of the agents directory (this tool never replaces a file it did not create), then `skillherd render`",
    },
    FindingCode {
        code: "agent_drift",
        severity: Severity::Warn,
        fixable: false,
        remediation: "`skillherd update {name}` re-imports the agents from upstream unless they were edited by hand; an edited agent is a fork",
    },
];

fn code_row(code: &str) -> FindingCode {
    FINDING_CODES
        .iter()
        .copied()
        .find(|row| row.code == code)
        .expect("every finding code is published in FINDING_CODES")
}

/// One diagnostic finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    /// A stable finding code: `dangling_link`, `foreign_entry`, …
    pub code: String,
    pub severity: Severity,
    pub path: PathBuf,
    pub message: String,
    pub fixable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
}

/// The doctor's report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorReport {
    pub findings: Vec<Finding>,
    pub fixed: Vec<Finding>,
    pub failed: Vec<Finding>,
    /// Where `--fix` moved what it removed, when it removed anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quarantine: Option<PathBuf>,
    /// `--fix --dry-run`: the fixes that would run, as plan operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Vec<PlanOp>>,
    /// Set when `--fix` refused as a whole (exit 4); nothing was touched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

/// One code in the published contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FindingCapability {
    pub code: String,
    pub severity: Severity,
    pub fixable: bool,
    /// The command (or the one edit) that resolves the code.
    pub remediation: String,
}

/// One row of doctor's linter-style exit dictionary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorExitCode {
    pub code: i32,
    pub outcome: String,
    pub meaning: String,
}

/// What `doctor --capabilities` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorCapabilities {
    pub codes: Vec<FindingCapability>,
    pub exit_codes: Vec<DoctorExitCode>,
    /// The code a refused `--fix` records in `failed`.
    pub refusal_code: String,
}

/// The capabilities contract, as data.
pub fn capabilities() -> DoctorCapabilities {
    DoctorCapabilities {
        codes: FINDING_CODES
            .iter()
            .map(|row| FindingCapability {
                code: row.code.to_string(),
                severity: row.severity,
                fixable: row.fixable,
                remediation: row.remediation.to_string(),
            })
            .collect(),
        exit_codes: vec![
            DoctorExitCode {
                code: 0,
                outcome: "healthy".into(),
                meaning: "no warn or error findings (info findings do not count)".into(),
            },
            DoctorExitCode {
                code: 1,
                outcome: "findings".into(),
                meaning: "warn or error findings remain; the report is on stdout".into(),
            },
            DoctorExitCode {
                code: 2,
                outcome: "partial_fix".into(),
                meaning: "some fixes landed and some failed; the report is on stdout".into(),
            },
            DoctorExitCode {
                code: 3,
                outcome: "fix_failed".into(),
                meaning: "every attempted fix failed; the report is on stdout".into(),
            },
            DoctorExitCode {
                code: 4,
                outcome: "refused".into(),
                meaning: "a fix path lies outside HOME; nothing was touched".into(),
            },
        ],
        refusal_code: REFUSED_CODE.to_string(),
    }
}

/// The exit status a report publishes (doctor's linter dictionary).
pub fn exit_code(report: &DoctorReport) -> i32 {
    if report.refused.is_some() {
        return EXIT_DOCTOR_REFUSED;
    }
    if !report.failed.is_empty() {
        return if report.fixed.is_empty() {
            EXIT_DOCTOR_FIX_FAILED
        } else {
            EXIT_DOCTOR_PARTIAL_FIX
        };
    }
    let fixed: BTreeSet<(&str, &Path)> = report
        .fixed
        .iter()
        .map(|finding| (finding.code.as_str(), finding.path.as_path()))
        .collect();
    let remaining = report.findings.iter().any(|finding| {
        finding.severity != Severity::Info
            && !fixed.contains(&(finding.code.as_str(), finding.path.as_path()))
    });
    if remaining { EXIT_DOCTOR_FINDINGS } else { 0 }
}

/// Scan, and with `fix` repair what is safe.
///
/// Findings are not a failure: the caller publishes the linter exit status with
/// [`exit_code`] and `crate::output::request_exit_code`.
pub fn run(paths: &Paths, fix: bool, dry_run: bool) -> Result<DoctorReport> {
    if fix {
        // Diagnostic loading below can tolerate source overlaps, but a repair
        // must retain the same strict preflight as every other mutator.
        policy::load(paths)?;
    }
    let policy = policy::load_for_inspection(paths)?;
    let mut findings = Vec::new();

    scan_inbox(paths, &mut findings)?;
    scan_surfaces(paths, &policy, &mut findings)?;
    scan_policy_names(paths, &policy, &mut findings);
    scan_render_drift(paths, &mut findings);
    scan_codex_config(paths, &mut findings)?;
    scan_grok_config(paths, &mut findings)?;
    scan_visibility(paths, &policy, &mut findings)?;
    scan_project_install(paths, &mut findings)?;
    scan_lock_snapshot(paths, &mut findings)?;
    scan_npx_records(paths, &policy, &mut findings)?;
    scan_provenance(paths, &mut findings)?;

    findings.sort_by(|left, right| {
        (&left.code, &left.path, &left.message).cmp(&(&right.code, &right.path, &right.message))
    });
    findings.dedup();

    let mut report = DoctorReport {
        findings,
        fixed: Vec::new(),
        failed: Vec::new(),
        quarantine: None,
        plan: None,
        refused: None,
    };
    if fix {
        apply_fixes(paths, &mut report, dry_run);
    }
    Ok(report)
}

// ---- findings ---------------------------------------------------------

fn finding(code: &str, path: &Path, message: String) -> Finding {
    let row = code_row(code);
    let hint = row
        .remediation
        .replace("{path}", &path.display().to_string());
    Finding {
        code: code.to_string(),
        severity: row.severity,
        path: path.to_path_buf(),
        message,
        fixable: row.fixable,
        suggested_fix: Some(hint),
    }
}

/// A finding whose suggested fix names this instance's exact command.
fn finding_with_hint(code: &str, path: &Path, message: String, hint: String) -> Finding {
    let row = code_row(code);
    Finding {
        code: code.to_string(),
        severity: row.severity,
        path: path.to_path_buf(),
        message,
        fixable: row.fixable,
        suggested_fix: Some(hint),
    }
}

/// Real inbox entries: a directory with `SKILL.md` and no managed marker.
fn is_inbox_candidate(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(meta) => meta.is_dir() && path.join("SKILL.md").is_file() && !managed::has_marker(path),
        Err(_) => false,
    }
}

fn library_entry_exists(paths: &Paths, name: &str) -> bool {
    fs::symlink_metadata(paths.library.join(name)).is_ok()
}

/// Non-dot child names of a directory; a missing directory is empty.
fn subdir_names(dir: &Path) -> Result<Vec<String>> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::io(&format!("reading {}", dir.display()), &err)),
    };
    let mut names = Vec::new();
    for dirent in read {
        let dirent =
            dirent.map_err(|err| Error::io(&format!("reading {}", dir.display()), &err))?;
        let name = dirent.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        names.push(name);
    }
    names.sort();
    Ok(names)
}

fn scan_inbox(paths: &Paths, findings: &mut Vec<Finding>) -> Result<()> {
    // The shared agents directory is a harness surface, not an import inbox.
    // A retained arrival matters even if the pool already contains this name:
    // it may be a refused update, rather than a never-imported skill.
    for inbox in crate::npx::recoverable_inboxes(paths)? {
        for name in subdir_names(&inbox)? {
            let entry = inbox.join(&name);
            if !is_inbox_candidate(&entry) || validate_name(&name).is_err() {
                continue;
            }
            findings.push(finding_with_hint(
                "inbox_unreconciled",
                &entry,
                format!("{name} is a retained arrival in the tool-owned stage ({}); reconcile has not consumed it", inbox.display()),
                format!("`skillherd reconcile` retries the arrival; if refused, `skillherd reconcile --adopt {name}` accepts it or `skillherd reconcile --discard {name}` discards it without removing the pool copy"),
            ));
        }
    }
    Ok(())
}

fn scan_surfaces(paths: &Paths, policy: &Policy, findings: &mut Vec<Finding>) -> Result<()> {
    for row in &policy.harnesses {
        if !crate::paths::surface_available(&row.surface) {
            continue;
        }
        let desired: BTreeSet<String> = policy::effective_set(policy, &row.name, paths.platform)?
            .into_iter()
            .collect();
        for entry in managed::list_entries(&row.surface, paths)? {
            if entry.managed {
                if entry.is_dangling() {
                    let target = entry
                        .symlink_target()
                        .map(|target| target.display().to_string())
                        .unwrap_or_default();
                    findings.push(finding(
                        "dangling_link",
                        &entry.path,
                        format!(
                            "{} is a managed link whose target ({target}) does not exist",
                            entry.path.display()
                        ),
                    ));
                }
            } else {
                findings.push(finding(
                    "foreign_entry",
                    &entry.path,
                    format!(
                        "{} is not ours (not a managed link, no marker); it is reported and never touched",
                        entry.path.display()
                    ),
                ));
            }
            if !entry.managed && desired.contains(&entry.name) {
                findings.push(finding(
                    "foreign_shadow",
                    &entry.path,
                    format!(
                        "{} hides {} which the {} policy wants served; the harness sees this entry instead",
                        entry.path.display(),
                        entry.name,
                        row.name
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn scan_policy_names(paths: &Paths, policy: &Policy, findings: &mut Vec<Finding>) {
    for row in &policy.rows {
        // A row that excludes this machine activates nothing here, so its
        // skill may legitimately be absent from this machine's pool.
        if row.not_platform.contains(&paths.platform) || library_entry_exists(paths, &row.name) {
            continue;
        }
        findings.push(finding_with_hint(
            "policy_name_missing",
            &paths.library.join(&row.name),
            format!(
                "sets line {} activates {:?}, but the pool has no such entry",
                row.line, row.name
            ),
            format!(
                "`skillherd off {}` drops the row, or `skillherd reconcile` imports the skill if it is in the inbox",
                row.name
            ),
        ));
    }
}

/// One finding per operation a dry-run render plans.
fn drift_findings(plan: &[PlanOp]) -> Vec<Finding> {
    plan.iter()
        .map(|op| {
            let target = op
                .target
                .as_ref()
                .map(|target| format!(" -> {}", target.display()))
                .unwrap_or_default();
            finding(
                "surface_drift",
                &op.path,
                format!(
                    "`skillherd render` would {} {}{target}",
                    op.op,
                    op.path.display()
                ),
            )
        })
        .collect()
}

/// One finding per foreign entry holding a path an active skill's agent needs.
fn agent_shadow_findings(reports: &[AgentSurfaceReport]) -> Vec<Finding> {
    reports
        .iter()
        .flat_map(|report| {
            report.blocked.iter().map(move |entry| {
                finding(
                    "agent_shadow",
                    &entry.path,
                    format!(
                        "{} is {}, not a link this tool created, so the {} agent {} is not served from the pool",
                        entry.path.display(),
                        managed::describe_entry(entry),
                        report.harness,
                        entry.name
                    ),
                )
            })
        })
        .collect()
}

fn scan_render_drift(paths: &Paths, findings: &mut Vec<Finding>) {
    let opts = RenderOpts {
        surfaces: Vec::new(),
        dry_run: true,
        only: None,
    };
    match render::run(paths, opts) {
        Ok(report) => {
            findings.extend(drift_findings(&report.plan));
            findings.extend(agent_shadow_findings(&report.agents));
        }
        Err(error) => output::warn(
            "surface_drift_unchecked",
            format!("surface drift was not checked: {error}"),
        ),
    }
}

/// One `[[skills.config]]` table Codex reads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexEntry {
    path: String,
    line: usize,
    enabled: bool,
}

/// The `[[skills.config]]` tables that disable a skill, parsed textually.
fn codex_entries(text: &str) -> Vec<CodexEntry> {
    let mut out = Vec::new();
    let mut current: Option<CodexEntry> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.starts_with('[') {
            if let Some(entry) = current.take()
                && !entry.enabled
                && !entry.path.is_empty()
            {
                out.push(entry);
            }
            if line.starts_with("[[skills.config]]") {
                current = Some(CodexEntry {
                    path: String::new(),
                    line: index + 1,
                    enabled: true,
                });
            }
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        if let Some(value) = table_value(line, "path") {
            entry.path = unquote(value);
        } else if let Some(value) = table_value(line, "enabled") {
            entry.enabled = value.trim() != "false";
        }
    }
    if let Some(entry) = current.take()
        && !entry.enabled
        && !entry.path.is_empty()
    {
        out.push(entry);
    }
    out
}

fn scan_codex_config(paths: &Paths, findings: &mut Vec<Finding>) -> Result<()> {
    let config = paths.codex_home.join("config.toml");
    let text = match fs::read_to_string(&config) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(Error::io(&format!("reading {}", config.display()), &err)),
    };
    for entry in codex_entries(&text) {
        let disabled = lexical_abs(&paths.home, &expand_home(&paths.home, &entry.path));
        if !(disabled.starts_with(&paths.agents_dir) || disabled.starts_with(&paths.library)) {
            continue;
        }
        findings.push(finding_with_hint(
            "codex_disable_entry",
            &config,
            format!(
                "{}:{} disables the Codex skill {}; Codex reads {} and its own served dir, so the disable is pool bookkeeping that hides a real skill",
                config.display(),
                entry.line,
                entry.path,
                paths.agents_dir.display()
            ),
            format!(
                "delete that `[[skills.config]]` table in {} (backup first: `cp -f {0} {0}.bak`)",
                config.display()
            ),
        ));
    }
    Ok(())
}

/// Which `[compat.<harness>] skills` keys Grok has; `None` means "not set".
fn grok_compat_state(text: &str) -> BTreeMap<String, Option<bool>> {
    let mut state: BTreeMap<String, Option<bool>> = BTreeMap::new();
    for harness in ["claude", "codex"] {
        state.insert(harness.to_string(), None);
    }
    let mut section: Option<String> = None;
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.starts_with('[') {
            section = line
                .trim_matches(|character| character == '[' || character == ']')
                .trim()
                .strip_prefix("compat.")
                .map(str::to_string);
            continue;
        }
        let Some(current) = section.as_deref() else {
            continue;
        };
        let Some(value) = table_value(line, "skills") else {
            continue;
        };
        if let Some(state) = state.get_mut(current) {
            *state = Some(value.trim() == "true");
        }
    }
    state
}

fn scan_grok_config(paths: &Paths, findings: &mut Vec<Finding>) -> Result<()> {
    // A machine without Grok installed has no compat knob to report.
    if !paths.grok_home.is_dir() {
        return Ok(());
    }
    let config = paths.grok_home.join("config.toml");
    let text = match fs::read_to_string(&config) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(Error::io(&format!("reading {}", config.display()), &err)),
    };
    for (harness, state) in grok_compat_state(&text) {
        if state == Some(false) {
            continue;
        }
        let present = if state.is_none() {
            "is not set"
        } else {
            "is not `false`"
        };
        findings.push(finding_with_hint(
            "grok_compat_skills_on",
            &config,
            format!(
                "`[compat.{harness}] skills` {present} in {}; Grok reads Claude's or Codex's surfaces as well, so its visible set is larger than the lane set",
                config.display()
            ),
            format!(
                "add `skills = false` under `[compat.{harness}]` in {} (or set the harness's compat env knob to 0), then rerun `skillherd doctor`",
                config.display()
            ),
        ));
    }
    Ok(())
}

fn scan_visibility(paths: &Paths, policy: &Policy, findings: &mut Vec<Finding>) -> Result<()> {
    for row in &policy.harnesses {
        if row.tier != Tier::None {
            continue;
        }
        let intent = policy::effective_set(policy, &row.name, paths.platform)?;
        let visible = policy::visible_set(policy, &row.name, paths.platform)?;
        if visible.is_empty() || visible == intent {
            continue;
        }
        let shared: Vec<String> = row
            .reads
            .iter()
            .filter(|root| **root == ReadRoot::AgentsDir)
            .map(|_| paths.agents_dir.display().to_string())
            .collect();
        findings.push(finding_with_hint(
            "visibility_excess",
            &row.surface,
            format!(
                "{} is `none` in the harness table but sees {} skill(s) through {}; the policy wants nothing served there",
                row.name,
                visible.len(),
                if shared.is_empty() {
                    "its read roots".to_string()
                } else {
                    shared.join(", ")
                }
            ),
            format!(
                "`skillherd list --harness {}` shows the set; the excess comes from a shared read root, so turn the harness's cross-harness scans off",
                row.name
            ),
        ));
    }
    Ok(())
}

/// Real (not linked) skills installed inside the current project.
fn scan_project_install(paths: &Paths, findings: &mut Vec<Finding>) -> Result<()> {
    let cwd =
        std::env::current_dir().map_err(|err| Error::io("reading the working directory", &err))?;
    scan_project_install_from(paths, &cwd, findings)
}

/// Walk from `start` up to the nearest `.git`. A project root that resolves to
/// a surface the renderer owns (including a symlinked custom Claude root)
/// holds rendered copies, not project installs, so it is skipped.
fn scan_project_install_from(
    paths: &Paths,
    start: &Path,
    findings: &mut Vec<Finding>,
) -> Result<()> {
    let owned: Vec<PathBuf> = paths
        .surfaces()
        .into_iter()
        .filter_map(|(_, surface)| fs::canonicalize(surface).ok())
        .collect();
    let mut dir = start.to_path_buf();
    loop {
        for root in [".agents/skills", ".claude/skills", ".codex/skills"] {
            let skills = dir.join(root);
            if !skills.is_dir() {
                continue;
            }
            if fs::canonicalize(&skills)
                .map(|real| owned.contains(&real))
                .unwrap_or(false)
            {
                continue;
            }
            for name in subdir_names(&skills)? {
                let entry = skills.join(&name);
                let meta = match fs::symlink_metadata(&entry) {
                    Ok(meta) => meta,
                    Err(_) => continue,
                };
                if !meta.is_dir() || !entry.join("SKILL.md").is_file() {
                    continue;
                }
                findings.push(finding_with_hint(
                    "project_npx_install",
                    &entry,
                    format!(
                        "{} is a real directory installed in the project ({root}); project scope is not the pool's job and `npx skills` will not maintain it",
                        entry.display()
                    ),
                    format!(
                        "run `skillherd add {name} --project {}` to activate the pool copy, then delete the npx copy at {}",
                        dir.display(),
                        entry.display()
                    ),
                ));
            }
        }
        if dir.join(".git").exists() {
            break;
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => break,
        }
    }
    Ok(())
}

fn scan_lock_snapshot(paths: &Paths, findings: &mut Vec<Finding>) -> Result<()> {
    // Match npx::snapshot_lock's opt-in exactly. With snapshots disabled even
    // an existing snapshot is historical data, not a promise to track the lock.
    if std::env::var("SKILLHERD_SNAPSHOT_LOCK").as_deref() != Ok("1") {
        return Ok(());
    }
    let snapshot = paths.library.join(crate::npx::lock_snapshot(paths));
    let live = read_bytes(&paths.lock)?;
    let kept = read_bytes(&snapshot)?;
    match (live, kept) {
        (None, None) => Ok(()),
        (Some(live), Some(kept)) if live == kept => Ok(()),
        (Some(_), Some(_)) => {
            findings.push(finding(
                "lock_drift",
                &snapshot,
                format!(
                    "the live npx lock ({}) differs from the pool snapshot ({}); the pool's record of what npx installed is stale",
                    paths.lock.display(),
                    snapshot.display()
                ),
            ));
            Ok(())
        }
        (Some(_), None) => {
            findings.push(finding(
                "lock_drift",
                &snapshot,
                format!(
                    "the live npx lock ({}) has never been snapshotted into the pool",
                    paths.lock.display()
                ),
            ));
            Ok(())
        }
        (None, Some(_)) => {
            findings.push(finding(
                "lock_drift",
                &snapshot,
                format!(
                    "the pool holds a lock snapshot but the live lock ({}) is gone",
                    paths.lock.display()
                ),
            ));
            Ok(())
        }
    }
}

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::io(&format!("reading {}", path.display()), &err)),
    }
}

fn scan_npx_records(paths: &Paths, policy: &Policy, findings: &mut Vec<Finding>) -> Result<()> {
    let served: BTreeSet<String> = policy
        .harnesses
        .iter()
        .filter_map(|row| row.variant_key.clone())
        .collect();

    for name in subdir_names(&paths.library)? {
        let variants = paths.library.join(&name).join(".variants");
        if !variants.is_dir() {
            continue;
        }
        for key in subdir_names(&variants)? {
            if served.contains(&key) {
                continue;
            }
            findings.push(finding_with_hint(
                "variant_unserved",
                &variants.join(&key),
                format!(
                    "{name} stores a `.variants/{key}` tree, but no harness row serves the variant key {key:?}; it is imported and never served"
                ),
                format!("no harness reads a `{key}` variant; `skillherd list --harness claude` shows what is served"),
            ));
        }
    }

    for (name, record, record_path) in read_records(paths)? {
        let entry = paths.library.join(&name);
        if entry.is_dir() && !entry.is_symlink() {
            match committed_tree_hash(&paths.library, &name, &entry, &[]) {
                Ok(hash) if hash != record.imported_hash => findings.push(finding_with_hint(
                    "local_edit",
                    &entry,
                    format!(
                        "{name} differs from its import record, so `skillherd reconcile` will refuse to overwrite or delete it"
                    ),
                    format!(
                        "`skillherd reconcile --adopt {name}` takes upstream, `--keep {name}` keeps the house copy and stops tracking it"
                    ),
                )),
                Ok(_) => {}
                Err(error) => output::warn(
                    "npx_entry_unhashed",
                    format!("could not hash {}: {error}", entry.display()),
                ),
            }
        }
        if record.variants_pending {
            findings.push(finding_with_hint(
                "variant_pending",
                &record_path,
                format!(
                    "{name} was imported without its upstream variants; the next `skillherd reconcile` retries them"
                ),
                "`skillherd reconcile` retries every pending variant whenever the network is up".to_string(),
            ));
        }
        for (key, recorded) in &record.variants {
            let variant = entry.join(".variants").join(key);
            match committed_tree_hash(
                &paths.library,
                &format!("{name}/.variants/{key}"),
                &variant,
                &[],
            ) {
                Ok(hash) if &hash != recorded => findings.push(finding_with_hint(
                    "variant_drift",
                    &variant,
                    format!(
                        "{name}: `.variants/{key}` was edited after import (recorded {}, found {})",
                        short(recorded),
                        short(&hash)
                    ),
                    format!(
                        "`skillherd reconcile` replaces the variant from upstream unless it was edited by hand; `--adopt {name}` takes upstream again"
                    ),
                )),
                Ok(_) => {}
                Err(_) => findings.push(finding_with_hint(
                    "variant_drift",
                    &variant,
                    format!("{name}: `.variants/{key}` is recorded in `.npx/{name}.json` but missing from the pool"),
                    format!("`skillherd reconcile` re-imports the variant from upstream for {name}"),
                )),
            }
        }
        for (harness, recorded) in &record.harness_agents {
            let stored = agents::stored_agents(paths, &name, harness);
            let hint = format!(
                "`skillherd update {name}` re-imports the agents unless they were edited by hand; `skillherd remove {name} --force` discards an edited copy"
            );
            match committed_tree_hash(
                &paths.library,
                &format!("{name}/{}/{harness}", agents::AGENTS_DIR),
                &stored,
                &[],
            ) {
                Ok(hash) if &hash != recorded => findings.push(finding_with_hint(
                    "agent_drift",
                    &stored,
                    format!(
                        "{name}: `{}/{harness}` was edited after import (recorded {}, found {})",
                        agents::AGENTS_DIR,
                        short(recorded),
                        short(&hash)
                    ),
                    hint,
                )),
                Ok(_) => {}
                Err(_) => findings.push(finding_with_hint(
                    "agent_drift",
                    &stored,
                    format!(
                        "{name}: `{}/{harness}` is recorded in `.npx/{name}.json` but missing from the pool",
                        agents::AGENTS_DIR
                    ),
                    hint,
                )),
            }
        }
    }
    Ok(())
}

/// Every real pool entry is tracked upstream (`.npx/<name>.json`) or declared
/// house (`.globals/house`); a symlinked entry's provenance is its target.
fn scan_provenance(paths: &Paths, findings: &mut Vec<Finding>) -> Result<()> {
    let house = crate::provenance::load_house(paths)?;
    let recorded: BTreeSet<String> = read_records(paths)?
        .into_iter()
        .map(|(name, _, _)| name)
        .collect();
    let lock = match crate::npx::read_lock(&paths.lock) {
        Ok(lock) => lock,
        Err(error) => {
            output::warn(
                "npx_lock_unreadable",
                format!("provenance check read no live lock: {error}"),
            );
            BTreeMap::new()
        }
    };
    let house_file = crate::provenance::house_path(paths);

    for name in subdir_names(&paths.library)? {
        if validate_name(&name).is_err() || house.contains(&name) || recorded.contains(&name) {
            continue;
        }
        let entry = paths.library.join(&name);
        let Ok(meta) = fs::symlink_metadata(&entry) else {
            continue;
        };
        if !meta.is_dir() || !entry.join("SKILL.md").is_file() {
            continue;
        }
        match lock.get(&name) {
            Some(locked) => findings.push(finding_with_hint(
                "untracked_upstream",
                &entry,
                format!(
                    "{name} came from {} (the live npx lock says so) but has no .npx/{name}.json import record, so `skillherd update` refuses it and it never gets upstream fixes",
                    locked.source
                ),
                format!(
                    "`skillherd install {source} --skill {name} --adopt {name}` takes upstream and tracks it; the same command with `--keep {name}` declares the pool copy house",
                    source = locked.source
                ),
            )),
            None => findings.push(finding_with_hint(
                "unknown_provenance",
                &entry,
                format!(
                    "{name} has no import record, no npx lock entry, and no line in {}; nobody knows whether it has an upstream",
                    house_file.display()
                ),
                format!(
                    "`skillherd install <owner/repo> --skill {name} --adopt {name}` tracks its upstream; if it was written here, add `{name}` to {}",
                    house_file.display()
                ),
            )),
        }
    }

    for name in &house {
        if !library_entry_exists(paths, name) {
            findings.push(finding(
                "house_name_missing",
                &house_file,
                format!(
                    "{} declares {name} house, but the pool has no {name} entry",
                    house_file.display()
                ),
            ));
        }
    }
    Ok(())
}

fn short(hash: &str) -> String {
    hash.chars().take(8).collect()
}

fn read_records(paths: &Paths) -> Result<Vec<(String, NpxMeta, PathBuf)>> {
    let dir = paths.library.join(".npx");
    let mut records = Vec::new();
    for name in subdir_names(&dir)? {
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        // `skill-lock.<platform>.json` is the npx lock snapshot, never a record.
        if stem.starts_with("skill-lock") || validate_name(stem).is_err() {
            continue;
        }
        let path = dir.join(&name);
        if !path.is_file() {
            continue;
        }
        let text = fs::read_to_string(&path)
            .map_err(|err| Error::io(&format!("reading {}", path.display()), &err))?;
        match serde_json::from_str::<NpxMeta>(&text) {
            Ok(record) => records.push((stem.to_string(), record, path)),
            Err(error) => output::warn(
                "npx_record_unreadable",
                format!("{} is not a usable import record: {error}", path.display()),
            ),
        }
    }
    records.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(records)
}

// ---- fix --------------------------------------------------------------

fn apply_fixes(paths: &Paths, report: &mut DoctorReport, dry_run: bool) {
    let fixable: Vec<Finding> = report
        .findings
        .iter()
        .filter(|finding| finding.fixable)
        .cloned()
        .collect();
    if fixable.is_empty() {
        return;
    }

    let park = paths
        .home
        .join(PARK_DIR)
        .join(format!("doctor-{}", now_iso8601()));
    let outside: Vec<&Finding> = fixable
        .iter()
        .filter(|finding| !inside_home(paths, &finding.path) || !inside_home(paths, &park))
        .collect();
    if !outside.is_empty() {
        report.refused = Some(format!(
            "{} fix path(s) lie outside HOME ({}); nothing was moved",
            outside.len(),
            paths.home.display()
        ));
        for finding in outside {
            report.failed.push(Finding {
                code: REFUSED_CODE.to_string(),
                severity: Severity::Error,
                path: finding.path.clone(),
                message: format!(
                    "refusing to fix `{}` at {}: it is not lexically inside HOME ({})",
                    finding.code,
                    finding.path.display(),
                    paths.home.display()
                ),
                fixable: false,
                suggested_fix: Some(format!(
                    "move the surface under {} (or fix that path by hand), then rerun `skillherd doctor`",
                    paths.home.display()
                )),
            });
        }
        return;
    }

    let mut plan = Vec::new();
    for finding in &fixable {
        if moves_entry(finding) {
            plan.push(PlanOp {
                op: "remove".into(),
                path: finding.path.clone(),
                target: Some(
                    park.join(surface_name(paths, &finding.path))
                        .join(entry_name(&finding.path)),
                ),
            });
        } else if finding.code == "surface_drift" {
            plan.push(PlanOp {
                op: "refresh".into(),
                path: finding.path.clone(),
                target: None,
            });
        }
    }
    if dry_run {
        report.plan = Some(plan);
        return;
    }

    let mut quarantined = false;
    for finding in &fixable {
        if !moves_entry(finding) {
            continue;
        }
        let target = match parkable(paths, finding) {
            Ok(Parkable::No(reason)) => {
                report.failed.push(fix_failure(finding, &reason));
                continue;
            }
            Ok(Parkable::Yes { target }) => target,
            Err(error) => {
                report.failed.push(fix_failure(finding, &error.to_string()));
                continue;
            }
        };
        let root = park.join(surface_name(paths, &finding.path));
        match park_link(&finding.path, &target, &root, &entry_name(&finding.path)) {
            Ok(_parked) => {
                quarantined = true;
                report.fixed.push(finding.clone());
            }
            Err(reason) => report.failed.push(fix_failure(finding, &reason)),
        }
    }
    if quarantined {
        report.quarantine = Some(park.clone());
    }

    let drift: Vec<Finding> = fixable
        .iter()
        .filter(|finding| finding.code == "surface_drift")
        .cloned()
        .collect();
    // Render removes a stale managed entry outright; it never parks. When a
    // move above failed, the entry it could not park is still on the surface,
    // so rendering now would delete exactly what `--fix` promised to keep.
    if !drift.is_empty() && !report.failed.is_empty() {
        for finding in drift {
            report.failed.push(fix_failure(
                &finding,
                "not rendered: an earlier fix failed, and render would delete what could not be parked",
            ));
        }
    } else if !drift.is_empty() {
        let opts = RenderOpts {
            surfaces: Vec::new(),
            dry_run: false,
            only: None,
        };
        match render::run(paths, opts) {
            Ok(_) => report.fixed.extend(drift),
            Err(error) => {
                for finding in drift {
                    report
                        .failed
                        .push(fix_failure(&finding, &error.to_string()));
                }
            }
        }
    }
}

fn moves_entry(finding: &Finding) -> bool {
    finding.code == "dangling_link"
}

/// Whether a fix may park the entry a finding names.
enum Parkable {
    /// The path still holds the managed link the scan named, resolving to this
    /// target; the park re-proves the same target at its mutation boundary.
    Yes { target: PathBuf },
    /// It does not, for this reason: refuse and report `failed`.
    No(String),
}

/// Re-prove one move at the moment of the move.
///
/// The scan's finding is a claim about the moment it was made: the fix parks a
/// path only while it still holds a managed link carrying the defect the finding
/// named. A real directory — a user's, or a rendered copy — that took the path
/// since is refused, so `--fix` can never move something it did not plan for.
///
/// The identity re-proved here is the defect class, not the exact link text
/// (coordinator ruling, fix round 2): a managed link that was swapped for
/// another managed link with the same defect between the scan and the fix is
/// parked too, which is the fix that link needs as well, and parking is a move
/// under `~/.agents/skill-park/` that a human can reverse. Nothing that is not a
/// managed symlink is ever moved.
fn parkable(paths: &Paths, finding: &Finding) -> Result<Parkable> {
    let Some(entry) = managed::entry_at(&finding.path, paths)? else {
        return Ok(Parkable::No(format!("{} is gone", finding.path.display())));
    };
    match (&entry.kind, entry.managed) {
        (EntryKind::Symlink { target, dangling }, true) => {
            let still_named = finding.code == "dangling_link" && *dangling;
            if still_named {
                Ok(Parkable::Yes {
                    target: target.clone(),
                })
            } else {
                Ok(Parkable::No(format!(
                    "{} no longer carries what the scan named; nothing was moved",
                    finding.path.display()
                )))
            }
        }
        _ => Ok(Parkable::No(format!(
            "{} is {} now, not the managed link the scan named; nothing was moved and nothing foreign was touched",
            finding.path.display(),
            managed::describe_entry(&entry)
        ))),
    }
}

fn fix_failure(finding: &Finding, reason: &str) -> Finding {
    Finding {
        code: finding.code.clone(),
        severity: Severity::Error,
        path: finding.path.clone(),
        message: format!("{} could not be fixed: {reason}", finding.code),
        fixable: finding.fixable,
        suggested_fix: finding.suggested_fix.clone(),
    }
}

fn inside_home(paths: &Paths, path: &Path) -> bool {
    let home = normalize(&paths.home);
    let candidate = lexical_abs(&home, path);
    candidate.starts_with(&home)
}

/// The harness whose surface holds `path`; `unknown` for a path outside them.
fn surface_name(paths: &Paths, path: &Path) -> String {
    paths
        .surfaces()
        .into_iter()
        .find(|(_, surface)| path.starts_with(surface))
        .map(|(name, _)| name.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn entry_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "entry".to_string())
}

/// Park a managed link: a copy of it is created under `root` first, then the
/// original is unlinked, after the link has been read again at that boundary
/// and still says what the scan proved.
///
/// The copy is made with `symlink(2)`, which never replaces anything (a taken
/// name gets the next `<name>.<n>`), and the original goes with `unlink(2)`,
/// which cannot remove a directory: a real directory that takes the path
/// between the re-proof and the unlink is an error, never a loss. What remains
/// is the gap in which one managed link is swapped for another between the
/// last read and the unlink; the parked copy then holds the earlier text.
/// POSIX has no "unlink only if the link text is X", which is why the copy
/// comes first and the original is read twice.
fn park_link(
    source: &Path,
    expected: &Path,
    root: &Path,
    name: &str,
) -> std::result::Result<PathBuf, String> {
    fs::create_dir_all(root).map_err(|err| format!("creating {}: {err}", root.display()))?;
    let base = source.parent().unwrap_or_else(|| Path::new("."));
    let read = || -> std::result::Result<PathBuf, String> {
        let meta = fs::symlink_metadata(source)
            .map_err(|err| format!("reading {}: {err}", source.display()))?;
        if !meta.file_type().is_symlink() {
            return Err(format!(
                "{} is not a symlink any more; nothing was moved",
                source.display()
            ));
        }
        let text =
            fs::read_link(source).map_err(|err| format!("reading {}: {err}", source.display()))?;
        if normalize(&lexical_abs(base, &text)) != normalize(expected) {
            return Err(format!(
                "{} now links to {} instead of the {} the scan named; nothing was moved",
                source.display(),
                text.display(),
                expected.display()
            ));
        }
        Ok(text)
    };
    let text = read()?;

    let mut parked = root.join(name);
    let mut index = 1;
    loop {
        match std::os::unix::fs::symlink(&text, &parked) {
            Ok(()) => break,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                parked = root.join(format!("{name}.{index}"));
                index += 1;
            }
            Err(err) => return Err(format!("parking it at {}: {err}", parked.display())),
        }
    }

    let boundary = read().and_then(|again| {
        if again == text {
            Ok(())
        } else {
            Err(format!(
                "{} changed while it was being parked; nothing was moved",
                source.display()
            ))
        }
    });
    if let Err(reason) = boundary {
        let _ = fs::remove_file(&parked);
        return Err(reason);
    }
    match fs::remove_file(source) {
        Ok(()) => Ok(parked),
        Err(err) => {
            let _ = fs::remove_file(&parked);
            Err(format!("unlinking {}: {err}", source.display()))
        }
    }
}

// ---- textual TOML helpers --------------------------------------------

/// Drop an unquoted `#` comment (TOML comments start at a `#` outside quotes).
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    for (index, character) in line.char_indices() {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => {}
            None if character == '"' || character == '\'' => quote = Some(character),
            None if character == '#' => return &line[..index],
            None => {}
        }
    }
    line
}

/// `key = value` inside a table body, when the line is that key.
fn table_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (name, value) = line.split_once('=')?;
    (name.trim() == key).then_some(value.trim())
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    let trimmed = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        });
    match trimmed {
        Some(inner) => inner.to_string(),
        None => value.to_string(),
    }
}

fn expand_home(home: &Path, value: &str) -> PathBuf {
    match value.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(value),
    }
}

// ---- tests -----------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::tree_hash;
    use crate::model::{HarnessRow, SetRow};
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Paths) {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        fs::create_dir_all(&paths.library).unwrap();
        (dir, paths)
    }

    fn set_row(name: &str, main: bool, lane: bool, line: usize) -> SetRow {
        SetRow {
            name: name.to_string(),
            main,
            lane,
            not_platform: Vec::new(),
            only: Vec::new(),
            not: Vec::new(),
            line,
        }
    }

    fn with_rows(paths: &Paths, rows: Vec<SetRow>) -> Policy {
        let mut policy = policy::default_policy(paths);
        policy.rows = rows;
        policy
    }

    #[test]
    fn park_link_copies_then_unlinks_and_refuses_a_link_that_changed() {
        let (dir, _paths) = fixture();
        let surface = dir.path().join("surface");
        fs::create_dir_all(&surface).unwrap();
        let root = dir.path().join("park");
        let link = surface.join("ghost");
        std::os::unix::fs::symlink("../lib/ghost", &link).unwrap();
        let scanned = lexical_abs(&surface, Path::new("../lib/ghost"));

        // A link that no longer says what the scan named is refused at the
        // boundary and nothing is created or removed.
        let other = lexical_abs(&surface, Path::new("../lib/other"));
        let refused = park_link(&link, &other, &root, "ghost").unwrap_err();
        assert!(refused.contains("nothing was moved"), "{refused}");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!root.join("ghost").exists() && !fs::symlink_metadata(root.join("ghost")).is_ok());

        // The one the scan named is parked as a copy with the same text, and
        // a second park of the same name lands beside it, never over it.
        let parked = park_link(&link, &scanned, &root, "ghost").unwrap();
        assert_eq!(parked, root.join("ghost"));
        assert_eq!(fs::read_link(&parked).unwrap(), Path::new("../lib/ghost"));
        assert!(fs::symlink_metadata(&link).is_err(), "the original is gone");
        std::os::unix::fs::symlink("../lib/ghost", &link).unwrap();
        let again = park_link(&link, &scanned, &root, "ghost").unwrap();
        assert_eq!(again, root.join("ghost.1"));
        assert_eq!(fs::read_link(&parked).unwrap(), Path::new("../lib/ghost"));

        // A real directory is never parked by this path at all.
        let real = surface.join("mine");
        fs::create_dir_all(&real).unwrap();
        let refused = park_link(&real, &scanned, &root, "mine").unwrap_err();
        assert!(refused.contains("not a symlink"), "{refused}");
        assert!(real.is_dir());
    }

    #[test]
    fn every_published_code_has_a_row() {
        assert_eq!(FINDING_CODES.len(), 20);
        let capabilities = capabilities();
        assert_eq!(capabilities.codes.len(), FINDING_CODES.len());
        assert_eq!(capabilities.exit_codes.len(), 5);
        assert_eq!(capabilities.exit_codes[0].code, 0);
        assert_eq!(capabilities.exit_codes[4].code, 4);
        assert!(
            capabilities
                .codes
                .iter()
                .any(|row| row.code == "local_edit")
        );
        let fixable: Vec<&str> = capabilities
            .codes
            .iter()
            .filter(|row| row.fixable)
            .map(|row| row.code.as_str())
            .collect();
        assert_eq!(fixable, ["dangling_link", "surface_drift"]);
        assert_eq!(capabilities.refusal_code, REFUSED_CODE);
    }

    #[test]
    fn a_drift_plan_becomes_one_fixable_finding_per_op() {
        let plan = vec![
            PlanOp {
                op: "link".into(),
                path: PathBuf::from("/sandbox/user/.claude-custom/skills/post"),
                target: Some(PathBuf::from("/sandbox/user/.agents/skill-library/post")),
            },
            PlanOp {
                op: "unlink".into(),
                path: PathBuf::from("/sandbox/user/.agents/skills/old"),
                target: None,
            },
        ];
        let findings = drift_findings(&plan);
        assert_eq!(findings.len(), 2);
        for (finding, op) in findings.iter().zip(&plan) {
            assert_eq!(finding.code, "surface_drift");
            assert_eq!(finding.severity, Severity::Warn);
            assert!(finding.fixable);
            assert_eq!(&finding.path, &op.path);
            assert!(finding.suggested_fix.as_deref() == Some("`skillherd render`"));
        }
        assert!(findings[0].message.contains("link"));
        assert!(drift_findings(&[]).is_empty());
    }

    #[test]
    fn a_render_that_cannot_answer_never_invents_a_finding() {
        let (_dir, paths) = fixture();
        output::clear_warnings();
        let mut findings = Vec::new();
        scan_render_drift(&paths, &mut findings);
        for finding in &findings {
            assert_eq!(finding.code, "surface_drift");
        }
        output::clear_warnings();
    }

    #[test]
    fn a_sets_row_without_a_pool_entry_is_reported() {
        let (_dir, paths) = fixture();
        fs::create_dir_all(paths.library.join("post")).unwrap();
        let policy = with_rows(
            &paths,
            vec![
                set_row("post", true, false, 4),
                set_row("ghost", true, false, 5),
            ],
        );

        let mut findings = Vec::new();
        scan_policy_names(&paths, &policy, &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "policy_name_missing");
        assert_eq!(findings[0].severity, Severity::Warn);
        assert!(!findings[0].fixable);
        assert_eq!(findings[0].path, paths.library.join("ghost"));
        assert!(findings[0].message.contains("line 5"));
        assert!(
            findings[0]
                .suggested_fix
                .as_deref()
                .is_some_and(|hint| hint.contains("skillherd off ghost"))
        );
    }

    /// A row that excludes this machine's platform asks for nothing here: its
    /// skill missing from this pool is not reported (a Mac-only skill on a
    /// Linux machine). The same row on the platform it applies to still is.
    #[test]
    fn a_missing_name_on_a_row_excluding_this_platform_is_not_reported() {
        use crate::model::Platform;
        let (_dir, mut paths) = fixture();
        let mut row = set_row("mac-tool", true, false, 3);
        row.not_platform = vec![Platform::Linux];
        let policy = with_rows(&paths, vec![row]);

        paths.platform = Platform::Linux;
        let mut findings = Vec::new();
        scan_policy_names(&paths, &policy, &mut findings);
        assert!(findings.is_empty(), "{findings:?}");

        paths.platform = Platform::Mac;
        scan_policy_names(&paths, &policy, &mut findings);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].code, "policy_name_missing");
    }

    #[test]
    fn a_none_tier_harness_that_reads_a_shared_root_is_reported() {
        let (_dir, paths) = fixture();
        let clean = with_rows(&paths, Vec::new());
        let mut findings = Vec::new();
        scan_visibility(&paths, &clean, &mut findings).unwrap();
        assert!(findings.is_empty(), "no rows means nothing is visible");

        // Grok's own surface serves nothing, but it also reads the shared root.
        let policy = with_rows(&paths, vec![set_row("grilling", false, true, 2)]);
        let mut findings = Vec::new();
        scan_visibility(&paths, &policy, &mut findings).unwrap();
        let excess: Vec<&Finding> = findings
            .iter()
            .filter(|finding| finding.code == "visibility_excess")
            .collect();
        assert_eq!(excess.len(), 2, "grok and cursor both read the shared root");
        for finding in excess {
            assert_eq!(finding.severity, Severity::Info);
            assert!(!finding.fixable);
            assert!(finding.message.contains("sees 1 skill"));
            assert!(finding.path.starts_with(&paths.home));
        }
        // A main-tier harness with the same read root is by design, not excess.
        assert!(findings.iter().all(|finding| {
            !finding.message.contains("codex") || finding.code != "visibility_excess"
        }));
    }

    #[test]
    fn foreign_entries_shadow_a_desired_name() {
        let (_dir, paths) = fixture();
        fs::create_dir_all(paths.library.join("post")).unwrap();
        let surface = paths.claude_shared.join("skills");
        fs::create_dir_all(surface.join("post")).unwrap();
        let policy = with_rows(&paths, vec![set_row("post", true, false, 2)]);

        let mut findings = Vec::new();
        scan_surfaces(&paths, &policy, &mut findings).unwrap();
        let codes: Vec<&str> = findings
            .iter()
            .map(|finding| finding.code.as_str())
            .collect();
        assert!(codes.contains(&"foreign_entry"), "{codes:?}");
        assert!(codes.contains(&"foreign_shadow"), "{codes:?}");

        let mut clean = Vec::new();
        scan_surfaces(&paths, &policy::default_policy(&paths), &mut clean).unwrap();
        assert!(
            clean.iter().all(|finding| finding.code == "foreign_entry"),
            "with no rows nothing is desired, so nothing is shadowed"
        );
    }

    #[test]
    fn codex_disables_are_read_out_of_the_toml_text() {
        let text = "\
[other]
enabled = false

[[skills.config]]
path = \"/sandbox/user/.agents/skills/foo\"
enabled = false

[[skills.config]]
path = \"/sandbox/user/.agents/skills/bar\"   # keep this one
enabled = true

[[skills.config]]
path = \"/tmp/not-ours\"
enabled = false
";
        let entries = codex_entries(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "/sandbox/user/.agents/skills/foo");
        assert_eq!(entries[0].line, 4, "the table header line");
        assert_eq!(entries[1].line, 12);

        let (_dir, paths) = fixture();
        fs::create_dir_all(paths.codex_home.join("skills")).unwrap();
        fs::write(
            paths.codex_home.join("config.toml"),
            text.replace("/sandbox/user", &paths.home.display().to_string()),
        )
        .unwrap();
        let mut findings = Vec::new();
        scan_codex_config(&paths, &mut findings).unwrap();
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].code, "codex_disable_entry");
        assert_eq!(findings[0].path, paths.codex_home.join("config.toml"));
        assert!(!findings[0].fixable);
    }

    #[test]
    fn the_grok_compat_knobs_default_to_on() {
        let off =
            grok_compat_state("[compat.claude]\nskills = false\n[compat.codex]\nskills = false\n");
        assert_eq!(off["claude"], Some(false));
        assert_eq!(off["codex"], Some(false));

        let on = grok_compat_state("[compat.claude]\nskills = true\n");
        assert_eq!(on["claude"], Some(true));
        assert_eq!(on["codex"], None, "a missing section means on");

        let (_dir, paths) = fixture();
        fs::create_dir_all(&paths.grok_home).unwrap();
        let mut findings = Vec::new();
        scan_grok_config(&paths, &mut findings).unwrap();
        assert_eq!(findings.len(), 2, "a missing config means both are on");
        assert!(
            findings
                .iter()
                .all(|finding| finding.code == "grok_compat_skills_on")
        );

        fs::write(
            paths.grok_home.join("config.toml"),
            "[compat.claude]\nskills = false\n[compat.codex]\nskills = false\n",
        )
        .unwrap();
        let mut clean = Vec::new();
        scan_grok_config(&paths, &mut clean).unwrap();
        assert!(clean.is_empty());

        // A machine without Grok installed has nothing to report.
        fs::remove_dir_all(&paths.grok_home).unwrap();
        let mut absent = Vec::new();
        scan_grok_config(&paths, &mut absent).unwrap();
        assert!(absent.is_empty());
    }

    #[test]
    fn read_records_skips_every_platform_lock_snapshot() {
        let (_dir, paths) = fixture();
        let npx = paths.library.join(".npx");
        fs::create_dir_all(&npx).unwrap();
        // The lock snapshot has the `skills` CLI's own shape, not a record's;
        // snapshots have a platform suffix.
        let lock = r#"{"version":1,"skills":{},"lastSelectedAgents":[],"dismissed":{}}"#;
        for name in [
            "skill-lock.json",
            "skill-lock.mac.json",
            "skill-lock.linux.json",
        ] {
            fs::write(npx.join(name), lock).unwrap();
        }
        let record = NpxMeta {
            source: "owner/repo".into(),
            source_type: "github".into(),
            source_url: "https://github.com/owner/repo.git".into(),
            r#ref: None,
            skill_path: "skills/post/SKILL.md".into(),
            skill_folder_hash: "abc".into(),
            imported_hash: "def".into(),
            imported_at: "2026-09-22T00:00:00Z".into(),
            revision: "0123".into(),
            variants: BTreeMap::new(),
            variants_pending: false,
            harness_agents: BTreeMap::new(),
            release: None,
        };
        fs::write(
            npx.join("post.json"),
            serde_json::to_string(&record).unwrap(),
        )
        .unwrap();

        output::clear_warnings();
        let records = read_records(&paths).unwrap();
        let names: Vec<&str> = records.iter().map(|(name, _, _)| name.as_str()).collect();
        assert_eq!(names, ["post"]);
        assert!(
            output::warnings().is_empty(),
            "a lock snapshot is never a record: {:?}",
            output::warnings()
        );
    }

    #[test]
    fn a_served_surface_on_the_project_walk_is_not_a_project_install() {
        let (_dir, mut paths) = fixture();
        paths.claude_shared = paths.home.join("custom-claude");
        // A linked custom Claude surface may put a rendered fenced copy
        // (a real directory the renderer owns) on the project walk.
        let served = paths.claude_shared.join("skills");
        fs::create_dir_all(served.join("done")).unwrap();
        fs::write(served.join("done/SKILL.md"), "# done\n").unwrap();
        fs::create_dir_all(paths.home.join(".claude")).unwrap();
        std::os::unix::fs::symlink(&served, paths.home.join(".claude/skills")).unwrap();
        // Codex reads its surface straight from ~/.codex/skills.
        fs::create_dir_all(paths.codex_home.join("skills/herdr")).unwrap();
        fs::write(paths.codex_home.join("skills/herdr/SKILL.md"), "# herdr\n").unwrap();
        fs::create_dir_all(paths.home.join(".git")).unwrap();

        let mut findings = Vec::new();
        scan_project_install_from(&paths, &paths.home, &mut findings).unwrap();
        assert!(findings.is_empty(), "served copies reported: {findings:?}");

        // A real npx install inside a project still reports.
        let project = paths.home.join("project");
        fs::create_dir_all(project.join(".claude/skills/foo")).unwrap();
        fs::write(project.join(".claude/skills/foo/SKILL.md"), "# foo\n").unwrap();
        fs::create_dir_all(project.join(".git")).unwrap();
        let mut findings = Vec::new();
        scan_project_install_from(&paths, &project, &mut findings).unwrap();
        let codes: Vec<&str> = findings
            .iter()
            .map(|finding| finding.code.as_str())
            .collect();
        assert_eq!(codes, ["project_npx_install"]);
    }

    #[test]
    fn the_exit_dictionary_follows_the_report() {
        let warn = Finding {
            code: "lock_drift".into(),
            severity: Severity::Warn,
            path: PathBuf::from("/sandbox/user/.agents/.skill-lock.json"),
            message: "stale".into(),
            fixable: false,
            suggested_fix: None,
        };
        let info = Finding {
            code: "local_edit".into(),
            severity: Severity::Info,
            path: PathBuf::from("/sandbox/user/.agents/skill-library/post"),
            message: "edited".into(),
            fixable: false,
            suggested_fix: None,
        };
        let base = |findings: Vec<Finding>| DoctorReport {
            findings,
            fixed: Vec::new(),
            failed: Vec::new(),
            quarantine: None,
            plan: None,
            refused: None,
        };

        assert_eq!(exit_code(&base(Vec::new())), 0);
        assert_eq!(
            exit_code(&base(vec![info.clone()])),
            0,
            "info does not fail a run"
        );
        assert_eq!(exit_code(&base(vec![warn.clone()])), EXIT_DOCTOR_FINDINGS);

        let mut fixed = base(vec![warn.clone()]);
        fixed.fixed.push(warn.clone());
        assert_eq!(exit_code(&fixed), 0, "a fixed warn leaves nothing behind");

        let mut partial = base(vec![warn.clone()]);
        partial.fixed.push(warn.clone());
        partial.failed.push(Finding {
            severity: Severity::Error,
            ..warn.clone()
        });
        assert_eq!(exit_code(&partial), EXIT_DOCTOR_PARTIAL_FIX);

        let mut all_failed = base(vec![warn.clone()]);
        all_failed.failed.push(warn.clone());
        assert_eq!(exit_code(&all_failed), EXIT_DOCTOR_FIX_FAILED);

        let mut refused = base(vec![warn]);
        refused.refused = Some("outside HOME".into());
        assert_eq!(exit_code(&refused), EXIT_DOCTOR_REFUSED);
    }

    #[test]
    fn a_fix_outside_home_is_refused_whole() {
        let (_dir, paths) = fixture();
        let elsewhere = TempDir::new().unwrap();
        let elsewhere = elsewhere.path();
        // The park root and the entry must both sit inside HOME.
        assert!(inside_home(
            &paths,
            &paths.home.join(".agents/skill-park/x")
        ));
        assert!(!inside_home(&paths, &elsewhere.join("entry")));
        assert!(inside_home(
            &paths,
            &paths.claude_shared.join("skills/post")
        ));
    }

    /// `--fix` re-proves every move at the moment of the move: a path that is no
    /// longer the managed link the scan named is refused, and a real directory
    /// that took it is left untouched.
    #[test]
    fn a_fix_refuses_a_path_that_is_no_longer_the_scanned_link() {
        let (_dir, paths) = fixture();
        let surface = paths.claude_shared.join("skills");
        fs::create_dir_all(&surface).unwrap();
        let link = surface.join("ghost");
        // What the scan sees: a dangling managed link.
        std::os::unix::fs::symlink("../../.agents/skill-library/ghost", &link).unwrap();

        let policy = policy::default_policy(&paths);
        let mut findings = Vec::new();
        scan_surfaces(&paths, &policy, &mut findings).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.code == "dangling_link"),
            "{findings:?}"
        );

        // A real user directory takes the path before the fix runs.
        fs::remove_file(&link).unwrap();
        fs::create_dir_all(&link).unwrap();
        fs::write(link.join("SKILL.md"), "# mine\n").unwrap();
        let before = tree_hash(&link).unwrap();

        let mut report = DoctorReport {
            findings,
            fixed: Vec::new(),
            failed: Vec::new(),
            quarantine: None,
            plan: None,
            refused: None,
        };
        apply_fixes(&paths, &mut report, false);

        assert!(report.fixed.is_empty(), "{report:?}");
        assert!(
            report.quarantine.is_none(),
            "nothing was parked: {:?}",
            report.quarantine
        );
        let failure = report
            .failed
            .iter()
            .find(|finding| finding.path == link)
            .unwrap_or_else(|| panic!("the refusal is reported: {report:?}"));
        assert!(
            failure
                .message
                .contains("not the managed link the scan named"),
            "{failure:?}"
        );
        assert_eq!(
            tree_hash(&link).unwrap(),
            before,
            "the directory is untouched"
        );
        assert_eq!(
            fs::read_to_string(link.join("SKILL.md")).unwrap(),
            "# mine\n"
        );
    }

    #[test]
    fn harness_rows_are_the_default_table() {
        let (_dir, paths) = fixture();
        let policy = policy::default_policy(&paths);
        assert!(
            policy
                .harnesses
                .iter()
                .any(|row: &HarnessRow| row.name == "agents")
        );
        assert_eq!(
            policy
                .harnesses
                .iter()
                .filter(|row| row.tier == Tier::None)
                .count(),
            2
        );
    }
}
