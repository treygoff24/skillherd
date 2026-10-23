//! The shared ABI, pinned.
//!
//! Four lanes build on the scaffold and may not edit its files, so this is a
//! compile-time consumer: every signature the plan declares is referenced here.
//! A later lane that changes a name, an argument order or a type breaks this
//! test rather than a sibling lane's build. Stubs are allowed to answer
//! `not_implemented`, and nothing here asserts that they do.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tempfile::TempDir;

use skillherd::cli::{
    AddArgs, Cli, DoctorArgs, DropArgs, InfoArgs, InstallArgs, ListArgs, OffArgs, OnArgs,
    ReconcileArgs, RemoveArgs, RenderArgs, SchemaArgs, SearchArgs, SyncArgs, UpdateArgs,
};
use skillherd::error::{CodeInfo, Error, Result};
use skillherd::hash::{Sha256, sha256_hex, tree_hash};
use skillherd::library::{SkillEntry, catalog};
use skillherd::lockfile::{MutatorGuard, lock_library};
use skillherd::managed::{Entry, EntryKind, has_marker, is_managed, list_entries};
use skillherd::model::{
    Envelope, ErrorBody, ErrorEnvelope, HarnessRow, Imported, Meta, NpxMeta, PathsInfo, PlanOp,
    Platform, Policy, ReadRoot, Refusal, SCHEMA_VERSION, SetRow, Severity, StripperError,
    StripperReport, Tier, Warning, now_iso8601,
};
use skillherd::names::{MAX_NAME_LEN, validate_name};
use skillherd::paths::{DEFAULT_NPX_PKG, Paths, lexical_abs, normalize, pick_npx_from_path};
use skillherd::policy::{KNOWN_HARNESSES, default_policy, effective_set, load, save, visible_set};
use skillherd::{commands, doctor, output, reconcile, render, surfaces};

struct Fixture {
    home: TempDir,
    paths: Paths,
}

impl Fixture {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        let paths = Paths::fixture(home.path());
        for root in [
            &paths.library,
            &paths.agents_dir,
            &paths.claude_shared.join("skills"),
        ] {
            std::fs::create_dir_all(root).unwrap();
        }
        Self { home, paths }
    }
}

/// Every function the other lanes call, coerced to its ABI signature.
#[test]
fn every_abi_signature_is_pinned() {
    let _: fn() -> Result<Paths, Error> = Paths::resolve;
    let _: fn(&Paths) -> Result<(), Error> = Paths::validate;
    let _: fn(&Paths) -> PathsInfo = Paths::info;
    let _: fn(&str) -> Result<&str, Error> = validate_name;

    let _: fn(&Path, &Paths) -> Result<bool, Error> = is_managed;
    let _: fn(&Path, &Paths) -> Result<Vec<Entry>, Error> = list_entries;
    let _: fn(&Path) -> bool = has_marker;
    let _: fn(&Path, &Paths) -> Result<bool, Error> = surfaces::is_managed;

    let _: fn(&Paths) -> Result<MutatorGuard, Error> = lock_library;
    let _: fn(&Path) -> Result<String, Error> = tree_hash;
    let _: fn(&[u8]) -> String = sha256_hex;
    let _: fn() -> Sha256 = Sha256::new;

    let _: fn(&Policy, &str, Platform) -> Result<Vec<String>, Error> = effective_set;
    let _: fn(&Policy, &str, Platform) -> Result<Vec<String>, Error> = visible_set;
    let _: fn(&Paths) -> Policy = default_policy;
    let _: fn(&Paths) -> Result<Policy, Error> = load;
    let _: fn(&Paths, &Policy) -> Result<(), Error> = save;

    let _: fn(&Paths, render::RenderOpts) -> Result<render::RenderReport, Error> = render::run;
    let _: fn(&Paths, reconcile::ReconcileOpts) -> Result<reconcile::ReconcileReport, Error> =
        reconcile::run;
    let _: fn(&Paths, bool, bool) -> Result<doctor::DoctorReport, Error> = doctor::run;
    let _: fn(&Paths) -> Result<Vec<SkillEntry>, Error> = catalog;

    let _: fn(Cli, &Paths) -> Result<Envelope, Error> = commands::dispatch;
    let _: fn(&SchemaArgs, &Paths, bool) -> Result<Value, Error> = commands::schema::run;
    let _: fn(&DoctorArgs, &Paths, bool) -> Result<Value, Error> = commands::doctor::run;
    let _: fn(&ListArgs, &Paths, bool) -> Result<Value, Error> = commands::list::run;
    let _: fn(&SearchArgs, &Paths, bool) -> Result<Value, Error> = commands::search::run;
    let _: fn(&InfoArgs, &Paths, bool) -> Result<Value, Error> = commands::info::run;
    let _: fn(&OnArgs, &Paths, bool) -> Result<Value, Error> = commands::on::run;
    let _: fn(&OffArgs, &Paths, bool) -> Result<Value, Error> = commands::off::run;
    let _: fn(&AddArgs, &Paths, bool) -> Result<Value, Error> = commands::add::run;
    let _: fn(&DropArgs, &Paths, bool) -> Result<Value, Error> = commands::drop::run;
    let _: fn(&RenderArgs, &Paths, bool) -> Result<Value, Error> = commands::render::run;
    let _: fn(&ReconcileArgs, &Paths, bool) -> Result<Value, Error> = commands::reconcile::run;
    let _: fn(&InstallArgs, &Paths, bool) -> Result<Value, Error> = commands::install::run;
    let _: fn(&UpdateArgs, &Paths, bool) -> Result<Value, Error> = commands::update::run;
    let _: fn(&RemoveArgs, &Paths, bool) -> Result<Value, Error> = commands::remove::run;
    let _: fn(&SyncArgs, &Paths, bool) -> Result<Value, Error> = commands::sync::run;

    assert_eq!(SCHEMA_VERSION, "1.0");
    assert_eq!(MAX_NAME_LEN, 64);
    assert_eq!(DEFAULT_NPX_PKG, "skills@1.7.0");
    assert_eq!(KNOWN_HARNESSES.len(), 6);
}

