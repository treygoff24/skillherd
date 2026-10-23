//! `skillherd reconcile` — Import the npx inbox into the pool, then render.

use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::ReconcileArgs;
use crate::error::{Error, Result};
use crate::names::validate_name;
use crate::paths::Paths;
use crate::reconcile::{self, ReconcileOpts, ReconcileReport};

/// The payload `reconcile` reports, and the shape `install`/`update` extend.
#[derive(Debug, Serialize)]
pub struct Data {
    #[serde(flatten)]
    pub report: ReconcileReport,
}

/// Which command ran the reconcile, so a refusal names the retry that can
/// reach the arrival: a staged `install`/`update` keeps its arrival in its own
/// stage, which `skillherd reconcile` can also recover.
#[derive(Debug, Clone, Copy)]
pub enum Via<'a> {
    Reconcile,
    Update,
    Install { source: &'a str },
}

/// `--adopt`/`--keep` name valid skills and never the same one.
pub fn check_resolutions(adopt: &[String], keep: &[String]) -> Result<()> {
    for name in adopt.iter().chain(keep.iter()) {
        validate_name(name)?;
    }
    for name in adopt {
        if keep.contains(name) {
            return Err(Error::usage(format!(
                "--adopt and --keep disagree about {name}: adopt takes upstream, keep stays on the house copy"
            )));
        }
    }
    Ok(())
}

/// Import the inbox (or the named roots) into the pool and render.
pub fn run(args: &ReconcileArgs, paths: &Paths, dry_run: bool) -> Result<Value> {
    check_resolutions(&args.adopt, &args.keep)?;
    if !args.discard.is_empty() {
        if !args.adopt.is_empty() || !args.keep.is_empty() || !args.restore.is_empty() {
            return Err(Error::usage(
                "--discard cannot be combined with --adopt, --keep or --restore",
            ));
        }
        return serde_json::to_value(Data {
            report: reconcile::discard(paths, &args.discard, dry_run)?,
        })
        .map_err(|err| Error::internal(format!("serializing discard report: {err}")));
    }

    let report = reconcile::run(
        paths,
        ReconcileOpts {
            dry_run,
            adopt: args.adopt.clone(),
            keep: args.keep.clone(),
            restore: args.restore.clone(),
            inbox: Vec::new(),
            fetched: Default::default(),
        },
    )?;
    let refused = refusal_error(&report, Via::Reconcile);
    let data = Data { report };
    let value = serde_json::to_value(&data)
        .map_err(|err| Error::internal(format!("serializing the reconcile report: {err}")))?;
    match refused {
        Some(error) => Err(error),
        None => Ok(value),
    }
}

/// Turn recorded refusals into the command's exit-65 error, keeping the whole
/// report in `details` so the imports that did land stay visible.
pub fn refusal_error(report: &ReconcileReport, via: Via<'_>) -> Option<Error> {
    if report.refused.is_empty() {
        return None;
    }
    let names: Vec<&str> = report
        .refused
        .iter()
        .map(|refusal| refusal.name.as_str())
        .collect();
    Some(Error::LocalEditRefused {
        message: format!(
            "{} name(s) were left alone (the rest of the batch was imported): {}",
            report.refused.len(),
            names.join(", ")
        ),
        details: json!({
            "refusals": report.refused,
            "imported": report.imported,
        }),
        suggested_fix: Some(suggested_fix(report, via)),
    })
}

/// The remedy for what was actually refused: `--adopt`/`--keep` resolve a
/// local edit and nothing else, a displaced copy has `--restore`, and an
/// occupied backup path is cleared by hand. Advice that names a flag which
/// cannot clear the condition is worse than none.
fn suggested_fix(report: &ReconcileReport, via: Via<'_>) -> String {
    let names = |code: &str| -> Vec<&str> {
        report
            .refused
            .iter()
            .filter(|refusal| refusal.code == code)
            .map(|refusal| refusal.name.as_str())
            .collect()
    };
    let mut parts = Vec::new();
    let edited = names("local_edit_refused");
    if !edited.is_empty() {
        let retry = match via {
            Via::Reconcile => "`skillherd reconcile --adopt n`".to_string(),
            Via::Update => "`skillherd update n --adopt n`".to_string(),
            Via::Install { source } => format!("`skillherd install {source} --skill n --adopt n`"),
        };
        parts.push(format!(
            "for {}: {retry} takes upstream for one name, the same command with `--keep n` keeps the pool copy as house (.globals/house) and stops tracking it upstream, and the house rule is to fork under a distinct name",
            edited.join(", ")
        ));
    }
    let displaced = names("displaced_refused");
    if !displaced.is_empty() {
        parts.push(format!(
            "for {}: `skillherd reconcile --restore n` puts the displaced copy in .npx/backup back (nothing is restored unasked), then rerun",
            displaced.join(", ")
        ));
    }
    let occupied = names("stale_backup_refused");
    if !occupied.is_empty() {
        parts.push(format!(
            "for {}: compare .npx/backup/n with the live entry and remove the backup by hand (this tool never removes what it did not displace), then rerun",
            occupied.join(", ")
        ));
    }
    if parts.is_empty() {
        "read each refusal's message for its remedy".to_string()
    } else {
        parts.join("; ")
    }
}
