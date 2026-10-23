//! `skillherd drop` — Deactivate skills from a project.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use crate::cli::DropArgs;
use crate::error::{Error, Result};
use crate::managed::{self, Expected};
use crate::model::{PlanOp, Refusal};
use crate::names::validate_name;
use crate::output;
use crate::paths::Paths;
use crate::project::{self, ProjectDrop, ProjectDropReport, project_dir};

/// Remove the project links `add` created, plus a legacy `.codex/skills` link.
pub fn run(args: &DropArgs, paths: &Paths, dry_run: bool) -> Result<Value> {
    let project = project_dir(args.project.as_deref())?;
    let surfaces = project::project_surfaces(&project);

    let mut dropped: Vec<ProjectDrop> = Vec::new();
    let mut refused: Vec<Refusal> = Vec::new();
    let mut plan: Vec<PlanOp> = Vec::new();

    for name in &args.names {
        validate_name(name)?;
        let links: Vec<_> = surfaces.iter().map(|surface| surface.join(name)).collect();

        // What each link held when it was read, so the removal below can re-prove
        // it immediately before it deletes anything.
        let mut existed: Vec<(PathBuf, Expected)> = Vec::new();
        let mut foreign: Option<String> = None;
        for link in &links {
            match fs::symlink_metadata(link) {
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(Error::io(&format!("reading {}", link.display()), &err)),
            }
            let Some(entry) = managed::entry_at(link, paths)? else {
                continue;
            };
            let Some(expected) = Expected::of(&entry).filter(|_| entry.managed) else {
                foreign = Some(format!("{} is not a managed link", link.display()));
                break;
            };
            existed.push((link.clone(), expected));
        }

        if let Some(reason) = foreign {
            refused.push(Refusal {
                name: name.clone(),
                code: "foreign_entry".into(),
                message: format!(
                    "{reason}; refusing to remove anything for {name}. Delete it by hand if it is not another tool's, or rerun `skillherd drop {name} --project {}` once it is",
                    project.display()
                ),
            });
            dropped.push(ProjectDrop {
                name: name.clone(),
                existed: existed.into_iter().map(|(path, _)| path).collect(),
                removed: Vec::new(),
            });
            continue;
        }

        if existed.is_empty() {
            output::warn(
                "not_active",
                format!("{name} is not active in {}", project.display()),
            );
        }

        let outcome = project::remove_links(paths, &existed, dry_run)?;
        plan.extend(outcome.plan);
        let removed = outcome.removed;
        for reason in outcome.refused {
            refused.push(Refusal {
                name: name.clone(),
                code: "foreign_entry".into(),
                message: format!(
                    "{reason}; refusing to remove it. Delete it by hand if it is not another tool's, or rerun `skillherd drop {name} --project {}` once it is",
                    project.display()
                ),
            });
        }

        dropped.push(ProjectDrop {
            name: name.clone(),
            existed: existed.into_iter().map(|(path, _)| path).collect(),
            removed,
        });
    }

    let report = ProjectDropReport {
        project,
        dropped,
        refused,
        plan,
    };
    serde_json::to_value(&report)
        .map_err(|err| Error::internal(format!("serializing the drop report: {err}")))
}
