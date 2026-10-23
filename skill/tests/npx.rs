//! Explicit npx adapter: no installation or live agent configuration required.
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn fixture(script: &str) -> (TempDir, Command) {
    let dir = TempDir::new().unwrap();
    let real = dir.path().join("real-npx");
    fs::write(&real, script).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_skillherd"));
    cmd.env_clear()
        .env("HOME", dir.path())
        .env("PATH", "/usr/bin:/bin")
        .env("SKILLHERD_REAL_NPX", &real)
        .env("SKILLHERD_NPX", &real);
    (dir, cmd)
}

#[test]
fn explicit_adapter_preserves_raw_arguments_stdin_and_exit_status() {
    let (_dir, mut cmd) = fixture("#!/bin/sh\nprintf '<%s>\\n' \"$@\"\ncat\nexit 23\n");
    let mut child = cmd
        .args(["npx", "some-pkg", "--", "skills", "add", "a b"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"from stdin\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(23), "{output:?}");
    assert_eq!(
        output.stdout,
        b"<some-pkg>\n<-->\n<skills>\n<add>\n<a b>\nfrom stdin\n"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn adapter_routes_mutations_with_global_options_through_itself() {
    let (dir, mut cmd) = fixture("#!/bin/sh\necho RAW_MUST_NOT_RUN\nexit 99\n");
    let pool = dir.path().join("library");
    fs::create_dir_all(&pool).unwrap();
    let output = cmd
        .arg("--pool")
        .arg(&pool)
        .args([
            "--dry-run",
            "npx",
            "skills",
            "add",
            "owner/repo",
            "--skill",
            "example",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["meta"]["paths"]["library"], pool.to_str().unwrap());
    assert_eq!(envelope["data"]["plan"][0]["op"], "npx", "{envelope}");
    assert!(
        !pool.join(".npx").exists(),
        "dry-run must not write import state"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("owner/repo"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("RAW_MUST_NOT_RUN"));
}

#[test]
fn read_only_adapter_needs_no_valid_library_configuration() {
    let (dir, mut cmd) = fixture("#!/bin/sh\nprintf '<%s>\\n' \"$@\"\n");
    // If normal path resolution runs, this invalid configuration fails first.
    let config = dir.path().join(".config/skillherd");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("config.json"), "not json").unwrap();
    let output = cmd
        .args(["npx", "skills@9.9.9", "find", "a"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"<skills@9.9.9>\n<find>\n<a>\n");
}

#[test]
fn every_argument_after_npx_belongs_to_npx() {
    let (_dir, mut cmd) = fixture("#!/bin/sh\nprintf '<%s>\\n' \"$@\"\n");
    let output = cmd
        .args([
            "npx",
            "--correlation-id",
            "abc",
            "--pretty",
            "--pool",
            "X",
            "--dry-run",
        ])
        .output()
        .unwrap();
    // Unsupported npx syntax is refused by the router, never consumed as skill
    // flags (which previously produced a successful, argument-less npx call).
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unsupported npx option --correlation-id"),
        "{output:?}"
    );
    let (_dir, mut cmd) = fixture("#!/bin/sh\nprintf '<%s>\\n' \"$@\"\n");
    let output = cmd
        .args([
            "npx",
            "some-pkg",
            "--pretty",
            "--pool",
            "X",
            "--correlation-id",
            "abc",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout,
        b"<some-pkg>\n<--pretty>\n<--pool>\n<X>\n<--correlation-id>\n<abc>\n<--dry-run>\n"
    );
    let (_dir, mut cmd) = fixture("#!/bin/sh\nprintf '<%s>\\n' \"$@\"\n");
    let output = cmd.args(["npx", "--dry-run", "some-pkg"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"<--dry-run>\n<some-pkg>\n");
}

#[test]
fn configured_package_pin_is_shared_with_explicit_adapter() {
    let (dir, mut cmd) = fixture("#!/bin/sh\necho RAW_MUST_NOT_RUN\nexit 99\n");
    let pool = dir.path().join("library");
    fs::create_dir(&pool).unwrap();
    let output = cmd
        .env("SKILLHERD_NPX_PKG", "skills@2.0.0")
        .arg("--pool")
        .arg(&pool)
        .args(["--dry-run", "npx", "skills@2.0.0", "add", "owner/repo"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["data"]["plan"][0]["op"], "npx", "{envelope}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("skills@2.0.0"),
        "{envelope}"
    );
}

#[test]
fn dry_run_raw_adapter_refuses_instead_of_executing() {
    let (_dir, mut cmd) = fixture("#!/bin/sh\necho RAW_MUST_NOT_RUN\n");
    let output = cmd
        .args(["--dry-run", "npx", "--version"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("only supported for managed"),
        "{output:?}"
    );
}
