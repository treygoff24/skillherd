//! The public error contract.
//!
//! One variant per code in the plan's dictionary, each mapping to an exit code
//! and carrying `details`, `retryable` and `suggested_fix`. Agents key off
//! `code`, `retryable` and `details`; `message` is for humans. Every variant is
//! constructed through a named constructor, so the message and the structured
//! details stay in sync.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The published exit-code dictionary.
pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_DOCTOR_FINDINGS: i32 = 1;
pub const EXIT_DOCTOR_PARTIAL_FIX: i32 = 2;
pub const EXIT_DOCTOR_FIX_FAILED: i32 = 3;
pub const EXIT_DOCTOR_REFUSED: i32 = 4;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_INVALID_DATA: i32 = 65;
pub const EXIT_INPUT_NOT_FOUND: i32 = 66;
pub const EXIT_INTERNAL: i32 = 70;
pub const EXIT_TEMPORARY: i32 = 75;
pub const EXIT_PERMISSION: i32 = 77;
pub const EXIT_CONFIG: i32 = 78;

/// The known harnesses, used in `suggested_fix` text.
const KNOWN_HARNESSES: &str =
    "claude, codex, agents, grok, pi, cursor (omp is an alias for agents)";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{message}")]
    NotImplemented {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    Usage {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    InvalidName {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    InputNotFound {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    ConfigError {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    LibraryBusy {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    LibraryDirtyUnrelated {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    LocalEditRefused {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    NpxFailed {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    SyncFailed {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    GitFailed {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    GithubAuthFailed {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    ReleaseFailed {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    ReleaseArchiveRejected {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    RenderFailed {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    PolicyBadToken {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    PolicyLaneNotInMain {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    PolicyUnknownHarness {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    PolicyUnrepresentable {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    /// Warning-class: it never fails a command on its own (`exit_code` is 0).
    /// Commands surface it through `meta.warnings` with `output::warn_error`.
    #[error("{message}")]
    VariantsUnavailable {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },

    #[error("{message}")]
    Internal {
        message: String,
        details: Value,
        suggested_fix: Option<String>,
    },
}

/// One row of the published code dictionary.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct CodeInfo {
    pub code: String,
    pub exit_code: i32,
    pub retryable: bool,
    pub warning_class: bool,
}

impl Error {
    /// The stable machine code an agent branches on.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotImplemented { .. } => "not_implemented",
            Self::Usage { .. } => "usage",
            Self::InvalidName { .. } => "invalid_name",
            Self::InputNotFound { .. } => "input_not_found",
            Self::ConfigError { .. } => "config_error",
            Self::LibraryBusy { .. } => "library_busy",
            Self::LibraryDirtyUnrelated { .. } => "library_dirty_unrelated",
            Self::LocalEditRefused { .. } => "local_edit_refused",
            Self::NpxFailed { .. } => "npx_failed",
            Self::SyncFailed { .. } => "sync_failed",
            Self::GitFailed { .. } => "git_failed",
            Self::GithubAuthFailed { .. } => "github_auth_failed",
            Self::ReleaseFailed { .. } => "release_failed",
            Self::ReleaseArchiveRejected { .. } => "release_archive_rejected",
            Self::RenderFailed { .. } => "render_failed",
            Self::PolicyBadToken { .. } => "policy_bad_token",
            Self::PolicyLaneNotInMain { .. } => "policy_lane_not_in_main",
            Self::PolicyUnknownHarness { .. } => "policy_unknown_harness",
            Self::PolicyUnrepresentable { .. } => "policy_unrepresentable",
            Self::VariantsUnavailable { .. } => "variants_unavailable",
            Self::Internal { .. } => "internal",
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotImplemented { .. } | Self::GitFailed { .. } | Self::Internal { .. } => {
                EXIT_INTERNAL
            }
            Self::Usage { .. } => EXIT_USAGE,
            Self::InvalidName { .. }
            | Self::LibraryDirtyUnrelated { .. }
            | Self::LocalEditRefused { .. }
            | Self::ReleaseArchiveRejected { .. }
            | Self::RenderFailed { .. }
            | Self::PolicyBadToken { .. }
            | Self::PolicyLaneNotInMain { .. }
            | Self::PolicyUnknownHarness { .. }
            | Self::PolicyUnrepresentable { .. } => EXIT_INVALID_DATA,
            Self::InputNotFound { .. } => EXIT_INPUT_NOT_FOUND,
            Self::ConfigError { .. } => EXIT_CONFIG,
            Self::GithubAuthFailed { .. } => EXIT_PERMISSION,
            Self::LibraryBusy { .. }
            | Self::NpxFailed { .. }
            | Self::SyncFailed { .. }
            | Self::ReleaseFailed { .. } => EXIT_TEMPORARY,
            Self::VariantsUnavailable { .. } => EXIT_SUCCESS,
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::LibraryBusy { .. }
                | Self::NpxFailed { .. }
                | Self::SyncFailed { .. }
                | Self::ReleaseFailed { .. }
                | Self::VariantsUnavailable { .. }
        )
    }

    /// True for the warning-class code: commands report it and carry on.
    pub fn is_warning(&self) -> bool {
        matches!(self, Self::VariantsUnavailable { .. })
    }

    pub fn details(&self) -> &Value {
        match self {
            Self::NotImplemented { details, .. }
            | Self::Usage { details, .. }
            | Self::InvalidName { details, .. }
            | Self::InputNotFound { details, .. }
            | Self::ConfigError { details, .. }
            | Self::LibraryBusy { details, .. }
            | Self::LibraryDirtyUnrelated { details, .. }
            | Self::LocalEditRefused { details, .. }
            | Self::NpxFailed { details, .. }
            | Self::SyncFailed { details, .. }
            | Self::GitFailed { details, .. }
            | Self::GithubAuthFailed { details, .. }
            | Self::ReleaseFailed { details, .. }
            | Self::ReleaseArchiveRejected { details, .. }
            | Self::RenderFailed { details, .. }
            | Self::PolicyBadToken { details, .. }
            | Self::PolicyLaneNotInMain { details, .. }
            | Self::PolicyUnknownHarness { details, .. }
            | Self::PolicyUnrepresentable { details, .. }
            | Self::VariantsUnavailable { details, .. }
            | Self::Internal { details, .. } => details,
        }
    }
    fn details_mut(&mut self) -> &mut Value {
        match self {
            Self::NotImplemented { details, .. }
            | Self::Usage { details, .. }
            | Self::InvalidName { details, .. }
            | Self::InputNotFound { details, .. }
            | Self::ConfigError { details, .. }
            | Self::LibraryBusy { details, .. }
            | Self::LibraryDirtyUnrelated { details, .. }
            | Self::LocalEditRefused { details, .. }
            | Self::NpxFailed { details, .. }
            | Self::SyncFailed { details, .. }
            | Self::GitFailed { details, .. }
            | Self::GithubAuthFailed { details, .. }
            | Self::ReleaseFailed { details, .. }
            | Self::ReleaseArchiveRejected { details, .. }
            | Self::RenderFailed { details, .. }
            | Self::PolicyBadToken { details, .. }
            | Self::PolicyLaneNotInMain { details, .. }
            | Self::PolicyUnknownHarness { details, .. }
            | Self::PolicyUnrepresentable { details, .. }
            | Self::VariantsUnavailable { details, .. }
            | Self::Internal { details, .. } => details,
        }
    }

    /// The same error with one more key in its details.
    pub fn with_detail(mut self, key: &str, value: Value) -> Self {
        if let Some(object) = self.details_mut().as_object_mut() {
            object.insert(key.to_string(), value);
        }
        self
    }

    pub fn suggested_fix(&self) -> Option<&str> {
        match self {
            Self::NotImplemented { suggested_fix, .. }
            | Self::Usage { suggested_fix, .. }
            | Self::InvalidName { suggested_fix, .. }
            | Self::InputNotFound { suggested_fix, .. }
            | Self::ConfigError { suggested_fix, .. }
            | Self::LibraryBusy { suggested_fix, .. }
            | Self::LibraryDirtyUnrelated { suggested_fix, .. }
            | Self::LocalEditRefused { suggested_fix, .. }
            | Self::NpxFailed { suggested_fix, .. }
            | Self::SyncFailed { suggested_fix, .. }
            | Self::GitFailed { suggested_fix, .. }
            | Self::GithubAuthFailed { suggested_fix, .. }
            | Self::ReleaseFailed { suggested_fix, .. }
            | Self::ReleaseArchiveRejected { suggested_fix, .. }
            | Self::RenderFailed { suggested_fix, .. }
            | Self::PolicyBadToken { suggested_fix, .. }
            | Self::PolicyLaneNotInMain { suggested_fix, .. }
            | Self::PolicyUnknownHarness { suggested_fix, .. }
            | Self::PolicyUnrepresentable { suggested_fix, .. }
            | Self::VariantsUnavailable { suggested_fix, .. }
            | Self::Internal { suggested_fix, .. } => suggested_fix.as_deref(),
        }
    }

    /// The published dictionary, in the order the plan names the codes.
    pub fn dictionary() -> Vec<CodeInfo> {
        let rows: [(&str, i32, bool, bool); 21] = [
            ("usage", EXIT_USAGE, false, false),
            ("invalid_name", EXIT_INVALID_DATA, false, false),
            ("input_not_found", EXIT_INPUT_NOT_FOUND, false, false),
            ("config_error", EXIT_CONFIG, false, false),
            ("library_busy", EXIT_TEMPORARY, true, false),
            ("library_dirty_unrelated", EXIT_INVALID_DATA, false, false),
            ("local_edit_refused", EXIT_INVALID_DATA, false, false),
            ("npx_failed", EXIT_TEMPORARY, true, false),
            ("sync_failed", EXIT_TEMPORARY, true, false),
            ("git_failed", EXIT_INTERNAL, false, false),
            ("github_auth_failed", EXIT_PERMISSION, false, false),
            ("release_failed", EXIT_TEMPORARY, true, false),
            ("release_archive_rejected", EXIT_INVALID_DATA, false, false),
            ("render_failed", EXIT_INVALID_DATA, false, false),
            ("policy_bad_token", EXIT_INVALID_DATA, false, false),
            ("policy_lane_not_in_main", EXIT_INVALID_DATA, false, false),
            ("policy_unknown_harness", EXIT_INVALID_DATA, false, false),
            ("policy_unrepresentable", EXIT_INVALID_DATA, false, false),
            ("variants_unavailable", EXIT_SUCCESS, true, true),
            ("internal", EXIT_INTERNAL, false, false),
            ("not_implemented", EXIT_INTERNAL, false, false),
        ];
        rows.iter()
            .map(|(code, exit_code, retryable, warning_class)| CodeInfo {
                code: (*code).to_string(),
                exit_code: *exit_code,
                retryable: *retryable,
                warning_class: *warning_class,
            })
            .collect()
    }

    // ---- constructors -------------------------------------------------

    pub fn not_implemented(what: &str) -> Self {
        Self::NotImplemented {
            message: format!("{what} is not implemented in this build"),
            details: json!({ "command": what }),
            suggested_fix: None,
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::Usage {
            message: message.into(),
            details: Value::Null,
            suggested_fix: Some(
                "run `skillherd schema input` for the full command contract".into(),
            ),
        }
    }

    pub fn invalid_name(name: &str, reason: &str) -> Self {
        Self::InvalidName {
            message: format!("invalid skill name {name:?}: {reason}"),
            details: json!({ "name": name, "reason": reason }),
            suggested_fix: Some(
                "a skill name is one path component matching ^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$ and never starts with '.'"
                    .into(),
            ),
        }
    }

    pub fn input_not_found(path: &Path) -> Self {
        Self::input_not_found_reason(path, "does not exist")
    }

    pub fn input_not_found_reason(path: &Path, reason: &str) -> Self {
        let shown = path.display().to_string();
        Self::InputNotFound {
            message: format!("{shown} {reason}"),
            details: json!({ "path": shown, "reason": reason }),
            suggested_fix: Some(
                "check the path, or list what exists with `skillherd list --all`".into(),
            ),
        }
    }

    pub fn config_error(message: impl Into<String>) -> Self {
        Self::ConfigError {
            message: message.into(),
            details: Value::Null,
            suggested_fix: Some(
                "fix the named path or environment variable and rerun; `skillherd doctor` reports the resolved paths"
                    .into(),
            ),
        }
    }

    pub fn library_busy(lock_path: &Path) -> Self {
        let shown = lock_path.display().to_string();
        Self::LibraryBusy {
            message: format!("the library is locked by another skill mutator: {shown}"),
            details: json!({ "lock": shown }),
            suggested_fix: Some(
                "wait for the other mutator to finish and retry; the lock is only held while it writes"
                    .into(),
            ),
        }
    }

    pub fn library_dirty_unrelated(message: impl Into<String>, paths: Vec<String>) -> Self {
        Self::LibraryDirtyUnrelated {
            message: message.into(),
            details: json!({ "paths": paths }),
            suggested_fix: Some(
                "commit or stash the unrelated library changes, then rerun `skillherd reconcile`"
                    .into(),
            ),
        }
    }

    pub fn local_edit_refused(name: &str, reason: &str) -> Self {
        Self::LocalEditRefused {
            message: format!("{name} was edited locally and will not be overwritten: {reason}"),
            details: json!({ "name": name, "reason": reason }),
            suggested_fix: Some(format!(
                "fork it under a distinct name per the house rule, or pass `--adopt {name}` to take upstream / `--keep {name}` to keep the house copy"
            )),
        }
    }

    /// `skill-library-sync` exited non-zero; the script's own output says why.
    pub fn sync_failed(exit_code: i32, output_tail: &str) -> Self {
        let first = output_tail.lines().next().unwrap_or("").trim().to_string();
        Self::SyncFailed {
            message: format!("sync command exited {exit_code}: {first}"),
            details: json!({
                "exit_code": exit_code,
                "output_tail": output_tail,
            }),
            suggested_fix: Some(
                "read the sync output above (a dirty tree or a rebase conflict is resolved by hand), then run `skillherd sync` again"
                    .into(),
            ),
        }
    }

    pub fn npx_failed(argv: &[String], exit_code: i32, output_tail: &str) -> Self {
        let auth = [
            "permission denied (publickey)",
            "could not read username",
            "authentication failed",
            "repository not found",
            "401",
            "403",
        ]
        .iter()
        .any(|needle| output_tail.to_ascii_lowercase().contains(needle));
        let argv: Vec<String> = argv.iter().map(|arg| crate::npx::redact(arg)).collect();
        let output_tail = crate::npx::redact(output_tail);
        let first = output_tail.lines().next().unwrap_or("").trim().to_string();
        Self::NpxFailed {
            message: format!("npx exited {exit_code}: {first}"),
            details: json!({
                "argv": argv,
                "exit_code": exit_code,
                "output_tail": output_tail,
            }),
            suggested_fix: Some(if auth {
                "authentication failed; for a private skill rerun with SKILLHERD_NPX_CREDENTIALS=1 to pass your credential environment and user git/npm config (the child is redirected, not sandboxed)".into()
            } else {
                "retry once the network or npm registry recovers; `skillherd doctor` shows the resolved npx path".into()
            }),
        }
    }

    pub fn git_failed(message: impl Into<String>, output: &str) -> Self {
        Self::GitFailed {
            message: message.into(),
            details: json!({ "git_output": output }),
            suggested_fix: Some(
                "inspect the library with `git -C <library> status`; the imported tree is left in place so a rerun finishes the import"
                    .into(),
            ),
        }
    }

    /// GitHub refused the request or no credentials were available: the
    /// anonymous API rate limit is exactly what this guards against, so a
    /// release fetch never falls back to it.
    pub fn github_auth_failed(repo: &str, reason: &str) -> Self {
        Self::GithubAuthFailed {
            message: format!("GitHub refused access to {repo}: {reason}"),
            details: json!({ "repo": repo, "reason": reason }),
            suggested_fix: Some(
                "run `gh auth login` (or set GH_TOKEN/GITHUB_TOKEN in the environment), then rerun; `gh auth status` shows the active login"
                    .into(),
            ),
        }
    }

    /// Fetching a release listing or asset failed for a reason other than auth.
    pub fn release_failed(repo: &str, reason: &str) -> Self {
        Self::ReleaseFailed {
            message: format!("fetching a release of {repo} failed: {reason}"),
            details: json!({ "repo": repo, "reason": reason }),
            suggested_fix: Some(
                "retry once GitHub is reachable; nothing in the pool changed".into(),
            ),
        }
    }

    /// No release (or no asset, or no skill directory in it) matches the record.
    pub fn release_not_found(repo: &str, reason: &str) -> Self {
        Self::InputNotFound {
            message: format!("{repo}: {reason}"),
            details: json!({ "repo": repo, "reason": reason }),
            suggested_fix: Some(
                "check the repo, --release-asset, --tag-prefix and --release-dir against the repository's releases page"
                    .into(),
            ),
        }
    }

    /// A release zip carries an entry that could land outside its extraction
    /// directory (absolute, `..`, a symlink) or is not a plain file or dir.
    pub fn release_archive_rejected(asset: &str, entry: &str, reason: &str) -> Self {
        Self::ReleaseArchiveRejected {
            message: format!("{asset}: entry {entry:?} was rejected: {reason}"),
            details: json!({ "asset": asset, "entry": entry, "reason": reason }),
            suggested_fix: Some(
                "nothing was extracted into the pool; inspect the asset by hand before trusting this upstream"
                    .into(),
            ),
        }
    }

    pub fn render_failed(surface: &Path, errors: &[(String, String)]) -> Self {
        let shown = surface.display().to_string();
        let names: Vec<&str> = errors.iter().map(|(name, _)| name.as_str()).collect();
        Self::RenderFailed {
            message: format!(
                "the stripper failed for {} entr{} on {shown}: {}",
                errors.len(),
                if errors.len() == 1 { "y" } else { "ies" },
                names.join(", ")
            ),
            details: json!({
                "surface": shown,
                "errors": errors.iter().map(|(name, message)| json!({ "name": name, "message": message })).collect::<Vec<_>>(),
            }),
            suggested_fix: Some(
                "fix the reported files (a raw fenced skill left visible is a portability failure), then rerun `skillherd render`"
                    .into(),
            ),
        }
    }

    pub fn policy_bad_token(token: &str, line: usize) -> Self {
        Self::PolicyBadToken {
            message: format!("unknown policy token {token:?} on sets line {line}"),
            details: json!({ "token": token, "line": line }),
            suggested_fix: Some(
                "tokens are `main`, `lane`, `!mac`, `!linux` (legacy `!cell`), `only:<harness>` and `not:<harness>`"
                    .into(),
            ),
        }
    }

    pub fn policy_lane_not_in_main(name: &str, line: usize) -> Self {
        Self::PolicyLaneNotInMain {
            message: format!("sets line {line} gives {name:?} the `lane` token without `main`"),
            details: json!({ "name": name, "line": line }),
            suggested_fix: Some(format!(
                "add the `main` token to the row (`lane` implies `main`), or run `skillherd on {name} --tier lane` which writes both"
            )),
        }
    }

    pub fn policy_unknown_harness(harness: &str, line: Option<usize>) -> Self {
        let where_ = match line {
            Some(line) => format!("on sets line {line}"),
            None => "on the command line".to_string(),
        };
        Self::PolicyUnknownHarness {
            message: format!("unknown harness {harness:?} {where_}"),
            details: json!({ "harness": harness, "line": line }),
            suggested_fix: Some(format!("known harnesses: {KNOWN_HARNESSES}")),
        }
    }

    pub fn policy_unrepresentable(name: &str, reason: &str) -> Self {
        Self::PolicyUnrepresentable {
            message: format!("policy row {name:?} cannot be represented: {reason}"),
            details: json!({ "name": name, "reason": reason }),
            suggested_fix: Some(format!(
                "drop the token from {name}, or give the harness its own surface in .globals/harnesses; `only:` is only valid for a harness with its own surface ({KNOWN_HARNESSES})"
            )),
        }
    }

    pub fn variants_unavailable(name: &str, reason: &str) -> Self {
        Self::VariantsUnavailable {
            message: format!("upstream variants for {name} are unavailable: {reason}"),
            details: json!({ "name": name, "reason": reason }),
            suggested_fix: Some(
                "reconnect and rerun `skillherd reconcile`; pending variants are retried on every reconcile"
                    .into(),
            ),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
            details: Value::Null,
            suggested_fix: Some(
                "rerun with the same arguments; if it repeats, report the command and the correlation_id"
                    .into(),
            ),
        }
    }

    pub fn io(context: &str, source: &std::io::Error) -> Self {
        Self::Internal {
            message: format!("{context}: {source}"),
            details: json!({ "kind": format!("{:?}", source.kind()), "message": source.to_string() }),
            suggested_fix: Some(
                "check the path exists and is writable; `skillherd doctor` reports the resolved paths"
                    .into(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<Error> {
        vec![
            Error::not_implemented("list"),
            Error::usage("bad flag"),
            Error::invalid_name("bad/name", "contains '/'"),
            Error::input_not_found(Path::new("/nope")),
            Error::config_error("HOME is not set"),
            Error::library_busy(Path::new("/l/.skill.lock")),
            Error::library_dirty_unrelated("dirty", vec!["a".into()]),
            Error::local_edit_refused("n", "hash differs"),
            Error::npx_failed(&["npx".into()], 1, "boom\nmore"),
            Error::sync_failed(1, "refusing: dirty tree\nmore"),
            Error::git_failed("commit failed", "no identity"),
            Error::github_auth_failed("o/r", "HTTP 403"),
            Error::release_failed("o/r", "timeout"),
            Error::release_archive_rejected("a.zip", "../x", "parent component"),
            Error::render_failed(Path::new("/s"), &[("n".into(), "m".into())]),
            Error::policy_bad_token("bogus", 3),
            Error::policy_lane_not_in_main("n", 3),
            Error::policy_unknown_harness("nope", Some(3)),
            Error::policy_unrepresentable("n", "shared dir"),
            Error::variants_unavailable("n", "offline"),
            Error::internal("boom"),
        ]
    }

    /// The dictionary is the contract; every variant must agree with its row.
    #[test]
    fn every_variant_matches_the_dictionary() {
        let dictionary = Error::dictionary();
        assert_eq!(dictionary.len(), samples().len());
        for err in samples() {
            let row = dictionary
                .iter()
                .find(|row| row.code == err.code())
                .unwrap_or_else(|| panic!("{} is missing from the dictionary", err.code()));
            assert_eq!(
                row.exit_code,
                err.exit_code(),
                "exit code for {}",
                err.code()
            );
            assert_eq!(
                row.retryable,
                err.retryable(),
                "retryable for {}",
                err.code()
            );
            assert_eq!(
                row.warning_class,
                err.is_warning(),
                "warning class for {}",
                err.code()
            );
        }
    }

    #[test]
    fn codes_are_unique() {
        let dictionary = Error::dictionary();
        let mut codes: Vec<&str> = dictionary.iter().map(|row| row.code.as_str()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), dictionary.len());
    }

    #[test]
    fn messages_name_the_offending_input() {
        assert!(
            Error::invalid_name("../x", "starts with '.'")
                .to_string()
                .contains("../x")
        );
        assert!(
            Error::input_not_found(Path::new("/no/such"))
                .to_string()
                .contains("/no/such")
        );
        assert!(
            Error::library_busy(Path::new("/lib/.skill.lock"))
                .to_string()
                .contains("/lib/.skill.lock")
        );
        assert!(
            Error::policy_bad_token("meh", 7)
                .to_string()
                .contains("line 7")
        );
        assert!(
            Error::local_edit_refused("foo", "hash differs")
                .suggested_fix()
                .unwrap()
                .contains("--adopt foo")
        );
    }

    #[test]
    fn warning_class_does_not_fail() {
        let warning = Error::variants_unavailable("foo", "offline");
        assert_eq!(warning.exit_code(), 0);
        assert!(warning.is_warning());
        assert!(warning.details()["reason"] == "offline");
    }
}
