//! Black-box tests of the `skill` binary: what stays true as the lanes land.
//!
//! Nothing here asserts a stub's `not_implemented`; the tests pin the contract
//! (parsing, envelope shape, exit codes, stream discipline) and keep passing
//! once every command is implemented.

use std::process::Output;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// Every exit code the contract publishes, doctor's linter dictionary included.
const EXIT_CODES: [i32; 11] = [0, 1, 2, 3, 4, 65, 66, 70, 75, 77, 78];

/// Every error code `skillherd schema error` must publish.
const ERROR_CODES: [&str; 21] = [
    "usage",
    "invalid_name",
    "input_not_found",
    "config_error",
    "library_busy",
    "library_dirty_unrelated",
    "local_edit_refused",
    "npx_failed",
    "sync_failed",
    "git_failed",
    "github_auth_failed",
    "release_failed",
    "release_archive_rejected",
    "render_failed",
    "policy_bad_token",
    "policy_lane_not_in_main",
    "policy_unknown_harness",
    "policy_unrepresentable",
    "variants_unavailable",
    "internal",
    "not_implemented",
];

/// The root and global environment variables the binary reads.
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
    "SKILLHERD_GH",
    "XDG_STATE_HOME",
    "SKILLHERD_CONFIG",
    "SKILLHERD_POOL_REMOTE",
    "SKILLHERD_SYNC_COMMAND",
    "SKILLHERD_CURSOR_SKILLS",
    "SKILLHERD_PI_SKILLS",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
];

struct Harness {
    home: TempDir,
}

impl Harness {
    fn new() -> Self {
        Self {
            home: TempDir::new().unwrap(),
        }
    }

