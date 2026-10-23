//! Doctor, project activation and sync: black-box tests over a fake HOME.
//!
//! Every finding code gets a fixture that triggers exactly it and a fixture
//! that does not. Two families cannot be triggered through the binary while a
//! sibling lane owns their source of truth: the policy-dependent codes
//! (`policy_name_missing`, `visibility_excess`, and the desired-set half of
//! `foreign_shadow`) are unit-tested in `src/doctor.rs` against a constructed
//! policy, and `surface_drift` needs a real renderer (its plan-to-finding
//! mapping and the unavailable-renderer path are unit-tested too).

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

use skillherd::hash::tree_hash;
use skillherd::model::NpxMeta;

/// The root and global environment variables the binary reads.
const OVERRIDES: [&str; 26] = [
    "SKILLHERD_LIBRARY",
    "SKILLHERD_AGENTS_DIR",
    "SKILLHERD_LOCK",
    "SKILLHERD_STAGE",
    "CLAUDE_SHARED",
    "CODEX_HOME",
    "GROK_HOME",
    "SKILLHERD_PLATFORM",
    "SKILLHERD_NPX",
    "SKILLHERD_NPX_PKG",
    "SKILLHERD_SNAPSHOT_LOCK",
    "SKILLHERD_RECONCILE_FAILPOINT",
    "SKILLHERD_NPX_CREDENTIALS",
    "SKILLHERD_NPX_TELEMETRY",
    "SKILLHERD_BIN",
    "SKILLHERD_REAL_NPX",
    "CLAUDE_CONFIG_DIR",
    "XDG_STATE_HOME",
    "SKILLHERD_CONFIG",
    "SKILLHERD_POOL_REMOTE",
    "SKILLHERD_SYNC_COMMAND",
    "SKILLHERD_CURSOR_SKILLS",
    "SKILLHERD_PI_SKILLS",
    "SKILLHERD_GH",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
];

struct Sandbox {
    home: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        // The stock Claude layout: the Claude root is `~/.claude`.
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        Self { home }
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::cargo_bin("skillherd").unwrap();
        cmd.env("HOME", self.home.path());
        for key in OVERRIDES {
            cmd.env_remove(key);
        }
        // Pinned, so no assertion depends on the host: the platform picks the
        // lock snapshot (`skill-lock.mac.json`) and the fence blocks kept, and
        // the Claude root is named rather than detected.
        cmd.env("SKILLHERD_PLATFORM", "mac")
            .env("CLAUDE_SHARED", self.home.path().join(".claude"));
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().unwrap()
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        self.cmd().current_dir(dir).args(args).output().unwrap()
    }

    fn library(&self) -> PathBuf {
        self.home.path().join(".agents/skill-library")
    }

    fn agents_dir(&self) -> PathBuf {
        self.home.path().join(".agents/skills")
    }

    fn claude_surface(&self) -> PathBuf {
        self.home.path().join(".claude/skills")
    }

    fn codex_surface(&self) -> PathBuf {
        self.home.path().join(".codex/skills")
    }

    fn park_root(&self) -> PathBuf {
        self.home.path().join(".agents/skill-park")
    }

    /// A pool entry with a `SKILL.md`, declared house so its provenance is
    /// known (every pool entry must be tracked upstream or declared house).
    fn pool_skill(&self, name: &str) -> PathBuf {
        let entry = self.untracked_skill(name);
        self.declare_house(name);
        entry
    }

    /// A pool entry with a `SKILL.md` and no recorded provenance.
    fn untracked_skill(&self, name: &str) -> PathBuf {
        let entry = self.library().join(name);
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join("SKILL.md"), format!("# {name}\n")).unwrap();
        entry
    }

    /// Append a name to `.globals/house`.
    fn declare_house(&self, name: &str) {
        let path = self.library().join(".globals/house");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = fs::read_to_string(&path).unwrap_or_default();
        text.push_str(&format!("{name}  # fixture\n"));
        fs::write(&path, text).unwrap();
    }

    /// An import record for a pool entry.
    fn npx_record(&self, name: &str, record: &NpxMeta) {
        let dir = self.library().join(".npx");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_string_pretty(record).unwrap(),
        )
        .unwrap();
    }

    /// A minimal policy file, so a fixture reads the same before and after the
    /// policy lane lands (`load` must not have to invent a missing file).
    fn empty_policy(&self) {
        let dir = self.library().join(".globals");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("sets"), "# no always-on rows in this fixture\n").unwrap();
    }

    /// The project a project-scope test acts on: a git root under the sandbox.
    fn project(&self) -> PathBuf {
        let project = self.home.path().join("work/proj");
        fs::create_dir_all(project.join(".git")).unwrap();
        project
    }
}

fn envelope(output: &Output, argv: &[&str]) -> Value {
    let code = output.status.code().expect("the process exited normally");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.trim().is_empty(),
        "{argv:?} exited {code} without a report on stdout; stderr: {stderr}"
    );
    let value: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("{argv:?} stdout is not one JSON document ({err}): {stdout}"));
    assert_eq!(value["ok"], true, "{argv:?}");
    assert_eq!(value["meta"]["schema_version"], "1.0", "{argv:?}");
    if matches!(code, 0 | 1 | 3 | 4) {
        assert!(
            stderr.trim().is_empty(),
            "{argv:?} wrote diagnostics next to a report: {stderr}"
        );
    }
    value
}

fn data(output: &Output, argv: &[&str]) -> Value {
    envelope(output, argv)["data"].clone()
}

fn error(output: &Output, argv: &[&str]) -> Value {
    assert!(
        output.stdout.is_empty(),
        "{argv:?} wrote to stdout while failing"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let value: Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|err| panic!("{argv:?} stderr is not one JSON document ({err}): {stderr}"));
    assert_eq!(value["ok"], false, "{argv:?}");
    value["error"].clone()
}

fn exit_of(output: &Output) -> i32 {
    output.status.code().expect("the process exited normally")
}

