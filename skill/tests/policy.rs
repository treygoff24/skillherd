//! The policy file, the catalog and the five policy/catalog commands.
//!
//! Every test drives the real binary against a temp `HOME`: the sets file is
//! read and written by the same code the setup will use, so a grammar rule or
//! a round-trip claim here is a claim about `skill`, not about a helper.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// The root and global overrides the binary reads; a test must not inherit the
/// developer's own setup.
const OVERRIDES: [&str; 26] = [
    "SKILLHERD_LIBRARY",
    "SKILLHERD_AGENTS_DIR",
    "SKILLHERD_LOCK",
    "SKILLHERD_STAGE",
    "CLAUDE_SHARED",
    "CLAUDE_CONFIG_DIR",
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

struct Fixture {
    home: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            home: TempDir::new().unwrap(),
        };
        fs::create_dir_all(fixture.library().join(".globals")).unwrap();
        fs::create_dir_all(fixture.home.path().join(".agents/skills")).unwrap();
        fixture
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn library(&self) -> PathBuf {
        self.home.path().join(".agents/skill-library")
    }

    fn sets_path(&self) -> PathBuf {
        self.library().join(".globals/sets")
    }

    fn write_sets(&self, text: &str) {
        fs::write(self.sets_path(), text).unwrap();
    }

    fn read_sets(&self) -> String {
        fs::read_to_string(self.sets_path()).unwrap()
    }

    /// `sets` rows naming `name`; a lost update shows up as a missing or doubled
    /// row.
    fn row_lines(&self, name: &str) -> usize {
        self.read_sets()
            .lines()
            .filter(|line| line.split_whitespace().next() == Some(name))
            .count()
    }

    fn write_harnesses(&self, text: &str) {
        fs::write(self.library().join(".globals/harnesses"), text).unwrap();
    }

    fn write_skill(&self, name: &str, source: &str) {
        let entry = self.library().join(name);
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join("SKILL.md"), source).unwrap();
    }

    fn write_variant(&self, name: &str, harness: &str, source: &str) {
        let variant = self.library().join(name).join(".variants").join(harness);
        fs::create_dir_all(&variant).unwrap();
        fs::write(variant.join("SKILL.md"), source).unwrap();
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::cargo_bin("skillherd").unwrap();
        cmd.env("HOME", self.home());
        for key in OVERRIDES {
            cmd.env_remove(key);
        }
        cmd.env("SKILLHERD_PLATFORM", "mac");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().unwrap()
    }

    /// Run, require success, and return `data`.
    fn data(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{} failed: {stderr}",
            args.join(" ")
        );
        let envelope: Value =
            serde_json::from_str(stdout.trim()).unwrap_or_else(|err| panic!("{args:?}: {err}"));
        assert_eq!(envelope["ok"], true, "{args:?}");
        envelope["data"].clone()
    }

    /// Run, require a reported failure, and return the error body.
    fn error(&self, args: &[&str]) -> (i32, Value) {
        let output = self.run(args);
        let code = output.status.code().unwrap();
        assert_ne!(code, 0, "{args:?} should have failed");
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let envelope: Value = serde_json::from_str(stderr.trim())
            .unwrap_or_else(|err| panic!("{args:?}: {err}: {stderr}"));
        (code, envelope["error"].clone())
    }

    fn warnings(&self, args: &[&str]) -> Vec<String> {
        let output = self.run(args);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let envelope: Value = serde_json::from_str(stdout.trim()).unwrap();
        envelope["meta"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|warning| warning["code"].as_str().unwrap().to_string())
            .collect()
    }

    fn tree_hash(&self) -> String {
        skillherd::hash::tree_hash(self.home()).unwrap()
    }

    /// The default six-row table with `sets` rows for four house skills:
    /// a plain main row, one the Mac never serves, and a lane row.
    fn setup(&self) {
        self.write_sets(
            "# curation history, carried over verbatim\n\
             \n\
             alpha main\n\
             beta main   !mac\n\
             gamma lane main\n",
        );
        for name in ["alpha", "beta", "gamma", "delta"] {
            self.write_skill(
                name,
                &format!("---\nname: {name}\ndescription: {name} does one thing\n---\n\nProse.\n"),
            );
        }
    }

    fn names(data: &Value) -> Vec<String> {
        data["skills"]
            .as_array()
            .unwrap()
            .iter()
            .map(|skill| skill["name"].as_str().unwrap().to_string())
            .collect()
    }
}