/// The full items behave at their edges: names, paths, links, locks, hashes.
#[test]
fn the_full_abi_items_behave() {
    let fixture = Fixture::new();
    let paths = &fixture.paths;

    assert_eq!(validate_name("post").unwrap(), "post");
    assert_eq!(
        validate_name("bad/name").unwrap_err().code(),
        "invalid_name"
    );

    paths.validate().unwrap();

    // A missing entry is not ours; a link into the pool is.
    assert!(!is_managed(&paths.agents_dir.join("nope"), paths).unwrap());
    let link = paths.claude_shared.join("skills/post");
    std::os::unix::fs::symlink("../../.agents/skill-library/post", &link).unwrap();
    assert!(is_managed(&link, paths).unwrap());
    assert_eq!(
        lexical_abs(
            link.parent().unwrap(),
            Path::new("../../.agents/skill-library/post")
        ),
        paths.library.join("post")
    );
    assert_eq!(normalize(Path::new("/a/../b")), PathBuf::from("/b"));

    let entries = list_entries(&paths.claude_shared.join("skills"), paths).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "post");
    assert!(entries[0].managed);
    assert!(matches!(entries[0].kind, EntryKind::Symlink { .. }));
    assert_eq!(
        list_entries(&paths.agents_dir.join("missing"), paths).unwrap(),
        Vec::new()
    );

    let hash = tree_hash(&paths.library).unwrap();
    assert_eq!(hash.len(), 64);
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(
        fixture.home.path().join(".agents/skill-library"),
        paths.library,
        "the fixture pool lives under the temp HOME"
    );

    let guard = lock_library(paths).unwrap();
    assert_eq!(guard.lock_path(), paths.library.join(".skill.lock"));
    assert_eq!(lock_library(paths).unwrap_err().code(), "library_busy");
    drop(guard);

    let policy = default_policy(paths);
    assert!(
        effective_set(&policy, "claude", paths.platform)
            .unwrap()
            .is_empty()
    );
    assert!(
        visible_set(&policy, "grok", paths.platform)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        effective_set(&policy, "nope", paths.platform)
            .unwrap_err()
            .code(),
        "policy_unknown_harness"
    );

    assert!(pick_npx_from_path(std::ffi::OsStr::new("")).is_none());
    assert!(!now_iso8601().is_empty());
}