fn codes(report: &Value) -> Vec<String> {
    report["findings"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row["code"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The codes that fail a run: everything doctor does not call info.
fn failing_codes(report: &Value) -> Vec<String> {
    report["findings"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["severity"] != "info")
                .filter_map(|row| row["code"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn has_code(report: &Value, code: &str) -> bool {
    codes(report).iter().any(|found| found == code)
}

fn link_text(link: &Path) -> PathBuf {
    fs::read_link(link).unwrap_or_else(|err| panic!("{} is not a link: {err}", link.display()))
}

/// A dangling link is still a link: `Path::exists` follows it and says no.
fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

fn parked_entry(sandbox: &Sandbox, surface: &str, name: &str) -> PathBuf {
    let runs: Vec<PathBuf> = fs::read_dir(sandbox.park_root())
        .expect("the fix run parks what it removes")
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(
        runs[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("doctor-"),
        "{runs:?}"
    );
    runs[0].join(surface).join(name)
}

// ---- doctor -----------------------------------------------------------

/// A project write re-proves what the plan saw: a path that changed in
/// between — the check/use window `add` runs in — is refused, not replaced.
#[test]
fn a_project_entry_that_changes_after_planning_is_refused() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("plain");
    sandbox.pool_skill("other");
    let project = sandbox.project();
    let paths = skillherd::paths::Paths::fixture(sandbox.home.path());
    let [claude, _agents] = skillherd::project::project_rows(&project);

    // The plan: nothing at `plain`, so link it; `other` is a managed link to
    // the wrong source, so relink it.
    fs::create_dir_all(&claude.surface).unwrap();
    let wrong = claude.surface.join("other");
    symlink(sandbox.library().join("plain"), &wrong).unwrap();
    let plan = skillherd::surfaces::plan_surface_restricted(
        &claude,
        &["plain".to_string(), "other".to_string()],
        &paths,
    )
    .unwrap();
    assert_eq!(plan.report.linked, ["other", "plain"], "{plan:?}");

    // Real user directories take both paths before the writes.
    let link = claude.surface.join("plain");
    fs::create_dir_all(&link).unwrap();
    fs::write(link.join("SKILL.md"), "# mine\n").unwrap();
    fs::remove_file(&wrong).unwrap();
    fs::create_dir_all(&wrong).unwrap();
    fs::write(wrong.join("SKILL.md"), "# mine too\n").unwrap();
    let before = (tree_hash(&link).unwrap(), tree_hash(&wrong).unwrap());

    let refusals = skillherd::surfaces::apply_ops(&claude.surface, &plan.ops, &paths).unwrap();
    let names: Vec<&str> = refusals
        .iter()
        .map(|refusal| refusal.name.as_str())
        .collect();
    assert_eq!(names, ["other", "plain"], "{refusals:?}");
    assert_eq!(
        (tree_hash(&link).unwrap(), tree_hash(&wrong).unwrap()),
        before,
        "both directories are untouched"
    );
    assert!(!is_link(&link) && !is_link(&wrong));
}

/// `drop`'s removal re-proves each link it read: one that changed in between is
/// refused, and the other still goes.
#[test]
fn drop_refuses_a_link_that_changed_after_the_read() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("plain");
    sandbox.pool_skill("swapped");
    let project = sandbox.project();
    let paths = skillherd::paths::Paths::fixture(sandbox.home.path());

    let claude = project.join(".claude/skills");
    fs::create_dir_all(&claude).unwrap();
    let kept = claude.join("plain");
    let swapped = claude.join("swapped");
    for link in [&kept, &swapped] {
        std::os::unix::fs::symlink(paths.library.join(link.file_name().unwrap()), link).unwrap();
    }
    let read = |path: &Path| {
        skillherd::managed::Expected::of(
            &skillherd::managed::entry_at(path, &paths).unwrap().unwrap(),
        )
        .expect("a managed link is a plan identity")
    };
    let planned = [
        (kept.clone(), read(&kept)),
        (swapped.clone(), read(&swapped)),
    ];

    // One of the two paths becomes a real user directory before the removal.
    fs::remove_file(&swapped).unwrap();
    fs::create_dir_all(&swapped).unwrap();
    fs::write(swapped.join("SKILL.md"), "# mine\n").unwrap();
    let before = tree_hash(&swapped).unwrap();

    let outcome = skillherd::project::remove_links(&paths, &planned, false).unwrap();

    assert_eq!(
        outcome.removed,
        [kept.clone()].as_slice(),
        "the unchanged link went"
    );
    assert!(!is_link(&kept) && !kept.exists());
    assert_eq!(outcome.refused.len(), 1, "{outcome:?}");
    assert!(
        outcome.refused[0].contains("is no longer ours"),
        "{outcome:?}"
    );
    assert_eq!(
        tree_hash(&swapped).unwrap(),
        before,
        "the directory is untouched"
    );
    assert_eq!(
        fs::read_to_string(swapped.join("SKILL.md")).unwrap(),
        "# mine\n"
    );
}

#[test]
fn a_healthy_home_reports_nothing_and_exits_zero() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("post");

    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 0, "{report}");
    assert!(
        report["findings"].as_array().unwrap().is_empty(),
        "{report}"
    );
    assert!(report["fixed"].as_array().unwrap().is_empty());
    assert!(report["failed"].as_array().unwrap().is_empty());
    assert!(report.get("quarantine").is_none());
    assert!(report.get("plan").is_none());
    assert!(report.get("refused").is_none());

    // Info findings never fail a run.
    let foreign = sandbox.claude_surface().join("synced");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("SKILL.md"), "not ours\n").unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 0, "{report}");
    assert!(has_code(&report, "foreign_entry"), "{report}");
    assert!(failing_codes(&report).is_empty(), "{report}");
}

#[test]
fn inbox_unreconciled_reports_retained_stage_arrivals_even_with_a_pool_copy() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    let paths = skillherd::paths::Paths::fixture(sandbox.home.path());
    let stage = skillherd::npx::prepare_stage(&paths).unwrap();
    let inbox = stage.inbox().join("fresh");
    fs::create_dir_all(&inbox).unwrap();
    fs::write(inbox.join("SKILL.md"), "# fresh\n").unwrap();
    sandbox.pool_skill("fresh");

    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 1, "{report}");
    assert!(has_code(&report, "inbox_unreconciled"), "{report}");
    let finding = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["code"] == "inbox_unreconciled")
        .unwrap();
    assert_eq!(finding["severity"], "warn");
    assert_eq!(finding["path"], inbox.to_string_lossy().as_ref());
    assert_eq!(finding["fixable"], false);
    assert!(
        finding["suggested_fix"]
            .as_str()
            .is_some_and(|hint| hint.contains("skillherd reconcile")),
        "{finding}"
    );

    let hint = finding["suggested_fix"].as_str().unwrap();
    assert!(hint.contains("--adopt fresh"), "{finding}");
    assert!(hint.contains("--discard fresh"), "{finding}");

    // Only consuming or explicitly discarding the retained arrival clears it.
    fs::remove_dir_all(&inbox).unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "inbox_unreconciled"), "{report}");
    assert!(failing_codes(&report).is_empty(), "{report}");
    assert_eq!(exit_of(&output), 0, "{report}");
}

#[test]
fn shared_agents_skills_are_foreign_surface_entries_not_import_arrivals() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    let entry = sandbox.agents_dir().join("foreign");
    fs::create_dir_all(&entry).unwrap();
    fs::write(entry.join("SKILL.md"), "# foreign\n").unwrap();
    let before = tree_hash(&entry).unwrap();

    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "inbox_unreconciled"), "{report}");
    assert!(
        report["findings"].as_array().unwrap().iter().any(|row| {
            row["code"] == "foreign_entry" && row["path"] == entry.to_string_lossy().as_ref()
        }),
        "{report}"
    );
    assert_eq!(exit_of(&output), 0, "{report}");
    assert_eq!(before, tree_hash(&entry).unwrap());
}

#[test]
fn a_dangling_managed_link_is_a_warn_and_a_foreign_one_is_info() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    fs::create_dir_all(sandbox.claude_surface()).unwrap();
    let dangling = sandbox.claude_surface().join("ghost");
    symlink("../../.agents/skill-library/ghost", &dangling).unwrap();

    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 1, "{report}");
    assert!(has_code(&report, "dangling_link"), "{report}");
    let finding = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["code"] == "dangling_link")
        .unwrap();
    assert_eq!(finding["severity"], "warn");
    assert_eq!(finding["fixable"], true);

    // A dangling *foreign* link is an info entry, not a managed finding.
    let foreign = sandbox.claude_surface().join("elsewhere");
    symlink("/nonexistent/repo/skills/elsewhere", &foreign).unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(has_code(&report, "foreign_entry"), "{report}");
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["code"] == "dangling_link")
            .count()
            == 1,
        "{report}"
    );

    // Clean: a surface with nothing in it.
    fs::remove_file(&dangling).unwrap();
    fs::remove_file(&foreign).unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "dangling_link"), "{report}");
    assert!(!has_code(&report, "foreign_entry"), "{report}");
    assert_eq!(exit_of(&output), 0, "{report}");
}

/// `npx skills add -g` fans a skill out into a harness surface as a link into
/// the shared `~/.agents/skills`. That link is another tool's: doctor reports
/// it as foreign (info), and `--fix` leaves it exactly where it is.
#[test]
fn an_npx_fanout_link_is_foreign_and_survives_a_fix() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("post");
    fs::create_dir_all(sandbox.agents_dir().join("web-design-guidelines")).unwrap();
    fs::write(
        sandbox.agents_dir().join("web-design-guidelines/SKILL.md"),
        "# placed by npx\n",
    )
    .unwrap();
    fs::create_dir_all(sandbox.codex_surface()).unwrap();
    let fanout = sandbox.codex_surface().join("web-design-guidelines");
    symlink("../../.agents/skills/web-design-guidelines", &fanout).unwrap();
    let text = link_text(&fanout);

    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    let finding = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["path"] == fanout.to_string_lossy().as_ref())
        .unwrap_or_else(|| panic!("the fan-out link is reported: {report}"));
    assert_eq!(finding["code"], "foreign_entry", "{report}");
    assert_eq!(finding["severity"], "info");
    assert_eq!(finding["fixable"], false);

    let output = sandbox.run(&["doctor", "--fix"]);
    let report = data(&output, &["doctor", "--fix"]);
    assert!(report["fixed"].as_array().unwrap().is_empty(), "{report}");
    assert!(is_link(&fanout), "a fix never moves another tool's link");
    assert_eq!(link_text(&fanout), text);
    assert!(
        !sandbox.park_root().exists(),
        "nothing was parked: {report}"
    );
}