#[test]
fn sets_grammar_errors_carry_their_line_number() {
    let fixture = Fixture::new();
    fixture.setup();

    let cases = [
        ("alpha main\nbeta main bogus\n", "policy_bad_token", 2),
        (
            "alpha main\n# note\nbeta lane\n",
            "policy_lane_not_in_main",
            3,
        ),
        (
            "alpha main\nbeta main only:nope\n",
            "policy_unknown_harness",
            2,
        ),
        ("alpha main\n../etc main\n", "invalid_name", 2),
        ("alpha main # inline\n", "policy_bad_token", 1),
    ];
    for (text, code, line) in cases {
        fixture.write_sets(text);
        let (exit, error) = fixture.error(&["list"]);
        assert_eq!(exit, 65, "{text:?}");
        assert_eq!(error["code"], code, "{text:?}");
        assert_eq!(error["details"]["line"], line, "{text:?}");
    }
}

#[test]
fn an_unrepresentable_row_is_refused_by_load_with_its_line() {
    let fixture = Fixture::new();
    fixture.setup();
    fixture.write_harnesses(
        "claude main {claude_shared}/skills claude surface\n\
         codex main {codex_home}/skills codex surface,agents_dir\n\
         grok none {grok_home}/skills - surface,agents_dir\n\
         agents lane {agents_dir} - surface\n",
    );
    fixture.write_sets("alpha main\nbeta main lane not:codex\n");

    let (exit, error) = fixture.error(&["list"]);
    assert_eq!(exit, 65);
    assert_eq!(error["code"], "policy_unrepresentable");
    assert_eq!(error["details"]["line"], 2);
    assert_eq!(error["details"]["name"], "beta");
    assert!(
        error["suggested_fix"]
            .as_str()
            .unwrap()
            .contains("harnesses"),
        "{error}"
    );

    fixture.write_sets("alpha main\nbeta lane main not:grok\n");
    let (exit, error) = fixture.error(&["list"]);
    assert_eq!(exit, 65);
    assert_eq!(error["code"], "policy_unrepresentable");
    assert_eq!(error["details"]["line"], 2);
    assert!(error.to_string().contains("grok"), "{error}");

    for line in ["beta main only:agents\n", "beta main only:omp\n"] {
        fixture.write_sets(line);
        let (_, error) = fixture.error(&["list"]);
        assert_eq!(error["code"], "policy_unrepresentable", "{line}");
        assert_eq!(error["details"]["line"], 1);
    }

    // A `not:` for a harness that reads its own surface only is representable,
    // on a lane row too (grok sees lane rows through the shared dir).
    fixture.write_sets("alpha main lane\nbeta main lane not:claude\n");
    assert_eq!(
        Fixture::names(&fixture.data(&["list", "--harness", "claude"])),
        ["alpha"]
    );
    assert_eq!(
        Fixture::names(&fixture.data(&["list", "--harness", "grok"])),
        ["alpha", "beta"],
        "grok reads the shared dir, which not:claude does not touch"
    );
}

#[test]
fn harnesses_file_errors_name_the_line_too() {
    let fixture = Fixture::new();
    fixture.setup();
    for (text, code, line) in [
        (
            "claude main {claude_shared}/skills claude surface\nnope main\n",
            "policy_bad_token",
            2,
        ),
        (
            "claude weird {claude_shared}/skills claude surface\n",
            "policy_bad_token",
            1,
        ),
        (
            "claude main {nope_home}/skills claude surface\n",
            "policy_bad_token",
            1,
        ),
        (
            "claude main {claude_shared}/skills claude sky\n",
            "policy_bad_token",
            1,
        ),
        ("omp main {agents_dir} - surface\n", "policy_bad_token", 1),
    ] {
        fixture.write_harnesses(text);
        let (exit, error) = fixture.error(&["list"]);
        assert_eq!(exit, 65, "{text:?}");
        assert_eq!(error["code"], code, "{text:?}");
        assert_eq!(error["details"]["line"], line, "{text:?}");
    }
}

