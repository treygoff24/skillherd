//! `skillherd sync`: optional git transport, then render; legacy hooks are opt-in.

use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, JsonSchema)]
pub struct PoolSyncReport {
    pub command: String,
    pub exit_code: i32,
    pub output_tail: String,
    pub transport: String,
    pub render: render::RenderReport,
}

use crate::cli::SyncArgs;
use crate::error::{Error, Result};
use crate::paths::Paths;
use crate::{fsx, lockfile, render};

/// Longest tail [`output_tail`] returns, so one chatty script cannot flood a
/// caller's context.
const MAX_TAIL_CHARS: usize = 4000;

/// The `skillherd sync` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncReport {
    pub command: String,
    pub exit_code: i32,
    pub output_tail: String,
}

/// The last lines of a command's output, for a payload or an error detail.
pub fn output_tail(text: &str) -> String {
    let mut lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.len() > 20 {
        lines = lines.split_off(lines.len() - 20);
    }
    let mut tail = lines.join("\n");
    if tail.len() > MAX_TAIL_CHARS {
        let mut start = tail.len() - MAX_TAIL_CHARS;
        while start < tail.len() && !tail.is_char_boundary(start) {
            start += 1;
        }
        tail = tail[start..].to_string();
    }
    tail
}

/// Print a child process's diagnostics, but only to a human's terminal: an
/// agent reads the JSON envelope, and a pipe must stay one document.
pub fn forward_when_tty(text: &str) {
    if !text.trim().is_empty() && std::io::stderr().is_terminal() {
        eprint!("{text}");
        if !text.ends_with('\n') {
            eprintln!();
        }
    }
}

pub fn run(_args: &SyncArgs, paths: &Paths, dry_run: bool) -> Result<Value> {
    if dry_run {
        return Err(Error::usage(
            "sync has no dry run; use `render --dry-run` to preview surfaces",
        ));
    }
    if let Some(command) = &paths.sync_command {
        return run_script(command, paths);
    }
    let mut transport = "folder";
    if let Some(remote) = &paths.pool_remote {
        if remote.trim().is_empty() || remote.starts_with('-') {
            return Err(Error::config_error(
                "pool remote must be a non-empty git URL or path, not an option",
            ));
        }
        if fs::symlink_metadata(&paths.library)
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            clone_pool(paths, remote)?;
            transport = "clone";
        } else {
            let _guard = lockfile::lock_library(paths)?;
            if !paths.library.join(".git").exists() {
                return Err(Error::config_error(
                    "remote configured but pool is not a git checkout; choose an absent path to clone, or unset the remote for a plain folder",
                ));
            }
            let origin = git(&paths.library, &["remote", "get-url", "origin"])?;
            if origin.trim() != remote {
                return Err(Error::config_error(
                    "pool origin differs from configured remote; refusing to change it",
                ));
            }
            let dirty = git(
                &paths.library,
                &["status", "--porcelain", "--untracked-files=no"],
            )?;
            if !dirty.trim().is_empty() {
                return Err(Error::config_error(
                    "pool has tracked local changes; commit or stash them before sync",
                ));
            }
            git(
                &paths.library,
                &["pull", "--no-rebase", "--ff-only", "origin"],
            )?;
            transport = "pull";
        }
    }
    let report = render::all_surfaces(paths, false)?;
    serde_json::to_value(PoolSyncReport {
        command: "sync".into(),
        exit_code: 0,
        output_tail: String::new(),
        transport: transport.into(),
        render: report,
    })
    .map_err(|err| Error::internal(format!("serializing the pool sync report: {err}")))
}

fn run_script(script: &Path, paths: &Paths) -> Result<Value> {
    let mut command = Command::new(script);
    command.env("SKILLHERD_LIBRARY", &paths.library);
    if let Some(remote) = &paths.pool_remote {
        command.env("SKILLHERD_POOL_REMOTE", remote);
    }
    let output = command.output().map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => Error::config_error(format!(
            "{} is not on PATH; set sync_command to your transport executable, or configure a remote and use the built-in sync",
            script.display()
        )),
        _ => Error::io(&format!("running {}", script.display()), &err),
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    forward_when_tty(&stderr);
    let tail = output_tail(&format!("{stdout}{stderr}"));
    if !output.status.success() {
        return Err(Error::sync_failed(output.status.code().unwrap_or(1), &tail));
    }
    serde_json::to_value(SyncReport {
        command: script.display().to_string(),
        exit_code: 0,
        output_tail: tail,
    })
    .map_err(|err| Error::internal(format!("serializing the sync report: {err}")))
}

fn git(pool: &Path, args: &[&str]) -> Result<String> {
    let output = crate::git_command()
        .arg("-C")
        .arg(pool)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|err| Error::io("running git sync", &err))?;
    if !output.status.success() {
        return Err(Error::sync_failed(
            output.status.code().unwrap_or(1),
            &output_tail(&String::from_utf8_lossy(&output.stderr)),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn clone_pool(paths: &Paths, remote: &str) -> Result<()> {
    let parent = paths
        .library
        .parent()
        .ok_or_else(|| Error::config_error("pool needs a parent directory"))?;
    fs::create_dir_all(parent).map_err(|err| Error::io("creating pool parent", &err))?;
    let scratch = unique_scratch(parent)?;
    let clone = scratch.join("pool");
    let result = (|| {
        let output = crate::git_command()
            .args(["clone", "--", remote])
            .arg(&clone)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|err| Error::io("cloning pool", &err))?;
        if !output.status.success() {
            return Err(Error::sync_failed(
                output.status.code().unwrap_or(1),
                &output_tail(&String::from_utf8_lossy(&output.stderr)),
            ));
        }
        // Never replace an entry another sync (or the user) created meanwhile.
        fsx::rename_noreplace(&clone, &paths.library)
            .map_err(|err| Error::io("publishing cloned pool without replacement", &err))
    })();
    // Only this invocation's private clone scratch is eligible for cleanup.
    let _ = fs::remove_dir_all(&scratch);
    result
}

fn unique_scratch(parent: &Path) -> Result<PathBuf> {
    for attempt in 0..1000 {
        let path = parent.join(format!(".skill-clone-{}-{attempt}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(Error::io("creating clone scratch", &err)),
        }
    }
    Err(Error::config_error("could not allocate clone scratch"))
}