    /// The binary with a temp HOME and no inherited `SKILLHERD_*` overrides.
    fn cmd(&self) -> Command {
        let mut cmd = Command::cargo_bin("skillherd").unwrap();
        cmd.env("HOME", self.home.path());
        for key in OVERRIDES {
            cmd.env_remove(key);
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().unwrap()
    }

    fn default_library(&self) -> std::path::PathBuf {
        self.home.path().join(".agents/skill-library")
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Assert one JSON document on the right stream, with a valid envelope.
///
/// Stream discipline: a success envelope is stdout, an error envelope is
/// stderr. Exit 2 is shared, because a usage error and `doctor`'s partial-fix
/// exit are both 2 — a doctor report may therefore sit on stdout there, but
/// never on both streams at once.
fn assert_envelope(output: &Output, argv: &[&str]) -> Value {
    let code = output.status.code().expect("the process exited normally");
    assert!(
        EXIT_CODES.contains(&code),
        "{argv:?} exited {code}, which is outside the dictionary; stderr: {}",
        stderr_of(output)
    );
    let stdout_text = stdout_of(output).trim().to_string();
    let stderr_text = stderr_of(output).trim().to_string();

    let (envelope, success) = match code {
        0 => {
            assert!(
                stderr_text.is_empty(),
                "{argv:?} wrote to stderr on success: {stderr_text}"
            );
            (parse_envelope(&stdout_text, argv, "stdout"), true)
        }
        1 | 3 | 4 => {
            assert!(
                stderr_text.is_empty(),
                "{argv:?} wrote diagnostics next to a doctor report: {stderr_text}"
            );
            (parse_envelope(&stdout_text, argv, "stdout"), true)
        }
        2 => {
            if stderr_text.is_empty() {
                (parse_envelope(&stdout_text, argv, "stdout"), true)
            } else {
                assert!(
                    stdout_text.is_empty(),
                    "{argv:?} wrote to both streams at once; stdout: {stdout_text}"
                );
                (parse_envelope(&stderr_text, argv, "stderr"), false)
            }
        }
        _ => {
            assert!(
                stdout_text.is_empty(),
                "{argv:?} wrote to stdout while failing: {stdout_text}"
            );
            (parse_envelope(&stderr_text, argv, "stderr"), false)
        }
    };

    assert_eq!(envelope["ok"], success, "{argv:?} envelope/exit mismatch");
    if success {
        assert!(
            envelope["data"].is_object() || envelope["data"].is_array(),
            "{argv:?}"
        );
    } else {
        let error = &envelope["error"];
        let reported = error["code"].as_str().unwrap_or_default();
        assert!(
            ERROR_CODES.contains(&reported),
            "{argv:?} reported unpublished code {reported:?}"
        );
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty()),
            "{argv:?}"
        );
        assert!(error["retryable"].is_boolean(), "{argv:?}");
        assert!(error.get("details").is_some(), "{argv:?}");
    }
    assert_envelope_meta(&envelope, argv);
    envelope
}

fn parse_envelope(text: &str, argv: &[&str], stream: &str) -> Value {
    assert!(
        !text.is_empty(),
        "{argv:?} exited non-zero without an envelope on {stream}"
    );
    serde_json::from_str(text)
        .unwrap_or_else(|err| panic!("{argv:?} {stream} is not one JSON document ({err}): {text}"))
}

fn assert_envelope_meta(envelope: &Value, argv: &[&str]) {
    let meta = &envelope["meta"];
    assert_eq!(meta["schema_version"], "1.0", "{argv:?}");
    assert!(
        meta["correlation_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "{argv:?} correlation_id"
    );
    assert!(
        meta.get("paths").is_some(),
        "{argv:?} meta.paths is required"
    );
    assert!(meta["warnings"].is_array(), "{argv:?} meta.warnings");
}

/// Every invocation an agent will try, with a valid argument set.
fn invocations() -> Vec<Vec<&'static str>> {
    vec![
        vec!["schema"],
        vec!["schema", "input"],
        vec!["schema", "output"],
        vec!["schema", "error"],
        vec!["doctor"],
        vec!["doctor", "--fix"],
        vec!["doctor", "--capabilities"],
        vec!["list"],
        vec!["list", "--harness", "claude"],
        vec!["list", "--tier", "lane"],
        vec!["list", "--all"],
        vec!["search", "post"],
        vec!["search", "post", "--names"],
        vec!["info", "post"],
        vec!["on", "post"],
        vec!["on", "post", "--tier", "lane"],
        vec![
            "on",
            "post",
            "--only",
            "claude",
            "--not",
            "grok",
            "--no-mac",
            "--no-cell",
        ],
        vec!["off", "post"],
        vec!["add", "post"],
        vec!["add", "post", "--project", "."],
        vec!["drop", "post"],
        vec!["drop", "post", "--project", "."],
        vec!["render"],
        vec!["render", "--surface", "claude", "--surface", "codex"],
        vec!["reconcile"],
        vec!["reconcile", "--adopt", "post", "--keep", "deslop"],
        vec!["install", "owner/repo"],
        vec!["install", "owner/repo", "--skill", "post"],
        vec!["update"],
        vec!["update", "post"],
        vec!["remove", "post"],
        vec!["remove", "post", "--force"],
        vec!["sync"],
    ]
}

#[test]
fn every_subcommand_answers_with_a_well_formed_envelope() {
    let harness = Harness::new();
    for argv in invocations() {
        let output = harness.run(&argv);
        assert_envelope(&output, &argv);
    }
}

#[test]
fn every_command_has_a_help_page() {
    let harness = Harness::new();
    for name in [
        "schema",
        "doctor",
        "list",
        "search",
        "info",
        "on",
        "off",
        "add",
        "drop",
        "render",
        "reconcile",
        "install",
        "update",
        "remove",
        "sync",
    ] {
        let output = harness.run(&[name, "--help"]);
        assert_eq!(output.status.code(), Some(0), "{name} --help");
        let text = stdout_of(&output);
        assert!(
            text.contains(name),
            "{name} --help does not name the command: {text}"
        );
        assert!(
            text.contains("Usage"),
            "{name} --help has no usage line: {text}"
        );
    }
}

#[test]
fn help_and_version_stay_human_readable() {
    let harness = Harness::new();
    let help = harness.run(&["--help"]);
    assert_eq!(help.status.code(), Some(0));
    let text = stdout_of(&help);
    for name in [
        "schema",
        "doctor",
        "list",
        "render",
        "reconcile",
        "install",
        "remove",
    ] {
        assert!(text.contains(name), "--help omits {name}: {text}");
    }
    let version = harness.run(&["--version"]);
    assert_eq!(version.status.code(), Some(0));
    assert!(
        stdout_of(&version).starts_with("skillherd "),
        "{}",
        stdout_of(&version)
    );
}

#[test]
fn dry_run_is_accepted_globally() {
    let harness = Harness::new();
    for argv in [
        vec!["render", "--dry-run"],
        vec!["--dry-run", "render"],
        vec!["list", "--dry-run", "--pretty"],
    ] {
        let output = harness.run(&argv);
        assert_envelope(&output, &argv);
    }
}

#[test]
fn a_pinned_correlation_id_makes_two_runs_byte_identical() {
    let harness = Harness::new();
    let first = harness.run(&["--correlation-id", "fixed-id", "schema", "output"]);
    let second = harness.run(&["--correlation-id", "fixed-id", "schema", "output"]);
    assert_envelope(&first, &["schema", "output"]);
    assert_eq!(stdout_of(&first), stdout_of(&second));
    let envelope: Value = serde_json::from_str(stdout_of(&first).trim()).unwrap();
    assert_eq!(envelope["meta"]["correlation_id"], "fixed-id");
}

#[test]
fn pretty_and_compact_output_parse_the_same() {
    let harness = Harness::new();
    let compact = harness.run(&["schema", "error"]);
    let pretty = harness.run(&["schema", "error", "--pretty"]);
    assert!(
        stdout_of(&pretty).contains("  \"ok\""),
        "{}",
        stdout_of(&pretty)
    );
    let compact_value: Value = serde_json::from_str(stdout_of(&compact).trim()).unwrap();
    let pretty_value: Value = serde_json::from_str(stdout_of(&pretty).trim()).unwrap();
    assert_eq!(compact_value["data"], pretty_value["data"]);
}

#[test]
fn schema_output_names_every_payload_type() {
    let harness = Harness::new();
    let output = harness.run(&["schema", "output"]);
    let envelope = assert_envelope(&output, &["schema", "output"]);
    let payloads = envelope["data"]["payloads"].as_object().unwrap();
    for name in [
        "Envelope",
        "Meta",
        "PathsInfo",
        "Warning",
        "PlanOp",
        "Policy",
        "SetRow",
        "HarnessRow",
        "Tier",
        "Severity",
        "ReadRoot",
        "StripperReport",
        "StripperError",
        "Imported",
        "Refusal",
        "NpxMeta",
        "Entry",
        "EntryKind",
    ] {
        assert!(payloads.contains_key(name), "schema output omits {name}");
    }
}

#[test]
fn schema_error_publishes_the_whole_dictionary() {
    let harness = Harness::new();
    let output = harness.run(&["schema", "error"]);
    let envelope = assert_envelope(&output, &["schema", "error"]);
    let codes: Vec<&str> = envelope["data"]["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["code"].as_str().unwrap())
        .collect();
    for code in ERROR_CODES {
        assert!(codes.contains(&code), "the dictionary omits {code}");
    }
    assert_eq!(envelope["data"]["exit_codes"]["78"], "configuration error");
}

#[test]
fn schema_input_lists_every_command() {
    let harness = Harness::new();
    let output = harness.run(&["schema", "input"]);
    let envelope = assert_envelope(&output, &["schema", "input"]);
    let names: Vec<&str> = envelope["data"]["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|sub| sub["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 17, "{names:?}");
    assert!(names.contains(&"strip"), "{names:?}");
    assert!(names.contains(&"npx"), "{names:?}");
    assert_eq!(
        envelope["data"]["blast_radius"]["doctor"],
        "read-only unless --fix"
    );
}

#[test]
fn sync_refuses_a_dry_run_with_a_usage_error() {
    let harness = Harness::new();
    let argv = ["sync", "--dry-run"];
    let output = harness.run(&argv);
    let envelope = assert_envelope(&output, &argv);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(envelope["error"]["code"], "usage");
}

#[test]
fn a_library_inside_a_surface_is_a_config_error() {
    let harness = Harness::new();
    let nested = harness.home.path().join(".claude/skills/pool");
    let output = harness
        .cmd()
        .env("SKILLHERD_LIBRARY", &nested)
        .arg("list")
        .output()
        .unwrap();
    let argv = ["list"];
    let envelope = assert_envelope(&output, &argv);
    assert_eq!(output.status.code(), Some(78));
    assert_eq!(envelope["error"]["code"], "config_error");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("claude"), "{message}");
    assert!(message.contains("pool"), "{message}");
    assert!(
        envelope["meta"]["paths"].is_null(),
        "unknowable paths report null"
    );
}

#[test]
fn a_surface_that_equals_the_library_is_a_config_error() {
    let harness = Harness::new();
    let library = harness.default_library();
    let output = harness
        .cmd()
        .env("SKILLHERD_AGENTS_DIR", &library)
        .arg("list")
        .output()
        .unwrap();
    let envelope = assert_envelope(&output, &["list"]);
    assert_eq!(output.status.code(), Some(78));
    assert_eq!(envelope["error"]["code"], "config_error");
}

#[test]
fn an_unknown_subcommand_is_a_usage_error() {
    let harness = Harness::new();
    let argv = ["frobnicate"];
    let output = harness.run(&argv);
    let envelope = assert_envelope(&output, &argv);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(envelope["error"]["code"], "usage");
    assert!(output.status.code() != Some(0));
}

#[test]
fn a_missing_home_is_a_config_error() {
    let harness = Harness::new();
    let output = harness
        .cmd()
        .env_remove("HOME")
        .arg("list")
        .output()
        .unwrap();
    let envelope = assert_envelope(&output, &["list"]);
    assert_eq!(output.status.code(), Some(78));
    assert_eq!(envelope["error"]["code"], "config_error");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("HOME")
    );
}

