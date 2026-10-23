//! `skillherd remove` — Remove upstream skills from the pool.
//!
//! Only npx-tracked skills can be removed: `.npx/<name>.json` is the record
//! that says where the entry came from, and a house skill is not the wrapper's
//! to delete. A locally edited entry or variant is refused unless `--force`.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::cli::RemoveArgs;
use crate::commands::reconcile::Data;
use crate::error::{Error, Result};
use crate::hash::committed_tree_hash;
use crate::lockfile::lock_library;
use crate::model::{NpxMeta, PlanOp};
use crate::names::validate_name;
use crate::npx::{self, NpxRun, Verb};
use crate::paths::Paths;
use crate::policy;
use crate::reconcile::{self, ReconcileReport};
use crate::variants::stored_variant;

/// The (empty) reconcile report, plus what was removed and what npx ran.
#[derive(Debug, Serialize)]
struct RemoveData {
    #[serde(flatten)]
    report: Data,
    removed: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    npx: Option<NpxRun>,
}

/// Read the import record of a tracked skill.
fn read_record(paths: &Paths, name: &str) -> Result<NpxMeta> {
    let path = paths.library.join(".npx").join(format!("{name}.json"));
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::InputNotFound {
                message: format!(
                    "{name} is not an npx install, so there is nothing for `skillherd remove` to remove ({} is missing)",
                    path.display()
                ),
                details: serde_json::json!({ "name": name, "record": path }),
                suggested_fix: Some(format!(
                    "`skillherd off {name}` deactivates a house skill; delete it in the library by hand"
                )),
            });
        }
        Err(err) => return Err(Error::io(&format!("reading {}", path.display()), &err)),
    };
    serde_json::from_str(&text).map_err(|err| {
        Error::internal(format!(
            "{} is not a readable import record: {err}",
            path.display()
        ))
    })
}

/// Refuse to discard hand edits, unless `--force` says otherwise.
fn guard_local_edits(paths: &Paths, name: &str, record: &NpxMeta, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    let entry = paths.library.join(name);
    if entry.is_dir()
        && committed_tree_hash(&paths.library, name, &entry, &[])? != record.imported_hash
    {
        return Err(local_edit(
            name,
            "the entry differs from its recorded import",
        ));
    }
    for (harness, recorded) in &record.variants {
        let variant = stored_variant(paths, name, harness);
        if variant.is_dir()
            && committed_tree_hash(
                &paths.library,
                &format!("{name}/.variants/{harness}"),
                &variant,
                &[],
            )? != *recorded
        {
            return Err(local_edit(
                name,
                &format!("the .variants/{harness} copy differs from its recorded import"),
            ));
        }
    }
    for (harness, recorded) in &record.harness_agents {
        let stored = crate::agents::stored_agents(paths, name, harness);
        if stored.is_dir()
            && committed_tree_hash(
                &paths.library,
                &format!("{name}/{}/{harness}", crate::agents::AGENTS_DIR),
                &stored,
                &[],
            )? != *recorded
        {
            return Err(local_edit(
                name,
                &format!(
                    "the {}/{harness} agents differ from their recorded import",
                    crate::agents::AGENTS_DIR
                ),
            ));
        }
    }
    Ok(())
}

fn local_edit(name: &str, reason: &str) -> Error {
    Error::LocalEditRefused {
        message: format!(
            "{name} was edited locally and `skillherd remove` will not discard it: {reason}"
        ),
        details: serde_json::json!({ "name": name, "reason": reason }),
        suggested_fix: Some(format!(
            "pass `--force` to remove {name} and its variants anyway, or move the edits out of the pool first"
        )),
    }
}

/// Drop every `sets` row naming one of `names`.
///
/// The policy module owns the file grammar and its safe write path.
fn drop_sets_rows(
    paths: &Paths,
    names: &BTreeSet<String>,
    dry_run: bool,
) -> Result<Option<PlanOp>> {
    let sets = paths.library.join(".globals/sets");
    let op = PlanOp {
        op: "write".to_string(),
        path: sets.clone(),
        target: None,
    };

    let mut policy = match policy::load(paths) {
        Ok(policy) => policy,
        Err(error) if error.code() == "input_not_found" => return Ok(None),
        Err(error) => return Err(error),
    };

    let before = policy.rows.len();
    policy.rows.retain(|row| !names.contains(&row.name));
    if policy.rows.len() == before {
        return Ok(None);
    }
    if !dry_run {
        policy::save(paths, &policy)?;
    }
    Ok(Some(op))
}