#[test]
fn a_codex_disable_entry_on_a_pool_path_is_reported() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("post");
    fs::create_dir_all(sandbox.home.path().join(".codex")).unwrap();
    let config = sandbox.home.path().join(".codex/config.toml");
    fs::write(
        &config,
        format!(
            "[[skills.config]]\npath = \"{}\"\nenabled = false\n\n[[skills.config]]\npath = \"{}\"\nenabled = false\n",
            sandbox.agents_dir().join("post").display(),
            sandbox.home.path().join("elsewhere/skill").display()
        ),
    )
    .unwrap();

    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 1, "{report}");
    assert!(has_code(&report, "codex_disable_entry"), "{report}");
    assert_eq!(
        codes(&report)
            .iter()
            .filter(|code| *code == "codex_disable_entry")
            .count(),
        1,
        "only the pool path is ours: {report}"
    );
    let finding = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["code"] == "codex_disable_entry")
        .unwrap();
    assert_eq!(finding["path"], config.to_string_lossy().as_ref());
    assert!(
        finding["message"]
            .as_str()
            .is_some_and(|message| message
                .contains(&sandbox.agents_dir().join("post").display().to_string())),
        "{finding}"
    );

    // Clean: nothing disabled under the pool.
    fs::write(
        &config,
        format!(
            "[[skills.config]]\npath = \"{}\"\nenabled = true\n",
            sandbox.agents_dir().join("post").display()
        ),
    )
    .unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "codex_disable_entry"), "{report}");
    assert_eq!(exit_of(&output), 0, "{report}");
}

#[test]
fn grok_compat_scans_are_reported_until_they_are_off() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    let grok = sandbox.home.path().join(".grok");
    fs::create_dir_all(&grok).unwrap();
    let config = grok.join("config.toml");
    fs::write(&config, "[compat.claude]\nskills = true\n").unwrap();

    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 1, "{report}");
    let on: Vec<&Value> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["code"] == "grok_compat_skills_on")
        .collect();
    assert_eq!(on.len(), 2, "claude is on and codex is unset: {report}");
    for finding in &on {
        assert_eq!(finding["severity"], "warn");
        assert_eq!(finding["path"], config.to_string_lossy().as_ref());
    }

    // Clean: both scans off.
    fs::write(
        &config,
        "[compat.claude]\nskills = false\n[compat.codex]\nskills = false\n",
    )
    .unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "grok_compat_skills_on"), "{report}");
    assert_eq!(exit_of(&output), 0, "{report}");

    // Clean: no Grok on this machine, so no knob to report.
    fs::remove_dir_all(&grok).unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "grok_compat_skills_on"), "{report}");
}

#[test]
fn project_installs_are_reported_for_real_directories_only() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("linked");
    let project = sandbox.project();
    let raw = project.join(".claude/skills/raw");
    fs::create_dir_all(&raw).unwrap();
    fs::write(raw.join("SKILL.md"), "# raw\n").unwrap();
    let linked = project.join(".claude/skills/linked");
    fs::create_dir_all(linked.parent().unwrap()).unwrap();
    symlink(
        PathBuf::from("../../../.agents/skill-library/linked"),
        &linked,
    )
    .unwrap();

    let output = sandbox.run_in(&project, &["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(has_code(&report, "project_npx_install"), "{report}");
    let installs: Vec<&Value> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["code"] == "project_npx_install")
        .collect();
    assert_eq!(
        installs.len(),
        1,
        "a managed link is not an install: {report}"
    );
    assert_eq!(installs[0]["severity"], "info");
    // Doctor walks up from the process cwd, which the OS reports canonical
    // (macOS: /private/var for a /var temp dir).
    let raw_canonical = fs::canonicalize(&raw).unwrap();
    assert_eq!(
        installs[0]["path"],
        raw_canonical.to_string_lossy().as_ref()
    );
    assert_eq!(exit_of(&output), 0, "info does not fail a run: {report}");

    // Clean: no real project installs.
    fs::remove_dir_all(&raw).unwrap();
    let output = sandbox.run_in(&project, &["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "project_npx_install"), "{report}");
    assert_eq!(exit_of(&output), 0, "{report}");
}

#[test]
fn lock_snapshots_are_not_checked_without_the_opt_in() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    let live = sandbox.home.path().join(".agents/.skill-lock.json");
    let snapshot = sandbox.library().join(".npx/skill-lock.mac.json");
    fs::write(&live, "{\"version\":1}\n").unwrap();
    for kept in [None, Some("{\"version\":2}\n")] {
        if let Some(text) = kept {
            fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
            fs::write(&snapshot, text).unwrap();
        }
        let output = sandbox.run(&["doctor"]);
        let report = data(&output, &["doctor"]);
        assert!(!has_code(&report, "lock_drift"), "{report}");
        assert_eq!(exit_of(&output), 0, "{report}");
    }
    // An abandoned snapshot is irrelevant even after the live lock disappears.
    fs::remove_file(&live).unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "lock_drift"), "{report}");
    assert_eq!(exit_of(&output), 0, "{report}");
}

#[test]
fn a_lock_that_disagrees_with_the_pool_snapshot_is_reported() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    let live = sandbox.home.path().join(".agents/.skill-lock.json");
    // Snapshot drift is meaningful only when reconcile maintains snapshots.
    let run = || {
        sandbox
            .cmd()
            .env("SKILLHERD_SNAPSHOT_LOCK", "1")
            .arg("doctor")
            .output()
            .unwrap()
    };
    // The snapshot is per platform; every sandbox run is pinned to `mac`.
    let snapshot = sandbox.library().join(".npx/skill-lock.mac.json");
    fs::create_dir_all(live.parent().unwrap()).unwrap();
    fs::create_dir_all(snapshot.parent().unwrap()).unwrap();

    fs::write(&live, "{\"version\":1}\n").unwrap();
    fs::write(&snapshot, "{\"version\":2}\n").unwrap();
    let output = run();
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 1, "{report}");
    assert!(has_code(&report, "lock_drift"), "{report}");
    assert_eq!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["code"] == "lock_drift")
            .unwrap()["path"],
        snapshot.to_string_lossy().as_ref()
    );

    // Clean: the snapshot matches the live lock.
    fs::write(&snapshot, "{\"version\":1}\n").unwrap();
    let output = run();
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "lock_drift"), "{report}");
    assert_eq!(exit_of(&output), 0, "{report}");

    // The live lock with no snapshot at all is drift too.
    fs::remove_file(&snapshot).unwrap();
    let output = run();
    let report = data(&output, &["doctor"]);
    assert!(has_code(&report, "lock_drift"), "{report}");

    // Clean: neither file exists.
    fs::remove_file(&live).unwrap();
    let output = run();
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "lock_drift"), "{report}");
    assert_eq!(exit_of(&output), 0, "{report}");
}

fn record(imported_hash: &str, variants: &[(&str, &str)], pending: bool) -> NpxMeta {
    NpxMeta {
        source: "owner/repo".into(),
        source_type: "github".into(),
        source_url: "https://github.com/owner/repo".into(),
        r#ref: None,
        skill_path: ".agents/skills/post/SKILL.md".into(),
        skill_folder_hash: "abc".into(),
        imported_hash: imported_hash.into(),
        imported_at: "2026-09-21T00:00:00Z".into(),
        revision: "deadbeef".into(),
        variants: variants
            .iter()
            .map(|(key, hash)| ((*key).to_string(), (*hash).to_string()))
            .collect(),
        variants_pending: pending,
        harness_agents: Default::default(),
        release: None,
    }
}