/// A `harnesses` grammar error teaches the harnesses row, not the `sets` row: a
/// trailing `#` note is a field, so the fix text has to say so.
#[test]
fn harnesses_errors_carry_the_harnesses_grammar() {
    let fixture = Fixture::new();
    fixture.setup();
    for text in [
        "agents   lane  {agents_dir}  -  surface  # note\n",
        "claude main {claude_shared}/skills claude\n",
        "claude main {claude_shared}/skills claude sky\n",
        "claude main {nope_home}/skills claude surface\n",
        "claude main {claude_shared}/skills claude surface\nclaude main {claude_shared}/skills claude surface\nomp main {agents_dir} - surface\n",
    ] {
        fixture.write_harnesses(text);
        let (exit, error) = fixture.error(&["list"]);
        assert_eq!(exit, 65, "{text:?}");
        assert_eq!(error["code"], "policy_bad_token", "{text:?}");
        let fix = error["suggested_fix"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            fix.contains("harnesses row is `harness tier surface variant-key reads`"),
            "the harnesses row shape is spelled out for {text:?}: {fix}"
        );
        assert!(
            fix.contains("own line"),
            "inline `#` comments are called out for {text:?}: {fix}"
        );
        assert!(
            !fix.contains("a sets row is"),
            "the sets grammar must not be handed to a harnesses error ({text:?}): {fix}"
        );
    }
}

/// A row with a trailing `# note` is the case that used to hand out the sets
/// grammar; the message itself names the field count and the comment rule.
#[test]
fn a_trailing_comment_on_a_harnesses_row_says_it_counts_as_fields() {
    let fixture = Fixture::new();
    fixture.setup();
    fixture.write_harnesses("agents   lane  {agents_dir}  -  surface  # note\n");

    let (exit, error) = fixture.error(&["list"]);
    assert_eq!(exit, 65);
    assert_eq!(error["details"]["line"], 1);
    let message = error["message"].as_str().unwrap_or_default().to_string();
    assert!(message.contains("has 7 fields"), "{message}");
    assert!(message.contains("counts as fields"), "{message}");
}

#[test]
fn list_by_harness_reports_the_visible_set_and_where_it_loads() {
    let fixture = Fixture::new();
    fixture.setup();

    let grok = fixture.data(&["list", "--harness", "grok"]);
    assert_eq!(
        Fixture::names(&grok),
        ["gamma"],
        "the shared dir carries the lane rows only"
    );
    assert_eq!(grok["tier"], "none");
    assert_eq!(grok["roots"].as_array().unwrap().len(), 2);
    assert!(
        grok["skills"]
            .as_array()
            .unwrap()
            .iter()
            .all(|skill| skill["via"] == "agents_dir"),
        "grok's own surface is emptied; the lane arrives through the shared dir"
    );
    assert_eq!(
        grok["skills"][0]["source"],
        fixture.library().join("gamma").to_str().unwrap()
    );

    let codex = fixture.data(&["list", "--harness", "codex"]);
    assert_eq!(
        Fixture::names(&codex),
        ["alpha", "gamma"],
        "codex's own surface serves every main row, so `via` is `surface` for each"
    );
    assert_eq!(codex["roots"][0]["via"], "surface");
    assert_eq!(codex["roots"][1]["via"], "agents_dir");
    assert!(
        codex["skills"]
            .as_array()
            .unwrap()
            .iter()
            .all(|skill| skill["via"] == "surface")
    );

    let claude = fixture.data(&["list", "--harness", "claude"]);
    assert_eq!(Fixture::names(&claude), ["alpha", "gamma"]);

    let omp = fixture.data(&["list", "--harness", "omp"]);
    assert_eq!(Fixture::names(&omp), ["gamma"]);
    assert_eq!(omp["tier"], "lane");
    assert_eq!(omp["roots"][0]["via"], "agents_dir");

    let unknown = fixture.error(&["list", "--harness", "nope"]);
    assert_eq!(unknown.0, 65);
    assert_eq!(unknown.1["code"], "policy_unknown_harness");
    assert!(unknown.1["suggested_fix"].as_str().unwrap().contains("omp"));
}

