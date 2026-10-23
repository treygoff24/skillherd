//! Standalone workflows: real git and renderer, isolated HOME, no network.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::TempDir;

#[test]
fn doctor_reports_legacy_config_without_loading_or_moving_it() {
    for xdg in [false, true] {
        let s = Sandbox::new();
        let root = if xdg {
            s.home().join("config-root")
        } else {
            s.home().join(".config")
        };
        let old = root.join("skill");
        fs::create_dir_all(&old).unwrap();
        // Invalid JSON proves the old configuration is not read as fallback.
        fs::write(old.join("config.json"), "not json").unwrap();
        let doctor = || {
            let mut cmd = s.cmd();
            if xdg {
                cmd.env("XDG_CONFIG_HOME", &root);
            }
            ok(cmd.arg("doctor").output().unwrap())
        };
        let report = doctor();
        let findings = report["data"]["findings"].as_array().unwrap();
        let migration = findings
            .iter()
            .find(|f| f["code"] == "legacy_config")
            .expect("legacy config migration finding");
        assert_eq!(migration["severity"], "info");
        assert_eq!(migration["fixable"], false);
        assert_eq!(migration["path"], old.to_str().unwrap());
        assert!(migration["message"].as_str().unwrap().contains("skillherd"));
        assert_eq!(
            fs::read_to_string(old.join("config.json")).unwrap(),
            "not json"
        );
        assert!(!root.join("skillherd").exists());
        fs::create_dir_all(root.join("skillherd")).unwrap();
        assert!(
            !doctor()["data"]["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["code"] == "legacy_config")
        );
    }
}

#[test]
fn linux_platform_and_legacy_cell_inputs_emit_canonical_linux() {
    let s = Sandbox::new();
    for platform in ["linux", "cell"] {
        let report = ok(s
            .cmd()
            .env("SKILLHERD_PLATFORM", platform)
            .arg("list")
            .output()
            .unwrap());
        assert_eq!(report["meta"]["paths"]["platform"], "linux");
    }
    for platform in ["linux", "cell"] {
        assert_eq!(
            serde_json::from_value::<skillherd::model::Platform>(json!(platform)).unwrap(),
            skillherd::model::Platform::Linux
        );
    }
    let source = s.home().join("fenced.md");
    fs::write(&source, "<!-- linux-only -->\nkept\n<!-- /linux-only -->\n").unwrap();
    for platform in ["linux", "cell"] {
        let report = ok(s
            .cmd()
            .args(["strip", "--keep", platform])
            .arg(&source)
            .output()
            .unwrap());
        assert_eq!(report["data"]["keep"], "linux");
        assert_eq!(report["data"]["files"][0]["content"], "kept\n");
    }
    // Both policy tokens and CLI flags produce the same canonical policy.
    let pool = s.pool();
    skill(&pool, "example");
    fs::create_dir_all(pool.join(".globals")).unwrap();
    for token in ["!linux", "!cell"] {
        fs::write(
            pool.join(".globals/sets"),
            format!("example main {token}\n"),
        )
        .unwrap();
        let report = ok(s
            .cmd()
            .arg("--pool")
            .arg(&pool)
            .env("SKILLHERD_PLATFORM", "linux")
            .args(["list", "--harness", "claude"])
            .output()
            .unwrap());
        assert!(report["data"]["skills"].as_array().unwrap().is_empty());
    }
    for flag in ["--no-linux", "--no-cell"] {
        // Exercise a new write, not the lossless no-op for an existing legacy row.
        fs::write(pool.join(".globals/sets"), "").unwrap();
        ok(s.cmd()
            .arg("--pool")
            .arg(&pool)
            .args(["on", "example", flag])
            .output()
            .unwrap());
        assert!(
            fs::read_to_string(pool.join(".globals/sets"))
                .unwrap()
                .contains("!linux")
        );
    }
}

#[test]
fn doctor_reads_legacy_linux_snapshot_only_when_canonical_snapshot_is_absent() {
    let s = Sandbox::new();
    let pool = s.pool();
    let npx = pool.join(".npx");
    fs::create_dir_all(&npx).unwrap();
    let lock = s.home().join("live-lock.json");
    let bytes = r#"{"version":1,"skills":{}}"#;
    fs::write(&lock, bytes).unwrap();
    fs::write(npx.join("skill-lock.cell.json"), bytes).unwrap();
    let doctor = || {
        ok(s.cmd()
            .arg("--pool")
            .arg(&pool)
            .env("SKILLHERD_LOCK", &lock)
            .env("SKILLHERD_PLATFORM", "linux")
            .env("SKILLHERD_SNAPSHOT_LOCK", "1")
            .arg("doctor")
            .output()
            .unwrap())
    };
    assert!(npx.join("skill-lock.cell.json").is_file());
    assert!(
        !doctor()["data"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == "lock_drift")
    );
    // Canonical bytes win, even when an identical legacy copy still exists.
    fs::write(npx.join("skill-lock.linux.json"), "different").unwrap();
    let output = s
        .cmd()
        .arg("--pool")
        .arg(&pool)
        .env("SKILLHERD_LOCK", &lock)
        .env("SKILLHERD_PLATFORM", "linux")
        .env("SKILLHERD_SNAPSHOT_LOCK", "1")
        .arg("doctor")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let finding = report["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["code"] == "lock_drift")
        .unwrap();
    assert_eq!(
        finding["path"],
        npx.join("skill-lock.linux.json").to_str().unwrap()
    );
}

struct Sandbox {
    root: TempDir,
}
impl Sandbox {
    fn new() -> Self {
        Self {
            root: TempDir::new().unwrap(),
        }
    }
    fn home(&self) -> &Path {
        self.root.path()
    }
    fn pool(&self) -> PathBuf {
        self.home().join("pool")
    }
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_skillherd"));
        cmd.env_clear()
            .env("HOME", self.home())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("SKILLHERD_NPX", "/nonexistent-npx");
        cmd
    }
    fn config(&self, config: Value) {
        let path = self.home().join(".config/skillherd/config.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, config.to_string()).unwrap();
    }
    fn git(&self, at: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(at)
            .args(args)
            .env("HOME", self.home())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Example")
            .env("GIT_AUTHOR_EMAIL", "you@example.com")
            .env("GIT_COMMITTER_NAME", "Example")
            .env("GIT_COMMITTER_EMAIL", "you@example.com")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    /// An owned npx run stage, the only place reconcile takes arrivals from
    /// (a user's ~/.agents/skills is a surface, never an inbox).
    fn inbox(&self) -> PathBuf {
        let stage = self.home().join(".agents/.skill-stage");
        let run = stage.join("run-fixture");
        let inbox = run.join(skillherd::npx::STAGE_INBOX);
        fs::create_dir_all(&inbox).unwrap();
        for dir in [&stage, &run] {
            fs::write(
                dir.join(skillherd::npx::STAGE_MARKER),
                "skill-stage-owner/v1 skill 00000000000000000000000000000000\n",
            )
            .unwrap();
        }
        inbox
    }
    fn upstream(&self) -> PathBuf {
        let remote = self.home().join("upstream");
        fs::create_dir_all(remote.join(".globals")).unwrap();
        skill(&remote, "example");
        fs::write(remote.join(".globals/sets"), "example main\n").unwrap();
        fs::write(remote.join(".globals/house"), "example\n").unwrap();
        fs::write(remote.join(".gitignore"), ".skill.lock\n").unwrap();
        self.git(&remote, &["init", "-b", "main"]);
        self.git(&remote, &["add", "."]);
        self.git(&remote, &["commit", "-m", "seed"]);
        remote
    }
}
fn skill(pool: &Path, name: &str) {
    fs::create_dir_all(pool.join(name)).unwrap();
    fs::write(
        pool.join(name).join("SKILL.md"),
        "---\nname: example\ndescription: Example skill.\n---\n\nInstructions.\n",
    )
    .unwrap();
}
fn ok(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn error(output: Output) -> Value {
    assert!(
        !output.status.success(),
        "unexpected success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stderr).unwrap()
}

#[test]
fn shared_policy_cannot_choose_a_home_destination() {
    let s = Sandbox::new();
    skill(&s.pool(), "example");
    s.config(json!({"library": "pool"}));
    fs::create_dir_all(s.pool().join(".globals")).unwrap();
    fs::write(s.pool().join(".globals/sets"), "example main\n").unwrap();
    fs::write(
        s.pool().join(".globals/harnesses"),
        "claude main {home}/.ssh - surface\n",
    )
    .unwrap();
    let report = error(s.cmd().arg("render").output().unwrap());
    assert_eq!(report["error"]["code"], "policy_bad_token");
    assert!(!s.home().join(".ssh/example").exists());
    assert!(s.pool().join("example/SKILL.md").is_file());
}

#[test]
fn shared_policy_cannot_render_into_the_library() {
    let s = Sandbox::new();
    skill(&s.pool(), "example");
    fs::write(s.pool().join("example/.skill-managed.json"), "{}").unwrap();
    s.config(json!({"library": "pool"}));
    fs::create_dir_all(s.pool().join(".globals")).unwrap();
    fs::write(
        s.pool().join(".globals/harnesses"),
        "claude none {library} - surface\n",
    )
    .unwrap();
    assert!(s.pool().join("example/SKILL.md").is_file());
    let report = error(s.cmd().arg("render").output().unwrap());
    assert_eq!(report["error"]["code"], "policy_bad_token");
    assert!(s.pool().join("example/SKILL.md").is_file());
}

#[test]
fn a_surface_symlink_cannot_alias_the_only_source() {
    let s = Sandbox::new();
    skill(&s.pool(), "example");
    fs::write(s.pool().join("example/.skill-managed.json"), "{}").unwrap();
    s.config(json!({"library": "pool"}));
    fs::create_dir_all(s.home().join(".claude")).unwrap();
    std::os::unix::fs::symlink(s.pool(), s.home().join(".claude/skills")).unwrap();
    assert!(s.pool().join("example/SKILL.md").is_file());
    let report = error(s.cmd().arg("render").output().unwrap());
    assert_eq!(report["error"]["code"], "config_error");
    assert!(s.pool().join("example/SKILL.md").is_file());
}

#[test]
fn policy_save_refuses_a_symlink_to_a_shell_config() {
    let s = Sandbox::new();
    skill(&s.pool(), "example");
    s.config(json!({"library": "pool"}));
    fs::create_dir_all(s.pool().join(".globals")).unwrap();
    let victim = s.home().join(".zshrc");
    fs::write(&victim, "# user data\n").unwrap();
    std::os::unix::fs::symlink(&victim, s.pool().join(".globals/sets")).unwrap();
    assert!(
        fs::symlink_metadata(s.pool().join(".globals/sets"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let report = error(s.cmd().args(["on", "example"]).output().unwrap());
    assert_eq!(report["error"]["code"], "config_error");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains(".globals/sets")
    );
    assert!(
        !report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("os error")
    );
    assert_eq!(fs::read_to_string(victim).unwrap(), "# user data\n");
}

#[test]
fn claude_config_dir_is_honoured() {
    let s = Sandbox::new();
    let root = s.home().join("relocated");
    let report = ok(s
        .cmd()
        .env("CLAUDE_CONFIG_DIR", &root)
        .arg("list")
        .output()
        .unwrap());
    assert_eq!(
        report["meta"]["paths"]["claude_shared"],
        root.to_str().unwrap()
    );
}

#[test]
fn configured_claude_home_outranks_session_config_dir() {
    let s = Sandbox::new();
    s.config(json!({"claude_home": "configured-claude"}));
    let report = ok(s
        .cmd()
        .env("CLAUDE_CONFIG_DIR", s.home().join("session-claude"))
        .arg("list")
        .output()
        .unwrap());
    assert_eq!(
        report["meta"]["paths"]["claude_shared"],
        s.home().join("configured-claude").to_str().unwrap()
    );
    let report = ok(s
        .cmd()
        .env("CLAUDE_CONFIG_DIR", s.home().join("session-claude"))
        .env("CLAUDE_SHARED", s.home().join("override-claude"))
        .arg("list")
        .output()
        .unwrap());
    assert_eq!(
        report["meta"]["paths"]["claude_shared"],
        s.home().join("override-claude").to_str().unwrap()
    );
}

#[test]
fn dangling_surface_is_unavailable_without_blocking_other_harnesses() {
    let s = Sandbox::new();
    skill(&s.pool(), "example");
    s.config(json!({"library": "pool"}));
    fs::create_dir_all(s.home().join(".cursor")).unwrap();
    let surface = s.home().join(".cursor/skills");
    std::os::unix::fs::symlink(s.home().join("missing"), &surface).unwrap();
    assert!(
        fs::symlink_metadata(&surface)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    for args in [vec!["list"], vec!["doctor"], vec!["on", "example"]] {
        let out = s.cmd().args(&args).output().unwrap();
        assert!(
            !out.stdout.is_empty(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(report["ok"], true, "{report}");
        assert!(
            report["meta"]["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["code"] == "surface_unavailable"),
            "{report}"
        );
    }
    assert!(s.home().join(".claude/skills/example/SKILL.md").is_file());
    assert_eq!(fs::read_link(&surface).unwrap(), s.home().join("missing"));
}

#[test]
fn linked_surface_source_is_reported_by_inspection_but_refuses_mutation() {
    let s = Sandbox::new();
    let surface = s.home().join(".agents/skills");
    skill(&surface, "example");
    fs::create_dir_all(s.pool()).unwrap();
    let entry = s.pool().join("example");
    std::os::unix::fs::symlink(surface.join("example"), &entry).unwrap();
    s.config(json!({"library": "pool"}));
    for args in [vec!["list", "--all"], vec!["doctor"]] {
        let out = s.cmd().args(&args).output().unwrap();
        assert!(
            !out.stdout.is_empty(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(report["ok"], true, "{report}");
        let warning = report["meta"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["code"] == "source_surface_overlap")
            .expect("inspection must explain overlap");
        let message = warning["message"].as_str().unwrap();
        assert!(
            message.contains("example") && message.contains("move") && message.contains("symlink"),
            "{message}"
        );
    }
    let report = error(s.cmd().args(["on", "example"]).output().unwrap());
    assert_eq!(report["error"]["code"], "config_error");
    assert!(!s.pool().join(".globals/sets").exists());
    assert!(entry.join("SKILL.md").is_file());
}

#[test]
fn off_preserves_the_sets_file_mode() {
    let s = Sandbox::new();
    skill(&s.pool(), "example");
    s.config(json!({"library": "pool"}));
    fs::create_dir_all(s.pool().join(".globals")).unwrap();
    let sets = s.pool().join(".globals/sets");
    fs::write(&sets, "example main\n").unwrap();
    fs::set_permissions(&sets, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        fs::metadata(&sets).unwrap().permissions().mode() & 0o777,
        0o644
    );
    ok(s.cmd().args(["off", "example"]).output().unwrap());
    assert_eq!(
        fs::metadata(&sets).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[test]
fn legacy_claude_directory_does_not_change_the_default() {
    let s = Sandbox::new();
    fs::create_dir_all(s.home().join(".claude-custom")).unwrap();
    let report = ok(s.cmd().arg("list").output().unwrap());
    assert_eq!(
        report["meta"]["paths"]["claude_shared"],
        s.home().join(".claude").to_str().unwrap()
    );
}

#[test]
fn default_npx_respects_path_order() {
    let s = Sandbox::new();
    let bin = s.home().join("version-manager/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("npx"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(bin.join("npx"), fs::Permissions::from_mode(0o755)).unwrap();
    let report = ok(s
        .cmd()
        .env_remove("SKILLHERD_NPX")
        .env("PATH", &bin)
        .arg("list")
        .output()
        .unwrap());
    assert_eq!(
        report["meta"]["paths"]["npx"],
        bin.join("npx").to_str().unwrap()
    );
}

#[test]
fn default_npx_accepts_a_real_executable_in_local_bin() {
    let s = Sandbox::new();
    let bin = s.home().join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("npx"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(bin.join("npx"), fs::Permissions::from_mode(0o755)).unwrap();
    let report = ok(s
        .cmd()
        .env_remove("SKILLHERD_NPX")
        .env("PATH", &bin)
        .arg("list")
        .output()
        .unwrap());
    assert_eq!(
        report["meta"]["paths"]["npx"],
        bin.join("npx").to_str().unwrap()
    );
}

#[test]
fn schema_does_not_require_home_or_valid_config() {
    let s = Sandbox::new();
    let report = ok(s
        .cmd()
        .env_remove("HOME")
        .env("SKILLHERD_CONFIG", "/no-such-config")
        .arg("schema")
        .output()
        .unwrap());
    assert!(report["data"]["output"].is_object());
}

#[test]
fn shared_policy_cannot_use_a_literal_destination() {
    let s = Sandbox::new();
    skill(&s.pool(), "example");
    s.config(json!({"library": "pool"}));
    fs::create_dir_all(s.pool().join(".globals")).unwrap();
    fs::write(
        s.pool().join(".globals/harnesses"),
        format!(
            "claude main {} - surface\n",
            s.home().join(".ssh").display()
        ),
    )
    .unwrap();
    error(s.cmd().arg("render").output().unwrap());
    assert!(!s.home().join(".ssh/example").exists());
}

#[test]
fn a_linked_library_source_cannot_live_in_an_output_tree() {
    let s = Sandbox::new();
    let surface = s.home().join(".claude/skills");
    skill(&surface, "example");
    fs::create_dir_all(s.pool()).unwrap();
    fs::write(surface.join("example/.skill-managed.json"), "{}").unwrap();
    std::os::unix::fs::symlink(surface.join("example"), s.pool().join("example")).unwrap();
    s.config(json!({"library": "pool"}));
    assert!(s.pool().join("example/SKILL.md").is_file());
    error(s.cmd().arg("render").output().unwrap());
    assert!(s.pool().join("example/SKILL.md").is_file());
}

#[test]
fn a_linked_variant_source_cannot_live_in_an_output_tree() {
    let s = Sandbox::new();
    let surface = s.home().join(".claude/skills");
    skill(&surface, "example");
    skill(&s.pool(), "example");
    let variants = s.pool().join("example/.variants");
    fs::create_dir(&variants).unwrap();
    fs::write(surface.join("example/.skill-managed.json"), "{}").unwrap();
    std::os::unix::fs::symlink(surface.join("example"), variants.join("claude")).unwrap();
    s.config(json!({"library": "pool"}));
    assert!(variants.join("claude/SKILL.md").is_file());
    error(s.cmd().arg("render").output().unwrap());
    assert!(variants.join("claude/SKILL.md").is_file());
}

#[test]
fn policy_save_refuses_a_symlinked_globals_directory() {
    let s = Sandbox::new();
    skill(&s.pool(), "example");
    s.config(json!({"library": "pool"}));
    let outside = s.home().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sets"), "# user data\n").unwrap();
    std::os::unix::fs::symlink(&outside, s.pool().join(".globals")).unwrap();
    error(s.cmd().args(["on", "example"]).output().unwrap());
    assert_eq!(
        fs::read_to_string(outside.join("sets")).unwrap(),
        "# user data\n"
    );
}

#[test]
fn legacy_layout_does_not_select_a_sync_executable() {
    let s = Sandbox::new();
    let pool = s.home().join(".agents/skill-library");
    fs::create_dir_all(pool.join(".globals")).unwrap();
    fs::create_dir_all(s.home().join(".claude-custom")).unwrap();
    fs::write(
        pool.join(".globals/harnesses"),
        "claude main claude claude surface\n",
    )
    .unwrap();
    let bin = s.home().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("skill-library-sync");
    fs::write(&script, "#!/bin/sh\nexit 42\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let report = ok(s.cmd().env("PATH", &bin).arg("sync").output().unwrap());
    assert_eq!(report["data"]["transport"], "folder");
}

#[test]
fn fresh_home_uses_stock_claude_and_no_node_is_needed() {
    let s = Sandbox::new();
    let report = ok(s
        .cmd()
        .env_remove("SKILLHERD_NPX")
        .env("PATH", "/nonexistent-path")
        .arg("list")
        .output()
        .unwrap());
    assert_eq!(
        report["meta"]["paths"]["claude_shared"],
        s.home().join(".claude").to_str().unwrap()
    );
    assert_eq!(
        report["meta"]["paths"]["library"],
        s.home().join(".agents/skill-library").to_str().unwrap()
    );
    assert!(
        !s.home().join(".agents").exists(),
        "read-only listing creates nothing"
    );
}

#[test]
fn configured_folder_renders_stock_surfaces_and_is_idempotent() {
    let s = Sandbox::new();
    skill(&s.pool(), "example");
    s.config(json!({"library": "~/pool"}));
    ok(s.cmd().args(["on", "example"]).output().unwrap());
    for root in [".claude/skills", ".codex/skills"] {
        assert_eq!(
            fs::canonicalize(s.home().join(root).join("example")).unwrap(),
            fs::canonicalize(s.pool().join("example")).unwrap()
        );
    }
    let again = ok(s.cmd().arg("render").output().unwrap());
    assert!(
        again["data"]["plan"].as_array().unwrap().is_empty(),
        "{again}"
    );
    assert!(!s.pool().join(".git").exists());
}

#[test]
fn flags_override_environment_which_overrides_config() {
    let s = Sandbox::new();
    s.config(json!({"library": "config-pool"}));
    let env_pool = s.home().join("env-pool");
    let report = ok(s
        .cmd()
        .env("SKILLHERD_LIBRARY", &env_pool)
        .arg("list")
        .output()
        .unwrap());
    assert_eq!(
        report["meta"]["paths"]["library"],
        env_pool.to_str().unwrap()
    );
    let report = ok(s
        .cmd()
        .env("SKILLHERD_LIBRARY", &env_pool)
        .arg("--pool")
        .arg(s.pool())
        .arg("list")
        .output()
        .unwrap());
    assert_eq!(
        report["meta"]["paths"]["library"],
        s.pool().to_str().unwrap()
    );
}

#[test]
fn malformed_or_unknown_config_is_not_silently_ignored() {
    let s = Sandbox::new();
    s.config(json!({"libray": "typo"}));
    let report = error(s.cmd().arg("list").output().unwrap());
    assert_eq!(report["error"]["code"], "config_error");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown field")
    );
    let report = error(
        s.cmd()
            .env("SKILLHERD_CONFIG", s.home().join("missing.json"))
            .arg("list")
            .output()
            .unwrap(),
    );
    assert_eq!(report["error"]["code"], "config_error");
}

#[test]
fn sync_clones_then_fast_forwards_an_arbitrary_remote() {
    let s = Sandbox::new();
    let remote = s.upstream();
    s.config(json!({"library": s.pool(), "remote": remote}));
    let first = ok(s.cmd().arg("sync").output().unwrap());
    assert_eq!(first["data"]["transport"], "clone");
    assert!(s.home().join(".claude/skills/example/SKILL.md").is_file());
    fs::write(remote.join("example/SKILL.md"), "Updated instructions.\n").unwrap();
    s.git(&remote, &["add", "."]);
    s.git(&remote, &["commit", "-m", "update"]);
    let next = ok(s.cmd().arg("sync").output().unwrap());
    assert_eq!(next["data"]["transport"], "pull");
    assert_eq!(
        fs::read_to_string(s.pool().join("example/SKILL.md")).unwrap(),
        "Updated instructions.\n"
    );
}

#[test]
fn sync_refuses_mismatched_remote_without_repointing_origin() {
    let s = Sandbox::new();
    let remote = s.upstream();
    s.config(json!({"library": s.pool(), "remote": remote}));
    ok(s.cmd().arg("sync").output().unwrap());
    let before = s.git(&s.pool(), &["remote", "get-url", "origin"]);
    let report = error(
        s.cmd()
            .arg("--pool-remote")
            .arg(s.home().join("different"))
            .arg("sync")
            .output()
            .unwrap(),
    );
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("origin differs"),
        "{report}"
    );
    assert_eq!(s.git(&s.pool(), &["remote", "get-url", "origin"]), before);
}

#[test]
fn sync_refuses_dirty_checkout_and_preserves_edits() {
    let s = Sandbox::new();
    let remote = s.upstream();
    s.config(json!({"library": s.pool(), "remote": remote}));
    ok(s.cmd().arg("sync").output().unwrap());
    fs::write(s.pool().join("example/SKILL.md"), "local edit\n").unwrap();
    let report = error(s.cmd().arg("sync").output().unwrap());
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("local changes"),
        "{report}"
    );
    assert_eq!(
        fs::read_to_string(s.pool().join("example/SKILL.md")).unwrap(),
        "local edit\n"
    );
}

#[test]
fn plain_folder_sync_only_renders_and_preserves_foreign_entries() {
    let s = Sandbox::new();
    skill(&s.pool(), "example");
    s.config(json!({"library": "pool"}));
    fs::create_dir_all(s.pool().join(".globals")).unwrap();
    fs::write(s.pool().join(".globals/sets"), "example main\n").unwrap();
    skill(&s.home().join(".claude/skills"), "example");
    let report = ok(s.cmd().arg("sync").output().unwrap());
    assert_eq!(report["data"]["transport"], "folder");
    assert!(
        !fs::symlink_metadata(s.home().join(".claude/skills/example"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(s.home().join(".codex/skills/example/SKILL.md").is_file());
}

#[test]
fn explicitly_configured_legacy_surface_and_sync_command_work() {
    let s = Sandbox::new();
    fs::create_dir_all(s.home().join(".claude-custom")).unwrap();
    let pool = s.home().join(".agents/skill-library");
    fs::create_dir_all(pool.join(".globals")).unwrap();
    fs::write(
        pool.join(".globals/harnesses"),
        "claude main {claude_shared}/skills claude surface\n",
    )
    .unwrap();
    let bin = s.home().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("skill-library-sync");
    fs::write(&script, "#!/bin/sh\necho legacy-hook\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    s.config(json!({"claude_home": "~/.claude-custom", "sync_command": script}));
    let report = ok(s.cmd().env("PATH", &bin).arg("sync").output().unwrap());
    assert_eq!(report["data"]["command"], script.to_str().unwrap());
    assert_eq!(
        report["meta"]["paths"]["claude_shared"],
        s.home().join(".claude-custom").to_str().unwrap()
    );
    assert!(
        report["data"]["output_tail"]
            .as_str()
            .unwrap()
            .contains("legacy-hook")
    );
}

#[test]
fn folder_commit_never_stages_an_enclosing_repository() {
    let s = Sandbox::new();
    s.git(s.home(), &["init", "-b", "main"]);
    skill(&s.pool(), "example");
    assert!(s.pool().join("example/SKILL.md").is_file());
    assert!(
        !skillherd::reconcile::git_commit(&s.pool(), "not a repo", &["example".to_owned()])
            .unwrap()
    );
    assert!(
        s.git(s.home(), &["diff", "--cached", "--name-only"])
            .is_empty()
    );
    assert!(!s.pool().join(".git").exists());
}

#[test]
fn configured_surface_roots_and_custom_sync_hook_work() {
    let s = Sandbox::new();
    let script = s.home().join("sync-hook");
    fs::write(&script, "#!/bin/sh\necho custom-hook\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    s.config(json!({"library": "pool", "claude_home": "alternate-claude", "codex_home": "alternate-codex", "sync_command": "~/sync-hook"}));
    let report = ok(s.cmd().arg("sync").output().unwrap());
    assert_eq!(report["data"]["command"], script.to_str().unwrap());
    assert_eq!(
        report["meta"]["paths"]["claude_shared"],
        s.home().join("alternate-claude").to_str().unwrap()
    );
    assert_eq!(
        report["meta"]["paths"]["codex_home"],
        s.home().join("alternate-codex").to_str().unwrap()
    );
}

#[test]
fn skills_shim_never_falls_back_to_live_home_for_mutations() {
    let s = Sandbox::new();
    let fake = s.home().join("fake-npx");
    let called = s.home().join("raw-called");
    fs::write(&fake, format!("#!/bin/sh\ntouch '{}'\n", called.display())).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let wrapper = Path::new(env!("CARGO_MANIFEST_DIR")).join("../bin/npx");
    for (binary, args) in [
        ("/nonexistent-skill", vec!["skills", "add", "owner/repo"]),
        ("true", vec!["skills", "add", "owner/repo", "--unknown"]),
    ] {
        let result = Command::new("bash")
            .arg(&wrapper)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", s.home())
            .env("SKILLHERD_BIN", binary)
            .env("SKILLHERD_REAL_NPX", &fake)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(!called.exists(), "raw npx must never receive live HOME");
    }
}

#[test]
fn folder_reconcile_imports_and_records_without_initializing_git() {
    let s = Sandbox::new();
    let inbox = s.inbox();
    skill(&inbox, "example");
    s.config(json!({"library": "pool"}));
    let report = ok(s.cmd().arg("reconcile").output().unwrap());
    assert_eq!(report["data"]["imported"][0]["name"], "example", "{report}");
    assert!(s.pool().join("example/SKILL.md").is_file());
    assert!(s.pool().join(".npx/example.json").is_file());
    assert!(!s.pool().join(".git").exists());
    assert!(!inbox.join("example").exists());
}

#[test]
fn npx_resolver_skips_a_wrapper_at_a_custom_prefix() {
    let s = Sandbox::new();
    let prefix = s.home().join("custom/bin");
    let real = s.home().join("node/bin");
    fs::create_dir_all(&prefix).unwrap();
    fs::create_dir_all(&real).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../bin/npx"),
        prefix.join("npx"),
    )
    .unwrap();
    fs::write(real.join("npx"), "#!/bin/sh\nexit 0\n").unwrap();
    let path = std::env::join_paths([&prefix, &real]).unwrap();
    assert_eq!(
        skillherd::paths::pick_npx_from_path(&path),
        Some(real.join("npx"))
    );
}

#[test]
fn skills_shim_refuses_ambiguous_mutations_before_the_verb() {
    let s = Sandbox::new();
    let fake = s.home().join("real-npx");
    let log = s.home().join("raw.log");
    fs::write(
        &fake,
        format!("#!/bin/sh\nprintf raw > '{}'\n", log.display()),
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let wrapper = Path::new(env!("CARGO_MANIFEST_DIR")).join("../bin/npx");
    for args in [
        vec!["-p", "skills", "skills", "add", "owner/repo"],
        vec!["skills", "-a", "claude-code", "add", "owner/repo"],
    ] {
        let output = Command::new(&wrapper)
            .args(&args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", s.home())
            .env("SKILLHERD_REAL_NPX", &fake)
            .env("SKILLHERD_BIN", "/usr/bin/true")
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "ambiguous syntax must be refused: {args:?}"
        );
        assert!(!log.exists(), "a skills mutation reached raw npx: {args:?}");
    }
}

#[test]
fn skills_shim_refuses_itself_or_another_wrapper_as_real_npx() {
    use std::time::{Duration, Instant};
    let s = Sandbox::new();
    let wrapper = Path::new(env!("CARGO_MANIFEST_DIR")).join("../bin/npx");
    let other = s.home().join("another-npx");
    fs::copy(&wrapper, &other).unwrap();
    for candidate in [&wrapper, &other] {
        let mut child = Command::new(&wrapper)
            .arg("--version")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", s.home())
            .env("SKILLHERD_REAL_NPX", candidate)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if start.elapsed() > Duration::from_secs(30) {
                child.kill().unwrap();
                child.wait().unwrap();
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.is_some(),
            "wrapper re-exec loop at {}",
            candidate.display()
        );
        assert_eq!(status.unwrap().code(), Some(127));
    }
}

#[test]
fn local_relative_and_home_remote_paths_survive_a_second_sync() {
    let s = Sandbox::new();
    let remote = s.upstream();
    for configured in ["upstream", "~/upstream"] {
        let pool = s.home().join(if configured.starts_with('~') {
            "tilde-pool"
        } else {
            "relative-pool"
        });
        s.config(json!({"library": pool, "remote": configured}));
        let first = ok(s.cmd().current_dir(s.home()).arg("sync").output().unwrap());
        assert_eq!(first["data"]["transport"], "clone");
        let next = ok(s.cmd().current_dir(s.home()).arg("sync").output().unwrap());
        assert_eq!(next["data"]["transport"], "pull");
        assert_eq!(
            s.git(&pool, &["remote", "get-url", "origin"]).trim(),
            remote.to_str().unwrap()
        );
    }
}

#[test]
fn remote_pool_mutators_require_first_sync_without_creating_the_pool() {
    let s = Sandbox::new();
    let remote = s.upstream();
    s.config(json!({"library": s.pool(), "remote": remote}));
    let report = error(s.cmd().arg("render").output().unwrap());
    assert_eq!(report["error"]["code"], "config_error");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("skillherd sync")
    );
    assert!(!s.pool().exists());
    let synced = ok(s.cmd().arg("sync").output().unwrap());
    assert_eq!(synced["data"]["transport"], "clone");
}

#[test]
fn folder_reconcile_ignores_enclosing_git_preflight_and_ignore_rules() {
    let s = Sandbox::new();
    let outer = s.home().join("outer");
    let pool = outer.join("pool");
    fs::create_dir_all(&pool).unwrap();
    fs::write(outer.join(".gitignore"), "ignored.txt\n").unwrap();
    fs::write(outer.join("unrelated"), "staged elsewhere\n").unwrap();
    s.git(&outer, &["init", "-b", "main"]);
    s.git(&outer, &["add", ".gitignore", "unrelated"]);
    let before = s.git(&outer, &["diff", "--cached"]);
    assert!(!before.is_empty());
    let inbox = s.inbox();
    skill(&inbox, "example");
    fs::write(
        inbox.join("example/ignored.txt"),
        "must count in a plain pool\n",
    )
    .unwrap();
    s.config(json!({"library": pool}));
    ok(s.cmd().arg("reconcile").output().unwrap());
    let record: Value =
        serde_json::from_slice(&fs::read(pool.join(".npx/example.json")).unwrap()).unwrap();
    let hash = skillherd::hash::tree_hash(&pool.join("example")).unwrap();
    assert_eq!(record["importedHash"], hash);
    assert_eq!(s.git(&outer, &["diff", "--cached"]), before);
    assert!(!pool.join(".git").exists());
}

#[test]
fn git_sync_and_import_ignore_another_repositories_git_environment() {
    let s = Sandbox::new();
    let remote = s.upstream();
    let other = s.home().join("other");
    fs::create_dir_all(&other).unwrap();
    s.git(&other, &["init", "-b", "main"]);
    fs::write(other.join("unrelated"), "must remain staged elsewhere\n").unwrap();
    s.git(&other, &["add", "unrelated"]);
    let before = s.git(&other, &["diff", "--cached"]);
    assert!(!before.is_empty());
    s.config(json!({"library": s.pool(), "remote": remote}));
    let run = |args: &[&str]| {
        ok(s.cmd()
            .args(args)
            .env("GIT_DIR", other.join(".git"))
            .env("GIT_WORK_TREE", &other)
            .env("GIT_INDEX_FILE", other.join(".git/index"))
            .env("GIT_AUTHOR_NAME", "Example")
            .env("GIT_AUTHOR_EMAIL", "you@example.com")
            .env("GIT_COMMITTER_NAME", "Example")
            .env("GIT_COMMITTER_EMAIL", "you@example.com")
            .output()
            .unwrap())
    };
    run(&["sync"]);
    run(&["sync"]);
    let inbox = s.inbox();
    skill(&inbox, "arrival");
    run(&["reconcile"]);
    assert!(s.pool().join("arrival/SKILL.md").is_file());
    assert!(
        s.git(&s.pool(), &["log", "-1", "--format=%s"])
            .trim()
            .starts_with("npx: import arrival (")
    );
    assert_eq!(s.git(&other, &["diff", "--cached"]), before);
    assert!(!other.join("arrival").exists());
}

#[test]
fn custom_sync_hook_receives_the_resolved_pool_and_remote() {
    let s = Sandbox::new();
    let hook = s.home().join("sync-hook");
    let log = s.home().join("hook.log");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$SKILLHERD_LIBRARY\" \"$SKILLHERD_POOL_REMOTE\" > '{}'\n",
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    s.config(json!({"library": "configured-pool", "remote": "https://example.com/pool.git", "sync_command": hook}));
    ok(s.cmd()
        .arg("--pool")
        .arg(s.pool())
        .arg("sync")
        .output()
        .unwrap());
    assert_eq!(
        fs::read_to_string(log).unwrap(),
        format!("{}\nhttps://example.com/pool.git\n", s.pool().display())
    );
}