#[test]
fn import_records_report_local_edits_pending_and_unserved_variants() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    let entry = sandbox.pool_skill("post");
    let variant = entry.join(".variants/claude");
    fs::create_dir_all(&variant).unwrap();
    fs::write(variant.join("SKILL.md"), "# post for claude\n").unwrap();
    let grok = entry.join(".variants/grok");
    fs::create_dir_all(&grok).unwrap();
    fs::write(grok.join("SKILL.md"), "# post for grok\n").unwrap();
    let grok_hash = tree_hash(&grok).unwrap();
    sandbox.npx_record(
        "post",
        &record(
            "0000000000000000",
            &[("claude", "1111111111111111"), ("grok", grok_hash.as_str())],
            true,
        ),
    );

    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 1, "{report}");
    assert!(has_code(&report, "local_edit"), "{report}");
    assert!(has_code(&report, "variant_drift"), "{report}");
    assert!(has_code(&report, "variant_pending"), "{report}");
    assert!(has_code(&report, "variant_unserved"), "{report}");

    let drift = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["code"] == "variant_drift")
        .unwrap();
    assert_eq!(drift["severity"], "warn");
    assert_eq!(drift["path"], variant.to_string_lossy().as_ref());
    let unserved = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["code"] == "variant_unserved")
        .unwrap();
    assert_eq!(unserved["severity"], "info");
    assert_eq!(unserved["path"], grok.to_string_lossy().as_ref());

    // Clean: hashes match, no pending variants, every variant key is served.
    fs::remove_dir_all(&grok).unwrap();
    let entry_hash = tree_hash(&entry).unwrap();
    let variant_hash = tree_hash(&variant).unwrap();
    sandbox.npx_record(
        "post",
        &record(
            entry_hash.as_str(),
            &[("claude", variant_hash.as_str())],
            false,
        ),
    );
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    for code in [
        "local_edit",
        "variant_drift",
        "variant_pending",
        "variant_unserved",
    ] {
        assert!(!has_code(&report, code), "{code}: {report}");
    }
    assert_eq!(exit_of(&output), 0, "{report}");

    // Clean: no import records at all.
    fs::remove_file(sandbox.library().join(".npx/post.json")).unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "local_edit"), "{report}");
    assert_eq!(exit_of(&output), 0, "{report}");
}

#[test]
fn fix_quarantines_what_it_removes_and_leaves_foreign_entries() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("post");
    fs::create_dir_all(sandbox.claude_surface()).unwrap();
    let dangling = sandbox.claude_surface().join("ghost");
    symlink("../../.agents/skill-library/ghost", &dangling).unwrap();
    let foreign = sandbox.claude_surface().join("synced");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("SKILL.md"), "# not ours\n").unwrap();

    let dangling_text = link_text(&dangling);

    let output = sandbox.run(&["doctor", "--fix"]);
    let report = data(&output, &["doctor", "--fix"]);
    assert_eq!(exit_of(&output), 0, "{report}");
    assert!(has_code(&report, "dangling_link"), "{report}");
    assert!(has_code(&report, "foreign_entry"), "{report}");
    assert!(
        report["fixed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["code"] == "dangling_link"),
        "{report}"
    );
    assert!(report["failed"].as_array().unwrap().is_empty(), "{report}");
    assert!(
        report.get("refused").is_none(),
        "nothing here is outside HOME: {report}"
    );
    let quarantine = report["quarantine"].as_str().expect("a quarantine root");
    assert!(quarantine.starts_with(&sandbox.park_root().to_string_lossy().to_string()));

    assert!(!is_link(&dangling), "the dangling link was moved");
    assert_eq!(
        link_text(&parked_entry(&sandbox, "claude", "ghost")),
        dangling_text
    );
    assert!(
        foreign.join("SKILL.md").is_file(),
        "a foreign entry is never moved"
    );
    assert_eq!(
        fs::read_to_string(foreign.join("SKILL.md")).unwrap(),
        "# not ours\n"
    );

    // Idempotent: a second fix has nothing left to move.
    let output = sandbox.run(&["doctor", "--fix"]);
    let report = data(&output, &["doctor", "--fix"]);
    assert!(!has_code(&report, "dangling_link"), "{report}");
    assert!(has_code(&report, "foreign_entry"), "{report}");
    assert_eq!(exit_of(&output), 0, "{report}");
}

#[test]
fn fix_dry_run_lists_the_fixes_and_changes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("post");
    fs::create_dir_all(sandbox.claude_surface()).unwrap();
    let dangling = sandbox.claude_surface().join("ghost");
    symlink("../../.agents/skill-library/ghost", &dangling).unwrap();

    let output = sandbox.run(&["doctor", "--fix", "--dry-run"]);
    let report = data(&output, &["doctor", "--fix", "--dry-run"]);
    assert_eq!(exit_of(&output), 1, "the finding is still open: {report}");
    let plan = report["plan"].as_array().expect("a plan");
    let move_op = plan
        .iter()
        .find(|op| op["path"] == dangling.to_string_lossy().as_ref())
        .expect("the plan lists the dangling link");
    assert_eq!(move_op["op"], "remove");
    let target = move_op["target"].as_str().unwrap();
    assert!(
        target.starts_with(&sandbox.park_root().to_string_lossy().to_string()),
        "{target}"
    );
    assert!(target.ends_with("claude/ghost"), "{target}");

    assert!(is_link(&dangling), "a dry run changes nothing");
    assert!(!sandbox.park_root().exists(), "a dry run parks nothing");
    assert!(report["fixed"].as_array().unwrap().is_empty());
    assert!(report.get("quarantine").is_none());

    // Plain --dry-run on doctor is read-only too.
    let output = sandbox.run(&["doctor", "--dry-run"]);
    let report = data(&output, &["doctor", "--dry-run"]);
    assert!(is_link(&dangling));
    assert!(report.get("plan").is_none());
}

#[test]
fn a_fix_path_outside_home_refuses_the_whole_fix() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    let elsewhere = TempDir::new().unwrap();
    let surface = elsewhere.path().join("skills");
    fs::create_dir_all(&surface).unwrap();
    // A managed link into the pool whose target is gone: fixable, and outside
    // HOME, so the whole fix is refused.
    let outside = surface.join("ghost");
    symlink(sandbox.library().join("ghost"), &outside).unwrap();

    let output = sandbox
        .cmd()
        .env("CLAUDE_SHARED", elsewhere.path())
        .args(["doctor", "--fix"])
        .output()
        .unwrap();
    let report = data(&output, &["doctor", "--fix"]);
    assert_eq!(exit_of(&output), 4, "{report}");
    let refused = report["refused"].as_str().expect("a refusal");
    assert!(refused.contains("outside HOME"), "{refused}");
    assert!(report["fixed"].as_array().unwrap().is_empty(), "{report}");
    let failed = report["failed"].as_array().unwrap();
    assert!(!failed.is_empty(), "{report}");
    assert!(
        failed
            .iter()
            .all(|finding| finding["code"] == "refused_outside_home"),
        "{report}"
    );
    assert!(
        failed
            .iter()
            .any(|finding| finding["path"] == outside.to_string_lossy().as_ref()),
        "{report}"
    );
    assert!(is_link(&outside), "a refused fix touches nothing");
    assert!(!sandbox.park_root().exists(), "nothing was parked");
}

