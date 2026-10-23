//! Explicit adapter for the optional shim. One embedded script owns routing.
//!
//! Raw npx is deliberately not a JSON command: exec preserves its streams and
//! exit status, and never resolves library configuration for unrelated tools.
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::process::Command;

use clap::Args;

use crate::cli::GlobalArgs;
use crate::error::{Error, Result};

#[derive(Debug, Args)]
pub struct NpxArgs {
    /// Arguments to npx, unchanged except for supported skills mutations.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<OsString>,
}

const SHIM: &str = include_str!("../../scripts/npx");
const PREFIX: &str = r#"
prefix_count=$1
shift
skill_prefix=()
while [ "$prefix_count" -gt 0 ]; do
  skill_prefix+=("$1")
  shift
  prefix_count=$((prefix_count - 1))
done
"#;

/// Replace the process with the shared router. Only an exec failure returns.
pub fn run(args: &NpxArgs, global: &GlobalArgs) -> Result<serde_json::Value> {
    let mut prefix: Vec<OsString> = Vec::new();
    if global.pretty {
        prefix.push("--pretty".into());
    }
    if global.dry_run {
        prefix.push("--dry-run".into());
    }
    if let Some(id) = &global.correlation_id {
        prefix.extend([OsString::from("--correlation-id"), id.into()]);
    }
    if let Some(pool) = &global.pool {
        prefix.extend([OsString::from("--pool"), pool.as_os_str().to_owned()]);
    }
    if let Some(remote) = &global.pool_remote {
        prefix.extend([OsString::from("--pool-remote"), remote.into()]);
    }
    let executable =
        std::env::current_exe().map_err(|err| Error::io("finding skillherd executable", &err))?;
    let err = Command::new("bash")
        .args(["-c", &format!("{PREFIX}\n{SHIM}"), "skillherd npx"])
        .arg(prefix.len().to_string())
        .args(prefix)
        .args(&args.args)
        .env("SKILLHERD_BIN", executable)
        .env(
            "SKILLHERD_NPX_PKG",
            std::env::var_os("SKILLHERD_NPX_PKG")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| crate::paths::DEFAULT_NPX_PKG.into()),
        )
        .env(
            "SKILLHERD_WRAPPER_DRY_RUN",
            if global.dry_run { "1" } else { "0" },
        )
        .exec();
    Err(Error::io(
        "executing the npx adapter (requires bash on PATH)",
        &err,
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_shim_default_matches_importer_pin() {
        let default = super::SHIM
            .split("${SKILLHERD_NPX_PKG:-")
            .nth(1)
            .expect("shim must use the configurable importer pin")
            .split('}')
            .next()
            .unwrap();
        assert_eq!(default, crate::paths::DEFAULT_NPX_PKG);
    }
}