fn entry_paths(paths: &Paths, names: &[String]) -> Vec<PathBuf> {
    names.iter().map(|name| paths.library.join(name)).collect()
}

/// Remove tracked skills: drop their policy rows, run a staged `npx remove`,
/// delete the pool entry and its record, commit, render.
///
/// The lock comes before anything mutable is read: the import records, the entry
/// hashes the local-edit guard compares and the `sets` rows all have to be the
/// state this run acts on, or an entry edited between the hash check and the
/// deletion would be discarded unguarded. The lock is released before the render
/// (`render` takes it itself) and the npx/reconcile calls it makes are the ones
/// that expect it held or handed over — `install`/`update` pass their guard to
/// `reconcile::run_locked`; this command keeps its own.
pub fn run(args: &RemoveArgs, paths: &Paths, dry_run: bool) -> Result<Value> {
    let mut names: Vec<String> = Vec::new();
    for name in &args.names {
        validate_name(name)?;
        if !names.contains(name) {
            names.push(name.clone());
        }
    }

    let guard = if dry_run {
        None
    } else {
        Some(lock_library(paths)?)
    };

    let name_set: BTreeSet<String> = names.iter().cloned().collect();
    for name in &names {
        let record = read_record(paths, name)?;
        guard_local_edits(paths, name, &record, args.force)?;
    }

    let mut plan = Vec::new();
    let sets_op = drop_sets_rows(paths, &name_set, dry_run)?;
    let sets_changed = sets_op.is_some();
    if let Some(op) = sets_op {
        plan.push(op);
    }

    let verb = Verb::Remove {
        names: names.clone(),
    };
    plan.push(verb.plan_op(paths));
    let npx_run = if dry_run {
        None
    } else {
        npx::prepare_stage(paths)?;
        let run = npx::run(paths, &verb)?;
        // The stage lock already forgot the removed names.
        npx::copy_file_if_exists(&npx::stage_lock(paths), &paths.lock)?;
        npx::clear_stage(paths)?;
        Some(run)
    };

    let snapshotted = npx::snapshot_lock(paths, dry_run)?;
    if snapshotted {
        plan.push(PlanOp {
            op: "write".to_string(),
            path: paths.library.join(npx::lock_snapshot(paths)),
            target: None,
        });
    }

    for entry in entry_paths(paths, &names) {
        plan.push(PlanOp {
            op: "delete".to_string(),
            path: entry.clone(),
            target: None,
        });
        if !dry_run {
            npx::remove_path(&entry)?;
        }
    }
    let mut staged: Vec<String> = names.clone();
    for name in &names {
        let record = paths.library.join(".npx").join(format!("{name}.json"));
        plan.push(PlanOp {
            op: "delete".to_string(),
            path: record.clone(),
            target: None,
        });
        if !dry_run {
            npx::remove_path(&record)?;
        }
        staged.push(format!(".npx/{name}.json"));
    }
    if snapshotted {
        staged.push(npx::lock_snapshot(paths));
    }
    if sets_changed {
        staged.push(".globals/sets".to_string());
    }

    let subject = format!("npx: remove {}", names.join(" "));
    let git_op = PlanOp {
        op: "git".to_string(),
        path: paths.library.clone(),
        target: None,
    };
    let committed = dry_run || reconcile::git_commit(&paths.library, &subject, &staged)?;
    if committed {
        plan.push(git_op);
    }

    // Render takes the library lock itself: release first.
    drop(guard);
    let render = reconcile::render_surfaces(paths, dry_run)?;

    let data = RemoveData {
        report: Data {
            report: ReconcileReport {
                imported: Vec::new(),
                refused: Vec::new(),
                kept: Vec::new(),
                variants_pending: Vec::new(),
                render,
                plan,
            },
        },
        removed: names,
        npx: npx_run,
    };
    serde_json::to_value(&data)
        .map_err(|err| Error::internal(format!("serializing the remove report: {err}")))
}
