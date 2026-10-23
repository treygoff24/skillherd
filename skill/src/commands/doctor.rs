//! `skillherd doctor` — Diagnose the pool, the policy and the surfaces.

use serde_json::Value;

use crate::cli::DoctorArgs;
use crate::doctor;
use crate::error::{Error, Result};
use crate::output;
use crate::paths::Paths;

/// Diagnose, or print the contract when asked for capabilities.
pub fn run(args: &DoctorArgs, paths: &Paths, dry_run: bool) -> Result<Value> {
    if args.capabilities {
        if args.fix {
            return Err(Error::usage(
                "`--capabilities` prints the contract and changes nothing; drop `--fix`",
            ));
        }
        return serde_json::to_value(doctor::capabilities())
            .map_err(|err| Error::internal(format!("serializing the doctor contract: {err}")));
    }

    let report = doctor::run(paths, args.fix, dry_run)?;
    // Findings are not a failure: the report is the payload and the linter
    // status rides the exit code.
    output::request_exit_code(doctor::exit_code(&report));
    serde_json::to_value(&report)
        .map_err(|err| Error::internal(format!("serializing the doctor report: {err}")))
}