#[test]
fn only_and_not_rows_land_on_exactly_the_surfaces_that_can_hold_them() {
    let fixture = Fixture::new();
    fixture.setup();

    fixture.data(&["on", "delta", "--only", "codex"]);
    let codex = fixture.data(&["list", "--harness", "codex"]);
    assert_eq!(Fixture::names(&codex), ["alpha", "delta", "gamma"]);
    assert_eq!(
        codex["skills"][1]["via"], "surface",
        "codex's own surface is the only one that may serve it"
    );
    for harness in ["claude", "grok", "omp", "pi"] {
        let view = fixture.data(&["list", "--harness", harness]);
        assert!(
            !Fixture::names(&view).contains(&"delta".to_string()),
            "{harness} must not serve an only:codex row"
        );
    }

    fixture.data(&["on", "delta", "--tier", "lane", "--not", "claude"]);
    let claude = fixture.data(&["list", "--harness", "claude"]);
    assert!(!Fixture::names(&claude).contains(&"delta".to_string()));
    let grok = fixture.data(&["list", "--harness", "grok"]);
    assert!(
        Fixture::names(&grok).contains(&"delta".to_string()),
        "claude reads its own surface only, so not:claude is representable"
    );
}

#[test]
fn list_by_harness_serves_the_variant_path_when_one_exists() {
    let fixture = Fixture::new();
    fixture.setup();
    fixture.write_variant(
        "alpha",
        "claude",
        "---\ndescription: alpha for claude\n---\n",
    );

    let claude = fixture.data(&["list", "--harness", "claude"]);
    assert_eq!(Fixture::names(&claude), ["alpha", "gamma"]);
    assert_eq!(
        claude["skills"][0]["source"],
        fixture
            .library()
            .join("alpha/.variants/claude")
            .to_str()
            .unwrap()
    );
    assert_eq!(
        claude["skills"][1]["source"],
        fixture.library().join("gamma").to_str().unwrap(),
        "no variant for gamma"
    );

    let omp = fixture.data(&["list", "--harness", "omp"]);
    assert_eq!(
        omp["skills"][0]["source"],
        fixture.library().join("gamma").to_str().unwrap(),
        "the shared dir serves the lane rows, universal copy only"
    );
}