#[test]
fn capabilities_lists_every_code_and_the_exit_dictionary() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["doctor", "--capabilities"]);
    let report = data(&output, &["doctor", "--capabilities"]);
    assert_eq!(exit_of(&output), 0);

    let published: Vec<String> = report["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["code"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(published.len(), 21, "{report}");
    for code in [
        "inbox_unreconciled",
        "dangling_link",
        "foreign_entry",
        "foreign_shadow",
        "surface_drift",
        "policy_name_missing",
        "codex_disable_entry",
        "grok_compat_skills_on",
        "visibility_excess",
        "project_npx_install",
        "lock_drift",
        "variant_drift",
        "variant_pending",
        "variant_unserved",
        "local_edit",
        "untracked_upstream",
        "unknown_provenance",
        "house_name_missing",
        "agent_shadow",
        "agent_drift",
    ] {
        let row = report["codes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["code"] == code)
            .unwrap_or_else(|| panic!("{code} is not published: {report}"));
        assert!(row["severity"].is_string(), "{row}");
        assert!(row["fixable"].is_boolean(), "{row}");
        assert!(!row["remediation"].as_str().unwrap().is_empty(), "{row}");
    }
    for (code, outcome) in [
        (0, "healthy"),
        (1, "findings"),
        (2, "partial_fix"),
        (3, "fix_failed"),
        (4, "refused"),
    ] {
        let row = report["exit_codes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["code"] == code)
            .unwrap_or_else(|| panic!("exit {code} is not published: {report}"));
        assert_eq!(row["outcome"], outcome);
    }
    assert_eq!(report["refusal_code"], "refused_outside_home");

    // Capabilities reports; it never repairs.
    let output = sandbox.run(&["doctor", "--capabilities", "--fix"]);
    assert_eq!(exit_of(&output), 2);
    assert_eq!(
        error(&output, &["doctor", "--capabilities", "--fix"])["code"],
        "usage"
    );
}

#[test]
fn a_fix_that_cannot_park_what_it_removes_fails_loudly() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    fs::create_dir_all(sandbox.claude_surface()).unwrap();
    let dangling = sandbox.claude_surface().join("ghost");
    symlink("../../.agents/skill-library/ghost", &dangling).unwrap();
    // A file where the park root must be a directory.
    fs::create_dir_all(sandbox.home.path().join(".agents")).unwrap();
    fs::write(sandbox.park_root(), "not a directory\n").unwrap();

    let output = sandbox.run(&["doctor", "--fix"]);
    let report = data(&output, &["doctor", "--fix"]);
    assert_eq!(exit_of(&output), 3, "{report}");
    assert!(report["fixed"].as_array().unwrap().is_empty(), "{report}");
    let failed = report["failed"].as_array().unwrap();
    assert!(!failed.is_empty(), "{report}");
    assert_eq!(failed[0]["code"], "dangling_link");
    assert!(
        failed[0]["message"]
            .as_str()
            .unwrap()
            .contains("could not be fixed"),
        "{report}"
    );
    assert!(
        report.get("quarantine").is_none(),
        "nothing was moved: {report}"
    );
    assert!(
        is_link(&dangling),
        "a failed fix leaves the entry where it was"
    );
}

#[test]
fn a_fix_that_moves_one_entry_and_fails_another_is_partial() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    fs::create_dir_all(sandbox.claude_surface()).unwrap();
    let movable = sandbox.claude_surface().join("ghost");
    symlink("../../.agents/skill-library/ghost", &movable).unwrap();
    let codex = sandbox.codex_surface();
    fs::create_dir_all(&codex).unwrap();
    let stuck = codex.join("stuck");
    symlink("../../.agents/skill-library/stuck", &stuck).unwrap();
    // Removing an entry needs write permission on the directory that holds it.
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o555)).unwrap();

    let output = sandbox.run(&["doctor", "--fix"]);
    let report = data(&output, &["doctor", "--fix"]);
    let failure = report["failed"].as_array().unwrap();
    // With a read-only surface the move cannot land; with a writable one it can.
    let partial = !report["fixed"].as_array().unwrap().is_empty() && !failure.is_empty();
    assert!(partial, "one move landed and one failed: {report}");
    assert_eq!(exit_of(&output), 2, "{report}");
    assert!(is_link(&stuck), "the refused move leaves its entry alone");
    assert!(!is_link(&movable), "the writable surface was parked");
    assert!(
        report["quarantine"].as_str().is_some(),
        "the parked entry reports where it went: {report}"
    );

    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
}

// ---- add --------------------------------------------------------------

fn variant_key(sandbox: &Sandbox, name: &str, key: &str) -> PathBuf {
    let variant = sandbox.library().join(name).join(".variants").join(key);
    fs::create_dir_all(&variant).unwrap();
    fs::write(variant.join("SKILL.md"), format!("# {name} for {key}\n")).unwrap();
    variant
}

#[test]
fn add_links_relative_serves_the_variant_and_reruns_unchanged() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("post");
    sandbox.pool_skill("plain");
    let variant = variant_key(&sandbox, "post", "claude");
    let project = sandbox.project();

    let argv = ["add", "post", "plain", "--project"];
    let output = sandbox.run_in(
        &project,
        &[
            "add",
            "post",
            "plain",
            "--project",
            project.to_str().unwrap(),
        ],
    );
    let report = data(&output, &argv);
    assert_eq!(exit_of(&output), 0, "{report}");
    assert!(report["refused"].as_array().unwrap().is_empty(), "{report}");
    let added = report["added"].as_array().unwrap();
    assert_eq!(added.len(), 2, "{report}");
    assert_eq!(added[0]["name"], "post");
    assert_eq!(
        added[0]["variant"], "claude",
        "the claude variant is present"
    );
    assert_eq!(added[1]["name"], "plain");
    assert!(added[1].get("variant").is_none(), "{report}");

    let claude_link = project.join(".claude/skills/post");
    let agents_link = project.join(".agents/skills/post");
    assert_eq!(
        claude_link.canonicalize().unwrap(),
        variant.canonicalize().unwrap(),
        "claude serves the variant tree"
    );
    assert_eq!(
        agents_link.canonicalize().unwrap(),
        sandbox.library().join("post").canonicalize().unwrap(),
        "the shared root serves the universal copy"
    );
    for link in [&claude_link, &agents_link] {
        assert!(
            !link_text(link).is_absolute(),
            "{} is a relative link",
            link.display()
        );
    }

    let surfaces = report["stripper"].as_array().unwrap();
    assert_eq!(surfaces.len(), 2, "{report}");
    assert_eq!(surfaces[0]["harness"], "claude");
    assert_eq!(
        surfaces[0]["report"]["unchanged"],
        serde_json::json!(["plain", "post"])
    );
    assert!(surfaces[1].get("harness").is_none(), "{report}");

    // Re-run: nothing to do, links untouched.
    let before = link_text(&claude_link);
    let output = sandbox.run_in(
        &project,
        &[
            "add",
            "post",
            "plain",
            "--project",
            project.to_str().unwrap(),
        ],
    );
    let report = data(&output, &argv);
    assert_eq!(exit_of(&output), 0, "{report}");
    assert!(report["added"].as_array().unwrap().is_empty(), "{report}");
    assert_eq!(report["unchanged"].as_array().unwrap().len(), 2, "{report}");
    assert!(report["plan"].as_array().unwrap().is_empty(), "{report}");
    assert_eq!(link_text(&claude_link), before);
}

/// A pool skill with a Claude block, a Codex block and neutral text.
fn fenced_skill(sandbox: &Sandbox, name: &str) {
    sandbox.pool_skill(name);
    fs::write(
        sandbox.library().join(name).join("SKILL.md"),
        format!(
            "# {name}\n\n<!-- claude-only -->\nCLAUDE-TEXT\n<!-- /claude-only -->\n\n<!-- codex-only -->\nCODEX-TEXT\n<!-- /codex-only -->\n\nNEUTRAL-TEXT\n"
        ),
    )
    .unwrap();
}

fn add(sandbox: &Sandbox, project: &Path, names: &[&str]) -> (i32, Value) {
    let mut argv = vec!["add"];
    argv.extend_from_slice(names);
    argv.extend_from_slice(&["--project", project.to_str().unwrap()]);
    let output = sandbox.run_in(project, &argv);
    (exit_of(&output), data(&output, &argv))
}