#[test]
fn meta_reports_the_paths_the_command_acted_on() {
    let harness = Harness::new();
    let output = harness.run(&["list"]);
    let envelope = assert_envelope(&output, &["list"]);
    let correlation_id = envelope["meta"]["correlation_id"].as_str().unwrap();
    assert_eq!(correlation_id.len(), 36, "generated ids are uuid-v4 shaped");
    assert_eq!(&correlation_id[14..15], "4");
    let paths = &envelope["meta"]["paths"];
    assert_eq!(
        paths["library"].as_str().unwrap(),
        harness.default_library().to_str().unwrap()
    );
    assert_eq!(paths["npx_pkg"], "skills@1.7.0");
    assert!(paths.get("skill_render").is_none(), "{paths}");
    let output = harness.run(&["schema", "output"]);
    let schema = assert_envelope(&output, &["schema", "output"]);
    let properties = schema["data"]["payloads"]["PathsInfo"]["properties"]
        .as_object()
        .expect("PathsInfo schema publishes its fields");
    assert!(!properties.contains_key("skill_render"), "{properties:?}");
    assert_eq!(
        paths["platform"],
        if cfg!(target_os = "macos") {
            "mac"
        } else {
            "linux"
        }
    );
}

#[test]
fn the_resolved_paths_honor_the_overrides() {
    let harness = Harness::new();
    let library = harness.home.path().join("pool");
    let output = harness
        .cmd()
        .env("SKILLHERD_LIBRARY", &library)
        .env("SKILLHERD_AGENTS_DIR", harness.home.path().join("inbox"))
        .env("SKILLHERD_NPX_PKG", "skills@9.9.9")
        .arg("list")
        .output()
        .unwrap();
    let envelope = assert_envelope(&output, &["list"]);
    assert_eq!(
        envelope["meta"]["paths"]["library"],
        library.to_str().unwrap()
    );
    assert_eq!(envelope["meta"]["paths"]["npx_pkg"], "skills@9.9.9");
}

#[test]
fn the_bad_overrides_are_named_in_the_error() {
    let harness = Harness::new();
    let output = harness
        .cmd()
        .env("SKILLHERD_PLATFORM", "solaris")
        .arg("list")
        .output()
        .unwrap();
    let envelope = assert_envelope(&output, &["list"]);
    assert_eq!(output.status.code(), Some(78));
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("SKILLHERD_PLATFORM")
    );
}

#[test]
fn a_flag_the_command_does_not_define_is_rejected_at_parse_time() {
    let harness = Harness::new();
    let output = harness.run(&["list", "--tier", "bogus"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout_of(&output).trim().is_empty());
    let envelope: Value = serde_json::from_str(stderr_of(&output).trim()).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "usage");
    assert!(predicate::str::contains("bogus").eval(stderr_of(&output).as_str()));
}