#[test]
fn list_default_tiers_and_all_active_in() {
    let fixture = Fixture::new();
    fixture.setup();

    let tiers = fixture.data(&["list"]);
    assert_eq!(tiers["mode"], "tiers");
    assert_eq!(Fixture::names(&tiers), ["alpha", "beta", "gamma"]);
    assert_eq!(tiers["skills"][0]["line"], 3, "1-based sets line");
    assert_eq!(tiers["skills"][0]["tiers"], serde_json::json!(["main"]));
    assert_eq!(
        tiers["skills"][2]["tiers"],
        serde_json::json!(["main", "lane"])
    );
    assert_eq!(tiers["skills"][0]["description"], "alpha does one thing");

    let lane = fixture.data(&["list", "--tier", "lane"]);
    assert_eq!(
        Fixture::names(&lane),
        ["gamma"],
        "the lane column: a lane row is also a main row, so `--tier main` lists it too"
    );
    let main = fixture.data(&["list", "--tier", "main"]);
    assert_eq!(Fixture::names(&main), ["alpha", "beta", "gamma"]);

    let all = fixture.data(&["list", "--all"]);
    assert_eq!(all["mode"], "all");
    assert_eq!(
        Fixture::names(&all),
        ["alpha", "beta", "delta", "gamma"],
        "the whole pool, active or not"
    );
    let by_name: std::collections::BTreeMap<&str, &Value> = all["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| (skill["name"].as_str().unwrap(), skill))
        .collect();
    let everything =
        serde_json::json!(["claude", "codex", "agents", "grok", "pi", "cursor", "omp"]);
    assert_eq!(
        by_name["delta"]["active_in"],
        serde_json::json!([]),
        "delta has no policy row"
    );
    assert_eq!(
        by_name["alpha"]["active_in"],
        serde_json::json!(["claude", "codex"]),
        "a main-only row reaches the main surfaces alone"
    );
    assert_eq!(by_name["gamma"]["active_in"], everything);
    assert_eq!(
        by_name["beta"]["active_in"],
        serde_json::json!([]),
        "!mac: no surface serves beta on this machine"
    );

    let (exit, error) = fixture.error(&["list", "--all", "--tier", "main"]);
    assert_eq!(exit, 2);
    assert_eq!(error["code"], "usage");
}

#[test]
fn on_tier_lane_writes_both_tokens_and_off_removes_the_row() {
    let fixture = Fixture::new();
    fixture.setup();

    let data = fixture.data(&["on", "delta", "--tier", "lane"]);
    assert_eq!(data["tier"], "lane");
    assert_eq!(data["changed"], true);
    assert_eq!(data["rows"][0]["op"], "add");
    assert_eq!(data["rows"][0]["after"], "delta main lane");
    assert_eq!(data["plan"][0]["op"], "set_row");
    assert_eq!(
        data["plan"][0]["path"],
        fixture.sets_path().to_str().unwrap()
    );
    assert_eq!(
        fixture.read_sets(),
        "# curation history, carried over verbatim\n\n\
         alpha main\n\
         beta main   !mac\n\
         gamma lane main\n\n\
         delta main lane\n",
        "the row is appended, every other byte untouched"
    );

    let data = fixture.data(&["on", "delta", "--tier", "main"]);
    assert_eq!(data["changed"], true);
    assert_eq!(data["rows"][0]["op"], "update");
    assert_eq!(data["rows"][0]["before"], "delta main lane");
    assert_eq!(data["rows"][0]["after"], "delta main");
    assert_eq!(
        fixture.read_sets(),
        "# curation history, carried over verbatim\n\n\
         alpha main\n\
         beta main   !mac\n\
         gamma lane main\n\n\
         delta main\n",
        "the edited row keeps its position and nothing else moves"
    );

    let data = fixture.data(&["on", "delta", "--tier", "main"]);
    assert_eq!(data["changed"], false, "activating twice changes nothing");
    assert_eq!(data["plan"].as_array().unwrap().len(), 0);
    assert_eq!(data["rows"][0]["op"], "unchanged");

    let data = fixture.data(&["off", "delta", "gamma"]);
    assert_eq!(data["changed"], true);
    assert_eq!(data["removed"].as_array().unwrap().len(), 2);
    assert!(!fixture.read_sets().contains("delta"));
    assert!(!fixture.read_sets().contains("gamma"));
    assert!(fixture.read_sets().contains("alpha main"));
}

/// A contending `on` must refuse before reading policy; its retry loads the
/// state the other writer published, rather than overwriting it from a snapshot.
#[test]
fn on_locks_before_reading_and_retry_keeps_the_latest_rows() {
    let fixture = Fixture::new();
    fixture.setup();
    let paths = skillherd::paths::Paths::fixture(fixture.home());
    let guard = skillherd::lockfile::lock_library(&paths).unwrap();
    let original = fixture.read_sets();
    fixture.write_sets("invalid policy row !unknown\n");

    // Invalid policy makes the ordering observable without timing or a large
    // fixture: a premature read reports a parse error instead of contention.
    let (exit, error) = fixture.error(&["on", "alpha", "--tier", "lane"]);
    assert_eq!(exit, 75, "{error}");
    assert_eq!(error["code"], "library_busy");
    assert_eq!(fixture.read_sets(), "invalid policy row !unknown\n");

    // Simulate the lock holder publishing a new row before the refused caller
    // retries. That row must survive the next successful update.
    fixture.write_sets(&format!("{original}delta main\n"));
    drop(guard);
    fixture.data(&["on", "alpha", "--tier", "lane"]);
    assert_eq!(fixture.row_lines("delta"), 1);
    assert_eq!(fixture.row_lines("alpha"), 1);
    assert!(fixture.read_sets().contains("alpha main lane"));
}

/// `remove` takes the library lock before it reads anything mutable: with the
/// lock held by another mutator it answers `library_busy` (75, retryable), not a
/// verdict read from a record outside the lock.
///
/// `delta` is a house skill with no import record, so a command that read its
/// record first would report `input_not_found` (66) instead — which is exactly
/// what the pre-fix ordering did.
#[test]
fn remove_takes_the_lock_before_reading_its_record() {
    let fixture = Fixture::new();
    fixture.setup();
    let paths = skillherd::paths::Paths::fixture(fixture.home());
    let guard = skillherd::lockfile::lock_library(&paths).unwrap();

    let (exit, error) = fixture.error(&["remove", "delta"]);
    assert_eq!(exit, 75, "{error}");
    assert_eq!(error["code"], "library_busy");
    assert!(error["retryable"].as_bool().unwrap_or(false));

    drop(guard);
    // With the lock free the same call reaches its real answer: no record.
    let (exit, error) = fixture.error(&["remove", "delta"]);
    assert_eq!(exit, 66, "{error}");
    assert_eq!(error["code"], "input_not_found");
}

#[test]
fn on_and_off_dry_runs_change_nothing() {
    let fixture = Fixture::new();
    fixture.setup();
    let before = fixture.tree_hash();

    let data = fixture.data(&["on", "delta", "--tier", "lane", "--dry-run"]);
    assert_eq!(data["changed"], true);
    assert_eq!(data["plan"][0]["op"], "set_row");
    assert!(data["render"].is_null());
    let data = fixture.data(&["off", "alpha", "--dry-run"]);
    assert_eq!(data["changed"], true);
    assert_eq!(data["removed"][0]["lines"], serde_json::json!([3]));

    assert_eq!(fixture.tree_hash(), before, "a dry run touches nothing");
    assert!(
        !fixture.library().join(".skill.lock").exists(),
        "a dry run takes no lock"
    );
}

#[test]
fn on_refuses_an_unrepresentable_combination_before_writing() {
    let fixture = Fixture::new();
    fixture.setup();
    let before = fixture.read_sets();

    for args in [
        vec!["on", "delta", "--tier", "lane", "--not", "grok"],
        vec!["on", "delta", "--only", "omp"],
        vec!["on", "delta", "--not", "cursor"],
    ] {
        let (exit, error) = fixture.error(&args);
        assert_eq!(exit, 65, "{args:?}");
        assert_eq!(error["code"], "policy_unrepresentable", "{args:?}");
        assert_eq!(error["details"]["name"], "delta", "{args:?}");
        assert_eq!(fixture.read_sets(), before, "{args:?} wrote nothing");
    }

    let (_, error) = fixture.error(&["on", "delta", "--only", "nope"]);
    assert_eq!(error["code"], "policy_unknown_harness");
    let data = fixture.data(&["on", "delta", "--not", "omp"]);
    assert_eq!(
        data["rows"][0]["after"], "delta main not:agents",
        "omp is the agents row: `--not omp` means the shared dir excludes it"
    );
    assert!(
        !fixture
            .data(&["list", "--harness", "omp"])
            .to_string()
            .contains("\"delta\""),
        "omp reads the shared dir only, so the row is gone from omp"
    );
    let data = fixture.data(&["on", "delta", "--not", "claude", "--no-mac"]);
    assert_eq!(data["rows"][0]["after"], "delta main !mac not:claude");
}

#[test]
fn on_refuses_a_name_the_pool_does_not_hold() {
    let fixture = Fixture::new();
    fixture.setup();

    let (exit, error) = fixture.error(&["on", "delt"]);
    assert_eq!(exit, 66);
    assert_eq!(error["code"], "input_not_found");
    assert!(
        error["suggested_fix"].as_str().unwrap().contains("delta"),
        "{error}"
    );
    assert!(!fixture.read_sets().contains("delt"));

    let (exit, error) = fixture.error(&["on", "../etc"]);
    assert_eq!(exit, 65);
    assert_eq!(error["code"], "invalid_name");
}

#[test]
fn off_warns_about_a_row_that_was_never_there() {
    let fixture = Fixture::new();
    fixture.setup();

    let data = fixture.data(&["off", "delta"]);
    assert_eq!(data["changed"], false);
    assert_eq!(data["removed"], serde_json::json!([]));
    assert!(
        fixture
            .warnings(&["off", "delta"])
            .contains(&"policy_row_absent".to_string()),
        "an absent row is a warning, not a failure"
    );
}

#[test]
fn search_ranks_name_hits_before_description_hits() {
    let fixture = Fixture::new();
    fixture.setup();
    fixture.write_skill("post", "---\ndescription: Publish letters\n---\n");
    fixture.write_skill(
        "blog",
        "---\ndescription: Writing that mentions post formats\n---\n",
    );
    fixture.write_skill("deslop", "---\ndescription: Tidy prose\n---\n");

    let data = fixture.data(&["search", "POST"]);
    assert_eq!(data["count"], 2);
    assert_eq!(Fixture::names(&data), ["post", "blog"]);
    assert_eq!(data["skills"][0]["match"], "name");
    assert_eq!(data["skills"][1]["match"], "description");

    let data = fixture.data(&["search", "post", "--names"]);
    assert_eq!(data["names"], serde_json::json!(["post", "blog"]));

    let data = fixture.data(&["search", "nothing-matches-this"]);
    assert_eq!(data["count"], 0, "an empty result is a success");
    assert_eq!(data["skills"], serde_json::json!([]));

    let (exit, error) = fixture.error(&["search", "   "]);
    assert_eq!(exit, 2);
    assert_eq!(error["code"], "usage");
}

#[test]
fn info_describes_one_skill_and_suggests_a_close_name() {
    let fixture = Fixture::new();
    fixture.setup();
    fixture.write_variant(
        "alpha",
        "claude",
        "---\ndescription: alpha for claude\n---\n",
    );
    fixture.write_skill("writing-plans", "---\ndescription: Plan a build\n---\n");

    let data = fixture.data(&["info", "alpha"]);
    assert_eq!(data["name"], "alpha");
    assert_eq!(data["tiers"], serde_json::json!(["main"]));
    assert_eq!(data["line"], 3);
    assert_eq!(data["variants"], serde_json::json!(["claude"]));
    assert_eq!(data["local_edit"], false);
    assert!(data["npx"].is_null());
    assert_eq!(
        data["visible_in"],
        serde_json::json!(["claude", "codex"]),
        "a main-only row is served by the main surfaces alone"
    );

    let beta = fixture.data(&["info", "beta"]);
    assert_eq!(
        beta["visible_in"],
        serde_json::json!([]),
        "!mac: beta is on no surface of this machine"
    );

    let (exit, error) = fixture.error(&["info", "writing-plan"]);
    assert_eq!(exit, 66);
    assert_eq!(error["code"], "input_not_found");
    assert!(
        error["suggested_fix"]
            .as_str()
            .unwrap()
            .contains("`writing-plans`"),
        "{error}"
    );

    let (exit, error) = fixture.error(&["info", "bad/name"]);
    assert_eq!(exit, 65);
    assert_eq!(error["code"], "invalid_name");
}

#[test]
fn the_catalog_follows_a_linked_entry_and_flags_a_local_edit() {
    let fixture = Fixture::new();
    fixture.setup();

    let outside = fixture.home().join("Code/deslop");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        outside.join("SKILL.md"),
        "---\ndescription: Rehabilitate slop\n---\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside, fixture.library().join("deslop")).unwrap();

    fs::create_dir_all(fixture.library().join(".npx")).unwrap();
    fs::write(
        fixture.library().join(".npx/alpha.json"),
        format!(
            "{{\"source\":\"owner/repo\",\"sourceType\":\"github\",\
              \"sourceUrl\":\"https://github.com/owner/repo\",\
              \"skillPath\":\".agents/skills/alpha/SKILL.md\",\
              \"skillFolderHash\":\"aaa\",\"importedHash\":\"{}\",\
              \"importedAt\":\"2026-09-21T00:00:00Z\",\"revision\":\"{}\",\
              \"variants\":{{\"claude\":\"bbb\"}},\"variantsPending\":false}}",
            "0".repeat(64),
            "1".repeat(40)
        ),
    )
    .unwrap();

    let info = fixture.data(&["info", "deslop"]);
    assert_eq!(info["is_symlink"], true);
    assert_eq!(
        info["link_target"],
        outside.to_str().unwrap(),
        "the link text, not the resolved path"
    );
    assert_eq!(info["description"], "Rehabilitate slop");
    assert_eq!(
        info["tiers"],
        serde_json::json!([]),
        "a linked entry with no row is in the pool but on no tier"
    );

    let info = fixture.data(&["info", "alpha"]);
    assert_eq!(info["local_edit"], true);
    assert_eq!(info["npx"]["importedHash"], "0".repeat(64));
    assert_eq!(info["npx"]["variants"]["claude"], "bbb");

    let all = fixture.data(&["list", "--all"]);
    let by_name: std::collections::BTreeMap<&str, &Value> = all["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| (skill["name"].as_str().unwrap(), skill))
        .collect();
    assert_eq!(by_name["deslop"]["is_symlink"], true);
    assert_eq!(by_name["deslop"]["active_in"], serde_json::json!([]));
    assert_eq!(by_name["alpha"]["local_edit"], true);
    assert_eq!(by_name["alpha"]["variants"], serde_json::json!([]));
}

#[test]
fn a_hand_edited_sets_file_survives_a_mutation_byte_for_byte() {
    let fixture = Fixture::new();
    fixture.setup();
    fixture.write_skill("extra", "---\ndescription: Extra\n---\n");

    let before = fixture.read_sets();
    // Activating a row that is already exactly as asked for rewrites nothing.
    fixture.data(&["on", "alpha", "--tier", "main"]);
    assert_eq!(fixture.read_sets(), before);

    // And removing a row leaves the rest of the file alone.
    fixture.data(&["off", "beta"]);
    assert_eq!(
        fixture.read_sets(),
        "# curation history, carried over verbatim\n\nalpha main\ngamma lane main\n"
    );
}

#[test]
fn policy_errors_show_only_a_bounded_offending_token() {
    let f = Fixture::new();
    let huge = "é".repeat(4096);
    for text in [
        format!("claude {huge} claude - surface\n"),
        format!("claude main {huge} - surface\n"),
        format!("claude main claude - {huge}\n"),
        "claude main claude - surface EXTRA_SECRET=not-a-token\n".to_string(),
    ] {
        f.write_harnesses(&text);
        let (_, error) = f.error(&["list"]);
        assert!(
            !error.to_string().contains("EXTRA_SECRET=not-a-token"),
            "whole row leaked: {error}"
        );
        assert!(
            error["details"]["token"].as_str().unwrap().len() <= 256,
            "{error}"
        );
        assert!(error["message"].as_str().unwrap().len() < 1024, "{error}");
    }
    f.write_harnesses("claude main claude - surface\n");
    for text in [
        format!("alpha main {huge}\n"),
        format!("alpha main only:{huge}\n"),
        format!("{huge} main\n"),
    ] {
        f.write_sets(&text);
        let (_, error) = f.error(&["list"]);
        assert!(error.to_string().len() < 2048, "oversized diagnostic");
    }
}

#[test]
fn refused_surface_explains_supported_names_and_local_configuration() {
    let f = Fixture::new();
    f.write_harnesses("custom main /arbitrary/root - surface\n");
    let (_, error) = f.error(&["list"]);
    let guidance = format!("{} {}", error["message"], error["suggested_fix"]);
    for name in [
        "claude",
        "codex",
        "agents",
        "grok",
        "pi",
        "cursor",
        "claude_home",
        "config.json",
        "built-in",
    ] {
        assert!(guidance.contains(name), "missing {name}: {guidance}");
    }
}