/// Adding a fenced skill twice is idempotent: the second run recognises the
/// served copies the first one wrote rather than refusing them as foreign.
#[test]
fn add_twice_on_a_fenced_skill_is_idempotent() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    fenced_skill(&sandbox, "fenced");
    let project = sandbox.project();

    let (code, report) = add(&sandbox, &project, &["fenced"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["added"][0]["name"], "fenced", "{report}");
    let claude = project.join(".claude/skills/fenced");
    let agents = project.join(".agents/skills/fenced");
    for copy in [&claude, &agents] {
        assert!(
            !is_link(copy) && copy.join(".skill-render.json").is_file(),
            "{report}"
        );
    }
    let before = (tree_hash(&claude).unwrap(), tree_hash(&agents).unwrap());

    let (code, report) = add(&sandbox, &project, &["fenced"]);
    assert_eq!(code, 0, "{report}");
    assert!(report["refused"].as_array().unwrap().is_empty(), "{report}");
    assert!(report["added"].as_array().unwrap().is_empty(), "{report}");
    assert_eq!(
        report["unchanged"],
        serde_json::json!(["fenced"]),
        "{report}"
    );
    assert!(report["plan"].as_array().unwrap().is_empty(), "{report}");
    assert_eq!(
        (tree_hash(&claude).unwrap(), tree_hash(&agents).unwrap()),
        before,
        "the served copies are untouched"
    );
}

/// Each project surface is stripped for its own harness, including when only
/// one of them needs repair; compacting a surface list must not change its
/// harness identity.
#[test]
fn repairing_one_project_surface_strips_it_for_its_own_harness() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    fenced_skill(&sandbox, "fenced");
    let project = sandbox.project();

    let (code, report) = add(&sandbox, &project, &["fenced"]);
    assert_eq!(code, 0, "{report}");
    let claude = project.join(".claude/skills/fenced/SKILL.md");
    let agents = project.join(".agents/skills/fenced");
    let claude_text = fs::read_to_string(&claude).unwrap();
    assert!(claude_text.contains("CLAUDE-TEXT") && !claude_text.contains("CODEX-TEXT"));

    // Half-installed: the shared root's copy is gone, the Claude one stays.
    fs::remove_dir_all(&agents).unwrap();
    let (code, report) = add(&sandbox, &project, &["fenced"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(
        report["added"][0]["links"],
        serde_json::json!([agents.to_string_lossy()]),
        "only the missing surface is written: {report}"
    );
    let shared = fs::read_to_string(agents.join("SKILL.md")).unwrap();
    assert!(
        shared.contains("NEUTRAL-TEXT")
            && !shared.contains("CLAUDE-TEXT")
            && !shared.contains("CODEX-TEXT"),
        "the shared root keeps no harness block: {shared}"
    );
    assert_eq!(fs::read_to_string(&claude).unwrap(), claude_text);
    let record: Value =
        serde_json::from_str(&fs::read_to_string(agents.join(".skill-render.json")).unwrap())
            .unwrap();
    assert!(record["harness"].is_null(), "{record}");
}

/// A malformed fence fails `add` before anything is written on any surface.
#[test]
fn add_fails_on_a_malformed_fence_and_writes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("plain");
    sandbox.pool_skill("broken");
    fs::write(
        sandbox.library().join("broken/SKILL.md"),
        "# broken\n<!-- mac-only -->\nnever closed\n",
    )
    .unwrap();
    let project = sandbox.project();

    let argv = [
        "add",
        "plain",
        "broken",
        "--project",
        project.to_str().unwrap(),
    ];
    let output = sandbox.run_in(&project, &argv);
    assert_eq!(exit_of(&output), 65);
    let body = error(&output, &argv);
    assert_eq!(body["code"], "render_failed", "{body}");
    assert_eq!(body["details"]["errors"][0]["name"], "broken", "{body}");
    let message = body["details"]["errors"][0]["message"].as_str().unwrap();
    assert!(
        message.starts_with("SKILL.md: line 4: ") && message.contains("never closed"),
        "{message}"
    );
    assert!(
        !project.join(".claude/skills").exists(),
        "nothing was written"
    );
    assert!(!project.join(".agents/skills").exists());
}

#[test]
fn add_refuses_a_foreign_entry_and_continues_the_others() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("post");
    sandbox.pool_skill("plain");
    let project = sandbox.project();
    let foreign = project.join(".claude/skills/plain");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("SKILL.md"), "# someone else's\n").unwrap();

    let output = sandbox.run_in(
        &project,
        &[
            "add",
            "post",
            "plain",
            "--project",
            project.to_str().unwrap(),
        ],
    );
    let argv = ["add", "post", "plain", "--project"];
    let report = data(&output, &argv);
    assert_eq!(exit_of(&output), 0, "{report}");
    let refused = report["refused"].as_array().unwrap();
    assert_eq!(refused.len(), 1, "{report}");
    assert_eq!(refused[0]["name"], "plain");
    assert_eq!(refused[0]["code"], "foreign_entry");
    assert_eq!(report["added"].as_array().unwrap().len(), 1, "{report}");
    assert_eq!(report["added"][0]["name"], "post");
    assert!(
        foreign.join("SKILL.md").is_file(),
        "the foreign entry survives"
    );
    assert!(
        !project.join(".agents/skills/plain").exists(),
        "a refused name touches neither surface"
    );

    // An unknown name is a hard error with a closest-name suggestion.
    let output = sandbox.run_in(
        &project,
        &["add", "pots", "--project", project.to_str().unwrap()],
    );
    assert_eq!(exit_of(&output), 66);
    let failure = error(&output, &["add", "pots", "--project"]);
    assert_eq!(failure["code"], "input_not_found");
    let suggestions = failure["details"]["suggestions"]["pots"]
        .as_array()
        .unwrap();
    assert!(suggestions.iter().any(|name| name == "post"), "{failure}");
}

/// A preset is read without following links and its contents are never
/// echoed: a preset (or the presets directory) linked to a secrets file is
/// refused, and a line that is not a skill name is reported by line number
/// only. The probe: a preset linked to a `.env` holding an API key used to
/// come back verbatim in `error.message` and `error.details.name`.
#[test]
fn a_preset_is_read_without_links_and_never_echoed() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("post");
    let secret = "OPENAI_API_KEY=sk-test-0123456789abcdef";
    let env_file = sandbox.home.path().join(".env");
    fs::write(&env_file, format!("{secret}\n")).unwrap();
    let globals = sandbox.library().join(".globals");
    let presets = globals.join("presets");
    fs::create_dir_all(&presets).unwrap();
    let project = sandbox.project();
    let run = |preset: &str| {
        let argv = [
            "add",
            preset,
            "--project",
            project.to_str().unwrap(),
            "--dry-run",
        ];
        let output = sandbox.run_in(&project, &argv);
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!all.contains("sk-test"), "the secret leaked: {all}");
        (exit_of(&output), error(&output, &argv))
    };

    // A preset that is a link to the secrets file.
    std::os::unix::fs::symlink(&env_file, presets.join("team")).unwrap();
    let (code, failure) = run("@team");
    assert_ne!(code, 0, "{failure}");
    assert!(
        failure["message"].as_str().unwrap().contains("symlink"),
        "{failure}"
    );

    // A regular preset whose line is not a skill name.
    fs::write(presets.join("bad"), format!("post\n{secret}\n")).unwrap();
    let (code, failure) = run("@bad");
    assert_eq!(code, 65, "{failure}");
    assert_eq!(failure["code"], "invalid_name", "{failure}");
    assert_eq!(failure["details"]["line"], 2, "{failure}");

    // The presets directory itself linked elsewhere.
    let elsewhere = sandbox.home.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("team"), format!("{secret}\n")).unwrap();
    fs::remove_dir_all(&presets).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &presets).unwrap();
    let (code, failure) = run("@team");
    assert_ne!(code, 0, "{failure}");
    assert!(
        failure["message"].as_str().unwrap().contains("symlink"),
        "{failure}"
    );
}

