//! `skillherd add` — Activate skills in a project.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use crate::cli::AddArgs;
use crate::error::{Error, Result};
use crate::managed;
use crate::model::{PlanOp, Refusal};
use crate::paths::Paths;
use crate::project::{
    ProjectActivation, ProjectAddReport, ProjectSurface, expand_names, project_dir, project_rows,
    require_library_entries,
};
use crate::surfaces;

/// Activate one or more pool skills in a project, variant-aware and stripped.
///
/// A name whose path on any project surface holds an entry that is not ours is
/// refused as a whole and touches neither surface. Every other name is planned
/// on both surfaces before anything is written, so a malformed fence fails the
/// run with nothing changed; a rerun is idempotent, served copies included.
pub fn run(args: &AddArgs, paths: &Paths, dry_run: bool) -> Result<Value> {
    let project = project_dir(args.project.as_deref())?;
    let names = expand_names(paths, &args.names)?;
    require_library_entries(paths, &names)?;
    let rows = project_rows(&project);

    let mut refused: Vec<Refusal> = Vec::new();
    let mut wanted: Vec<String> = Vec::new();
    'names: for name in &names {
        for row in &rows {
            let path = row.surface.join(name);
            if let Some(entry) = managed::entry_at(&path, paths)?
                && !entry.managed
            {
                refused.push(Refusal {
                    name: name.clone(),
                    code: "foreign_entry".into(),
                    message: format!(
                        "{} is {} and not ours; refusing to touch it. Remove or park it by hand if it is not another tool's, then rerun `skillherd add {name} --project {}`",
                        path.display(),
                        managed::describe_entry(&entry),
                        project.display()
                    ),
                });
                continue 'names;
            }
        }
        wanted.push(name.clone());
    }

    let mut plans = Vec::new();
    for row in &rows {
        let plan = surfaces::plan_surface_restricted(row, &wanted, paths)?;
        if !plan.errors.is_empty() {
            return Err(Error::render_failed(&row.surface, &plan.errors));
        }
        plans.push((row, plan));
    }

    let mut plan_ops: Vec<PlanOp> = Vec::new();
    let mut written: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut stripper: Vec<ProjectSurface> = Vec::new();
    for (row, plan) in plans {
        // The report names each link's source as an absolute path, as it
        // always has; the link written on disk stays relative.
        plan_ops.extend(plan.ops.iter().map(|planned| {
            let mut op = planned.op.clone();
            if let (Some(target), Some(dir)) = (&op.target, op.path.parent()) {
                op.target = Some(crate::paths::lexical_abs(dir, target));
            }
            op
        }));
        let refusals = if dry_run {
            Vec::new()
        } else {
            surfaces::apply_ops(&row.surface, &plan.ops, paths)?
        };
        for planned in &plan.ops {
            let name = planned
                .op
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if refusals.iter().any(|refusal| refusal.name == name) {
                continue;
            }
            written
                .entry(name)
                .or_default()
                .push(planned.op.path.clone());
        }
        for refusal in refusals {
            refused.push(Refusal {
                message: format!(
                    "{}; refusing to replace it. Move it by hand if it is not another tool's, then rerun `skillherd add {} --project {}`",
                    refusal.message,
                    refusal.name,
                    project.display()
                ),
                ..refusal
            });
        }
        if let Some(report) = plan.report.stripper {
            stripper.push(ProjectSurface {
                surface: row.surface.clone(),
                harness: row.variant_key.clone(),
                report,
            });
        }
    }

    let mut added: Vec<ProjectActivation> = Vec::new();
    let mut unchanged: Vec<String> = Vec::new();
    for name in &wanted {
        match written.remove(name) {
            Some(links) => added.push(ProjectActivation {
                name: name.clone(),
                variant: paths
                    .library
                    .join(name)
                    .join(".variants/claude")
                    .is_dir()
                    .then(|| "claude".to_string()),
                links,
            }),
            None if !refused.iter().any(|refusal| &refusal.name == name) => {
                unchanged.push(name.clone());
            }
            None => {}
        }
    }

    let report = ProjectAddReport {
        project,
        added,
        unchanged,
        refused,
        plan: plan_ops,
        stripper,
    };
    serde_json::to_value(&report)
        .map_err(|err| Error::internal(format!("serializing the add report: {err}")))
}