/// `--dry-run` reaches the library entry points without changing anything.
#[test]
fn the_lane_entry_points_answer_through_the_abi() {
    let fixture = Fixture::new();
    let paths = &fixture.paths;

    let before = tree_hash(&paths.home).unwrap();
    let render_options = render::RenderOpts {
        surfaces: Vec::new(),
        dry_run: true,
        only: None,
    };
    let reconcile_options = reconcile::ReconcileOpts {
        dry_run: true,
        adopt: Vec::new(),
        restore: Vec::new(),
        keep: Vec::new(),
        inbox: Vec::new(),
        fetched: Default::default(),
    };

    // A stub reports `not_implemented`; a landed lane reports its own outcome.
    accept(render::run(paths, render_options));
    accept(reconcile::run(paths, reconcile_options));
    accept(doctor::run(paths, false, true));
    accept(catalog(paths));
    accept(load(paths));

    let after = tree_hash(&paths.home).unwrap();
    assert_eq!(before, after, "a dry run must not touch the tree");
}

#[test]
fn save_is_reachable_without_touching_the_pool() {
    let fixture = Fixture::new();
    let policy = default_policy(&fixture.paths);
    let _: fn(&Paths, &Policy) -> Result<(), Error> = save;
    let before = tree_hash(&fixture.paths.home).unwrap();
    let _ = save(&fixture.paths, &policy);
    assert_eq!(before, tree_hash(&fixture.paths.home).unwrap());
}

fn accept<T>(result: Result<T, Error>) {
    if let Err(error) = result {
        assert!(!error.code().is_empty());
    }
}

/// Every model type round-trips, so a payload an agent parses stays parseable.
#[test]
fn every_model_type_round_trips() {
    let fixture = Fixture::new();
    let info: PathsInfo = fixture.paths.info();

    let set_row = SetRow {
        name: "post".into(),
        main: true,
        lane: true,
        not_platform: vec![Platform::Mac],
        only: vec!["codex".into()],
        not: vec!["grok".into()],
        line: 3,
    };
    let harness = HarnessRow {
        name: "claude".into(),
        tier: Tier::Main,
        surface: PathBuf::from("/sandbox/user/.claude/skills"),
        variant_key: Some("claude".into()),
        reads: vec![ReadRoot::Surface, ReadRoot::AgentsDir],
    };
    let policy = Policy {
        rows: vec![set_row.clone()],
        harnesses: vec![harness.clone()],
        raw_lines: vec!["# curations".into()],
    };
    let record = NpxMeta {
        source: "owner/repo".into(),
        source_type: "github".into(),
        source_url: "https://github.com/owner/repo".into(),
        r#ref: Some("main".into()),
        skill_path: ".agents/skills/post/SKILL.md".into(),
        skill_folder_hash: "aaa".into(),
        imported_hash: "bbb".into(),
        imported_at: now_iso8601(),
        revision: "c".repeat(40),
        variants: BTreeMap::from([("claude".into(), "ddd".into())]),
        variants_pending: false,
        harness_agents: BTreeMap::new(),
        release: None,
    };
    let stripper = StripperReport {
        rendered: vec!["a".into()],
        refreshed: vec!["b".into()],
        unchanged: vec!["c".into()],
        errors: vec![StripperError {
            name: "d".into(),
            message: "boom".into(),
        }],
    };
    let entry = Entry {
        name: "post".into(),
        path: fixture.paths.claude_shared.join("skills/post"),
        kind: EntryKind::Symlink {
            target: fixture.paths.library.join("post"),
            dangling: true,
        },
        managed: true,
    };
    let plan = vec![
        PlanOp {
            op: "link".into(),
            path: fixture.paths.claude_shared.join("skills/post"),
            target: Some(fixture.paths.library.join("post")),
        },
        PlanOp {
            op: "unlink".into(),
            path: fixture.paths.agents_dir.join("post"),
            target: None,
        },
    ];
    let warning = Warning {
        code: "variants_unavailable".into(),
        message: "offline".into(),
    };
    let meta = Meta {
        schema_version: SCHEMA_VERSION.into(),
        // A literal, not `output::correlation_id()`: that call initializes the
        // process-wide cell, and `the_output_helpers_are_reachable` asserts an
        // explicitly set id is adopted verbatim, so the two tests raced for it.
        correlation_id: "8b0f4e0f-8a4e-4a4f-9d0a-000000000001".into(),
        paths: Some(info.clone()),
        warnings: vec![warning.clone()],
    };
    let imported = Imported {
        name: "post".into(),
        hash: "eee".into(),
        variants: vec!["claude".into()],
    };
    let refusal = Refusal {
        name: "deslop".into(),
        code: "local_edit_refused".into(),
        message: "edited".into(),
    };
    let body = ErrorBody {
        code: "usage".into(),
        message: "bad flag".into(),
        details: json!({ "flag": "--jsno" }),
        retryable: false,
        suggested_fix: Some("run `skillherd schema input`".into()),
    };

    let success = Envelope {
        ok: true,
        data: json!({ "plan": plan }),
        meta: meta.clone(),
    };
    let failure = ErrorEnvelope {
        ok: false,
        error: body,
        meta,
    };
    for text in [
        serde_json::to_string(&success).unwrap(),
        serde_json::to_string(&failure).unwrap(),
    ] {
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["meta"]["schema_version"], "1.0");
    }
    assert_eq!(success.data["plan"][0]["op"], "link");
    assert_eq!(failure.error.details["flag"], "--jsno");
    assert_eq!(
        serde_json::to_string(&success).unwrap(),
        serde_json::to_string(
            &serde_json::from_str::<Envelope>(&serde_json::to_string(&success).unwrap()).unwrap()
        )
        .unwrap()
    );
    assert_eq!(
        failure.error.code,
        serde_json::from_str::<ErrorEnvelope>(&serde_json::to_string(&failure).unwrap())
            .unwrap()
            .error
            .code
    );

    let round_trip = |value: Value| -> Value {
        let text = serde_json::to_string(&value).unwrap();
        serde_json::from_str(&text).unwrap()
    };
    assert_eq!(round_trip(json!(set_row)), json!(set_row));
    assert_eq!(round_trip(json!(harness)), json!(harness));
    assert_eq!(round_trip(json!(policy)), json!(policy));
    assert_eq!(round_trip(json!(record)), json!(record));
    assert_eq!(round_trip(json!(stripper)), json!(stripper));
    assert_eq!(round_trip(json!(entry)), json!(entry));
    assert_eq!(round_trip(json!(imported)), json!(imported));
    assert_eq!(round_trip(json!(refusal)), json!(refusal));
    assert_eq!(round_trip(json!(info)), json!(info));
    assert_eq!(
        round_trip(json!(Severity::Warn)),
        json!("warn"),
        "severities serialize lowercase"
    );
    assert_eq!(round_trip(json!(Tier::None)), json!("none"));
    assert_eq!(round_trip(json!(Platform::Linux)), json!("linux"));
    assert_eq!(round_trip(json!(ReadRoot::AgentsDir)), json!("agents_dir"));

    let codes: Vec<CodeInfo> = Error::dictionary();
    assert_eq!(codes.len(), 21);
    assert!(
        codes
            .iter()
            .any(|row| row.code == "library_busy" && row.retryable)
    );
}