#[test]
fn add_expands_presets_and_refuses_an_unknown_one() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("post");
    sandbox.pool_skill("plain");
    let presets = sandbox.library().join(".globals/presets");
    fs::create_dir_all(&presets).unwrap();
    fs::write(
        presets.join("dev"),
        "# the dev lane\npost\n\nplain  # trailing\n",
    )
    .unwrap();
    let project = sandbox.project();

    let output = sandbox.run_in(
        &project,
        &["add", "@dev", "--project", project.to_str().unwrap()],
    );
    let report = data(&output, &["add", "@dev", "--project"]);
    assert_eq!(exit_of(&output), 0, "{report}");
    let names: Vec<&str> = report["added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["post", "plain"], "{report}");

    let output = sandbox.run_in(
        &project,
        &["add", "@nope", "--project", project.to_str().unwrap()],
    );
    assert_eq!(exit_of(&output), 66);
    let failure = error(&output, &["add", "@nope", "--project"]);
    assert_eq!(failure["code"], "input_not_found");
    assert!(
        failure["message"].as_str().unwrap().contains("presets"),
        "{failure}"
    );
}

#[test]
fn add_dry_run_lists_the_links_and_writes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("post");
    variant_key(&sandbox, "post", "claude");
    let project = sandbox.project();

    let output = sandbox.run_in(
        &project,
        &[
            "add",
            "post",
            "--project",
            project.to_str().unwrap(),
            "--dry-run",
        ],
    );
    let report = data(&output, &["add", "post", "--project", "--dry-run"]);
    assert_eq!(exit_of(&output), 0, "{report}");
    // The report describes what the run would do; nothing was written.
    assert_eq!(report["added"][0]["name"], "post", "{report}");
    let plan = report["plan"].as_array().unwrap();
    let links: Vec<&Value> = plan.iter().filter(|op| op["op"] == "link").collect();
    assert_eq!(links.len(), 2, "{report}");
    assert_eq!(
        links[0]["target"],
        sandbox
            .library()
            .join("post/.variants/claude")
            .to_string_lossy()
            .as_ref(),
        "the claude surface plans the variant tree"
    );
    assert_eq!(plan.len(), 2, "{report}");
    assert!(
        !project.join(".claude/skills").exists(),
        "a dry run writes nothing"
    );
    assert!(!project.join(".agents/skills").exists());
}

// ---- drop -------------------------------------------------------------

#[test]
fn drop_removes_all_three_roots_and_leaves_foreign_entries() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    sandbox.pool_skill("post");
    let project = sandbox.project();

    let output = sandbox.run_in(
        &project,
        &["add", "post", "--project", project.to_str().unwrap()],
    );
    let argv = ["add", "post", "--project"];
    assert_eq!(exit_of(&output), 0, "{}", data(&output, &argv));

    // The legacy root Codex used before `.agents/skills`.
    fs::create_dir_all(project.join(".codex/skills")).unwrap();
    symlink(
        sandbox.library().join("post"),
        project.join(".codex/skills/post"),
    )
    .unwrap();
    let other = project.join(".claude/skills/other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("SKILL.md"), "# other\n").unwrap();

    // Dry run first: it lists what it would remove.
    let output = sandbox.run_in(
        &project,
        &[
            "drop",
            "post",
            "--project",
            project.to_str().unwrap(),
            "--dry-run",
        ],
    );
    let report = data(&output, &["drop", "post", "--project", "--dry-run"]);
    assert_eq!(exit_of(&output), 0, "{report}");
    assert_eq!(
        report["dropped"][0]["existed"].as_array().unwrap().len(),
        3,
        "{report}"
    );
    assert_eq!(
        report["dropped"][0]["removed"].as_array().unwrap().len(),
        3,
        "the report describes what the run would remove: {report}"
    );
    assert_eq!(report["plan"].as_array().unwrap().len(), 3, "{report}");
    assert!(
        project.join(".claude/skills/post").exists(),
        "a dry run removes nothing"
    );

    let output = sandbox.run_in(
        &project,
        &["drop", "post", "--project", project.to_str().unwrap()],
    );
    let report = data(&output, &["drop", "post", "--project"]);
    assert_eq!(exit_of(&output), 0, "{report}");
    assert_eq!(
        report["dropped"][0]["existed"].as_array().unwrap().len(),
        3,
        "{report}"
    );
    assert_eq!(
        report["dropped"][0]["removed"].as_array().unwrap().len(),
        3,
        "{report}"
    );
    for link in [
        project.join(".claude/skills/post"),
        project.join(".agents/skills/post"),
        project.join(".codex/skills/post"),
    ] {
        assert!(!link.exists(), "{} is gone", link.display());
    }

    // Dropping a name that is not active is a warning, not a failure.
    let output = sandbox.run_in(
        &project,
        &["drop", "post", "--project", project.to_str().unwrap()],
    );
    let report = data(&output, &["drop", "post", "--project"]);
    assert_eq!(exit_of(&output), 0, "{report}");
    assert!(
        report["dropped"][0]["existed"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{report}"
    );
    assert!(
        envelope(&output, &["drop", "post", "--project"])["meta"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "not_active"),
        "{report}"
    );

    // A foreign entry is refused and survives.
    let output = sandbox.run_in(
        &project,
        &["drop", "other", "--project", project.to_str().unwrap()],
    );
    let report = data(&output, &["drop", "other", "--project"]);
    assert_eq!(exit_of(&output), 0, "{report}");
    assert_eq!(report["refused"][0]["code"], "foreign_entry", "{report}");
    assert!(
        other.join("SKILL.md").is_file(),
        "the foreign entry survives"
    );
    assert!(
        report["dropped"][0]["removed"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{report}"
    );
}

// ---- sync -------------------------------------------------------------

fn sync_stub(dir: &Path, body: &str) -> PathBuf {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("skill-library-sync");
    fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[test]
fn sync_runs_the_configured_script_and_reports_its_tail() {
    let sandbox = Sandbox::new();
    let path = sync_stub(
        sandbox.home.path(),
        "printf 'sync: pushed main\\n' >&2\necho 'rendered 42 skills'\nexit 0",
    );

    let output = sandbox
        .cmd()
        .env("PATH", &path)
        .env("SKILLHERD_SYNC_COMMAND", "skill-library-sync")
        .args(["sync"])
        .output()
        .unwrap();
    let report = data(&output, &["sync"]);
    assert_eq!(exit_of(&output), 0, "{report}");
    assert_eq!(report["command"], "skill-library-sync");
    assert_eq!(report["exit_code"], 0);
    let tail = report["output_tail"].as_str().unwrap();
    assert!(tail.contains("rendered 42 skills"), "{tail}");
    assert!(tail.contains("sync: pushed main"), "{tail}");
    assert!(
        output.stderr.is_empty(),
        "an agent's run keeps stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sync_reports_a_failing_script_and_a_missing_one() {
    let sandbox = Sandbox::new();
    let path = sync_stub(
        sandbox.home.path(),
        "echo 'skill-library-sync: ERROR: dirty checkout' >&2\nexit 1",
    );
    let output = sandbox
        .cmd()
        .env("PATH", &path)
        .env("SKILLHERD_SYNC_COMMAND", "skill-library-sync")
        .args(["sync"])
        .output()
        .unwrap();
    assert_eq!(exit_of(&output), 75);
    let failure = error(&output, &["sync"]);
    assert_eq!(failure["code"], "sync_failed");
    assert_eq!(failure["retryable"], true);
    assert_eq!(failure["details"]["exit_code"], 1);
    assert!(
        failure["details"]["output_tail"]
            .as_str()
            .unwrap()
            .contains("dirty checkout"),
        "{failure}"
    );

    // No script on PATH at all.
    let empty = TempDir::new().unwrap();
    let output = sandbox
        .cmd()
        .env("PATH", empty.path())
        .env("SKILLHERD_SYNC_COMMAND", "skill-library-sync")
        .args(["sync"])
        .output()
        .unwrap();
    assert_eq!(exit_of(&output), 78);
    let failure = error(&output, &["sync"]);
    assert_eq!(failure["code"], "config_error");
    assert!(
        failure["message"]
            .as_str()
            .unwrap()
            .contains("skill-library-sync"),
        "{failure}"
    );

    // The script has no dry run, so the flag is a usage error.
    let output = sandbox
        .cmd()
        .env("PATH", &path)
        .args(["sync", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(exit_of(&output), 2);
    assert_eq!(error(&output, &["sync", "--dry-run"])["code"], "usage");
}

fn finding_for<'a>(report: &'a Value, code: &str) -> &'a Value {
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["code"] == code)
        .unwrap_or_else(|| panic!("no {code}: {report}"))
}

#[test]
fn every_pool_entry_is_tracked_upstream_or_declared_house() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    let orphan = sandbox.untracked_skill("orphan");

    // No record, no lock entry, no house line: nobody knows where it came
    // from. That is how every hand-written skill starts (the README's quick
    // start), so it is info and `doctor` stays healthy.
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 0, "{report}");
    let row = finding_for(&report, "unknown_provenance");
    assert_eq!(row["severity"], "info");
    assert_eq!(row["path"], orphan.to_string_lossy().as_ref());
    assert!(!has_code(&report, "untracked_upstream"), "{report}");

    // The live npx lock names a source: it is an upstream copy nobody tracks,
    // and the hint is the install that adopts it from that source.
    let lock = sandbox.home.path().join(".agents/.skill-lock.json");
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(
        &lock,
        r#"{"version":3,"skills":{"orphan":{"source":"owner/repo","sourceType":"github","sourceUrl":"https://github.com/owner/repo.git","skillPath":"skills/orphan/SKILL.md","skillFolderHash":"abc"}}}"#,
    )
    .unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 1, "{report}");
    let row = finding_for(&report, "untracked_upstream");
    assert!(
        row["message"].as_str().unwrap().contains("owner/repo"),
        "{row}"
    );
    assert!(
        row["suggested_fix"]
            .as_str()
            .unwrap()
            .contains("skillherd install owner/repo --skill orphan --adopt orphan"),
        "{row}"
    );
    assert!(!has_code(&report, "unknown_provenance"), "{report}");

    // An import record is provenance.
    sandbox.npx_record("orphan", &record(&tree_hash(&orphan).unwrap(), &[], false));
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "untracked_upstream"), "{report}");
    assert!(!has_code(&report, "unknown_provenance"), "{report}");
    fs::remove_file(sandbox.library().join(".npx/orphan.json")).unwrap();
    fs::remove_file(&lock).unwrap();

    // So is a house line, a symlinked entry, and a directory that is not a skill.
    sandbox.declare_house("orphan");
    let linked_target = sandbox.home.path().join("repo/linked");
    fs::create_dir_all(&linked_target).unwrap();
    fs::write(linked_target.join("SKILL.md"), "# linked\n").unwrap();
    symlink(&linked_target, sandbox.library().join("linked")).unwrap();
    fs::create_dir_all(sandbox.library().join("not-a-skill")).unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "unknown_provenance"), "{report}");
    assert_eq!(exit_of(&output), 0, "{report}");

    // A house line with no pool entry is stale.
    sandbox.declare_house("ghost");
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 1, "{report}");
    let row = finding_for(&report, "house_name_missing");
    assert!(row["message"].as_str().unwrap().contains("ghost"), "{row}");
    assert!(
        !row["message"].as_str().unwrap().contains("orphan"),
        "{row}"
    );
}

