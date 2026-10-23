//! `skillherd off` — Deactivate skills, then render.

use serde_json::{Value, json};

use crate::cli::OffArgs;
use crate::error::{Error, Result};
use crate::library;
use crate::lockfile;
use crate::model::PlanOp;
use crate::names::validate_name;
use crate::output;
use crate::paths::Paths;
use crate::policy;
use crate::render;

/// Drop every row naming a requested skill, then render every surface.
///
/// A name with no row is a warning, not a failure: `off` says what it did, and
/// every name it cannot find was already in the state the caller asked for. The
/// read-decide-write of `sets` happens under the library lock, so an `off` racing
/// an `on` cannot resurrect a row or drop one. The render is planned before
/// `sets` is saved and applied under the same lock.
pub fn run(args: &OffArgs, paths: &Paths, dry_run: bool) -> Result<Value> {
    // A dry run reads the file without writing it, and the ABI forbids it the
    // lock; a real run holds it across the load, the decision and the save.
    let guard = if dry_run {
        None
    } else {
        Some(lockfile::lock_library(paths)?)
    };
    let mut policy = policy::load(paths)?;

    let mut names: Vec<String> = Vec::new();
    for name in &args.names {
        let name = validate_name(name)?.to_string();
        if !names.contains(&name) {
            names.push(name);
        }
    }

    let sets = library::sets_path(paths);
    let mut removed: Vec<Value> = Vec::new();
    let mut plan: Vec<PlanOp> = Vec::new();
    for name in &names {
        let lines: Vec<usize> = policy
            .rows
            .iter()
            .filter(|row| &row.name == name)
            .map(|row| row.line)
            .collect();
        if lines.is_empty() {
            output::warn(
                "policy_row_absent",
                format!("{name} has no row in sets; nothing to remove"),
            );
            continue;
        }
        policy.rows.retain(|row| &row.name != name);
        removed.push(json!({ "name": name, "lines": lines }));
        plan.push(PlanOp {
            op: "set_row".to_string(),
            path: sets.clone(),
            target: None,
        });
    }

    let changed = !plan.is_empty();
    // Plan the render of the edited policy before anything is written: a
    // malformed fence fails the run here, with neither the `sets` row nor any
    // link written (a dry run fails the same way the real run would).
    let render_plan = render::plan(paths, &every_surface(), &policy)?;
    if dry_run {
        return Ok(json!({
            "names": names,
            "changed": changed,
            "sets": sets,
            "removed": removed,
            "plan": plan,
            "render": Value::Null,
        }));
    }

    let guard = guard.ok_or_else(|| Error::internal("a real run holds the library lock"))?;
    policy::save(paths, &policy)?;
    // The lock taken for the edit is the one the render runs under: nobody
    // can change `sets` between the save and the surfaces it builds.
    let report = render_plan.apply(paths, &guard)?;
    let render = serde_json::to_value(report)
        .map_err(|err| Error::internal(format!("serializing the render report: {err}")))?;

    Ok(json!({
        "names": names,
        "changed": changed,
        "sets": sets,
        "removed": removed,
        "plan": plan,
        "render": render,
    }))
}

/// A render of every surface, every skill.
fn every_surface() -> render::RenderOpts {
    render::RenderOpts {
        surfaces: Vec::new(),
        dry_run: false,
        only: None,
    }
}