/// The output helpers lanes call, including the warning channel.
#[test]
fn the_output_helpers_are_reachable() {
    let fixture = Fixture::new();
    let paths = &fixture.paths;

    output::set_correlation_id(Some("abi-test".into()));
    assert_eq!(
        output::correlation_id(),
        "abi-test",
        "an explicit id is adopted verbatim"
    );
    let _: fn(i32) = output::request_exit_code;
    let _: fn() -> i32 = output::exit_code;
    assert_eq!(output::exit_code(), 0, "commands start at success");
    output::warn("variants_unavailable", "offline");
    output::warn_error(&Error::variants_unavailable("post", "offline"));
    assert_eq!(output::warnings().len(), 2);
    assert!(
        output::warnings()[0]
            .to_string()
            .contains("variants_unavailable")
    );

    let envelope = output::success_envelope(json!({ "entries": [] }), Some(paths));
    assert!(envelope.ok);
    assert_eq!(envelope.meta.warnings.len(), 2);
    assert_eq!(
        envelope
            .meta
            .paths
            .as_ref()
            .map(|info| info.library.clone()),
        Some(paths.library.clone())
    );

    let error = Error::config_error("HOME is not set");
    let error_envelope = output::error_envelope(&error, None);
    assert!(!error_envelope.ok);
    assert_eq!(error_envelope.error.code, "config_error");
    assert!(error_envelope.meta.paths.is_none());

    output::clear_warnings();
    assert!(output::warnings().is_empty());
}