/// A skill tracked as a GitHub release asset is tracked upstream: its record
/// is provenance like an npx import's, and nothing reports it as local.
#[test]
fn a_github_release_record_is_tracked_upstream() {
    let sandbox = Sandbox::new();
    sandbox.empty_policy();
    let entry = sandbox.untracked_skill("composio-cli");
    let source = skillherd::model::ReleaseSource {
        repo: "ComposioHQ/composio".into(),
        asset: "composio-skill.zip".into(),
        tag_prefix: "@composio/cli@".into(),
        dir: "composio-cli".into(),
    };
    let mut record = skillherd::release::record_for(&source, "@composio/cli@0.4.1", None);
    record.imported_hash = tree_hash(&entry).unwrap();
    record.imported_at = "2026-09-25T00:00:00Z".into();
    sandbox.npx_record("composio-cli", &record);

    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    for code in [
        "unknown_provenance",
        "untracked_upstream",
        "local_edit",
        "variant_pending",
    ] {
        assert!(!has_code(&report, code), "{code}: {report}");
    }
    assert_eq!(exit_of(&output), 0, "{report}");
}

#[test]
fn agent_shadow_and_agent_drift_trigger_and_clear() {
    let sandbox = Sandbox::new();
    let globals = sandbox.library().join(".globals");
    fs::create_dir_all(&globals).unwrap();
    fs::write(globals.join("sets"), "imp  main\n").unwrap();
    fs::write(globals.join("agents"), "imp\n").unwrap();
    let entry = sandbox.untracked_skill("imp");
    let stored = entry.join(".harness-agents/claude");
    fs::create_dir_all(&stored).unwrap();
    fs::write(stored.join("imp-a.md"), "agent\n").unwrap();
    let mut tracked = record(&tree_hash(&entry).unwrap(), &[], false);
    tracked
        .harness_agents
        .insert("claude".into(), tree_hash(&stored).unwrap());
    sandbox.npx_record("imp", &tracked);
    let agents = sandbox.home.path().join(".claude/agents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(agents.join("worker.md"), "a house agent\n").unwrap();
    let render = sandbox.run(&["render"]);
    assert_eq!(
        exit_of(&render),
        0,
        "{}",
        String::from_utf8_lossy(&render.stderr)
    );
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 0, "clean: {report}");

    // A file this tool did not create sits where the agent is served.
    fs::remove_file(agents.join("imp-a.md")).unwrap();
    fs::write(agents.join("imp-a.md"), "hand placed\n").unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 1, "{report}");
    let shadow = finding_for(&report, "agent_shadow");
    assert_eq!(shadow["severity"], "warn");
    assert_eq!(
        shadow["path"],
        agents.join("imp-a.md").to_string_lossy().as_ref()
    );
    assert!(!has_code(&report, "surface_drift"), "{report}");
    let output = sandbox.run(&["doctor", "--fix"]);
    assert_eq!(
        fs::read_to_string(agents.join("imp-a.md")).unwrap(),
        "hand placed\n",
        "--fix never replaces it: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    fs::remove_file(agents.join("imp-a.md")).unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert!(!has_code(&report, "agent_shadow"), "{report}");
    assert!(
        has_code(&report, "surface_drift"),
        "the missing link: {report}"
    );
    sandbox.run(&["render"]);

    // The stored agents drift from their record.
    fs::write(stored.join("imp-a.md"), "hand edit\n").unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 1, "{report}");
    let drift = finding_for(&report, "agent_drift");
    assert_eq!(drift["severity"], "warn");
    assert_eq!(drift["path"], stored.to_string_lossy().as_ref());
    assert!(
        !has_code(&report, "local_edit"),
        "agents stay out of the entry hash: {report}"
    );

    // Recorded but missing from the pool is drift too.
    fs::remove_dir_all(&stored).unwrap();
    let report = data(&sandbox.run(&["doctor"]), &["doctor"]);
    assert!(
        finding_for(&report, "agent_drift")["message"]
            .as_str()
            .unwrap()
            .contains("missing"),
        "{report}"
    );

    // Clean again once the record matches.
    fs::create_dir_all(&stored).unwrap();
    fs::write(stored.join("imp-a.md"), "agent\n").unwrap();
    let output = sandbox.run(&["doctor"]);
    let report = data(&output, &["doctor"]);
    assert_eq!(exit_of(&output), 0, "{report}");
    assert_eq!(
        fs::read_to_string(agents.join("worker.md")).unwrap(),
        "a house agent\n"
    );
}
