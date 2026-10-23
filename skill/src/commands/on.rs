//! `skillherd on` — Activate skills for a tier, then render.

use serde_json::{Value, json};

use crate::cli::{OnArgs, TierArg};
use crate::error::{Error, Result};
use crate::library;
use crate::lockfile;
use crate::model::{PlanOp, Platform, Policy, SetRow};
use crate::names::validate_name;
use crate::paths::Paths;
use crate::policy;
use crate::render;

/// Activate every name for the tier, then render every surface.
///
/// The row is validated before anything is written (`policy_unrepresentable`
/// for a combination no surface can express, or a row no harness would read),
/// and the whole read-decide-write of `sets` happens under the library lock: a
/// caller that loaded the file before taking the lock can drop a row another
/// caller added in between. The render is planned before `sets` is saved, so a
/// malformed fence leaves both the file and the surfaces as they were, and it
/// is applied under the same lock.
pub fn run(args: &OnArgs, paths: &Paths, dry_run: bool) -> Result<Value> {
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
    let only = harness_references(&policy, &args.only)?;
    let not = harness_references(&policy, &args.not)?;
    let mut not_platform = Vec::new();
    if args.no_mac {
        not_platform.push(Platform::Mac);
    }
    if args.no_linux {
        not_platform.push(Platform::Linux);
    }

    let catalog = library::catalog(paths)?;
    for name in &names {
        if !catalog.iter().any(|entry| &entry.name == name) {
            return Err(library::missing_skill(paths, &policy, &catalog, name));
        }
    }

    let tier = match args.tier {
        TierArg::Main => "main",
        TierArg::Lane => "lane",
    };
    let sets = library::sets_path(paths);
    let mut rows: Vec<Value> = Vec::new();
    let mut plan: Vec<PlanOp> = Vec::new();
    for name in &names {
        let existing = policy.rows.iter().position(|row| &row.name == name);
        // A row that is not in the file yet has no line to report; `check_row`
        // names a line only when there is one.
        let line = existing.map(|index| policy.rows[index].line).unwrap_or(0);
        let row = SetRow {
            name: name.clone(),
            main: true,
            lane: matches!(args.tier, TierArg::Lane),
            not_platform: not_platform.clone(),
            only: only.clone(),
            not: not.clone(),
            line,
        };
        policy::check_row(&policy, &row)?;

        match existing {
            Some(index) => {
                let before = policy.rows[index].clone();
                let unchanged = before.main == row.main
                    && before.lane == row.lane
                    && before.not_platform == row.not_platform
                    && before.only == row.only
                    && before.not == row.not;
                policy.rows[index] = row.clone();
                rows.push(json!({
                    "name": name,
                    "op": if unchanged { "unchanged" } else { "update" },
                    "before": policy::row_line(&before),
                    "after": policy::row_line(&row),
                }));
                if !unchanged {
                    plan.push(PlanOp {
                        op: "set_row".to_string(),
                        path: sets.clone(),
                        target: None,
                    });
                }
            }
            None => {
                policy.rows.push(row.clone());
                rows.push(json!({
                    "name": name,
                    "op": "add",
                    "before": Value::Null,
                    "after": policy::row_line(&row),
                }));
                plan.push(PlanOp {
                    op: "set_row".to_string(),
                    path: sets.clone(),
                    target: None,
                });
            }
        }
    }

    for name in &names {
        require_a_reader(&policy, name)?;
    }

    let changed = !plan.is_empty();
    // Plan the render of the edited policy before anything is written: a
    // malformed fence fails the run here, with neither the `sets` row nor any
    // link written (a dry run fails the same way the real run would).
    let render_plan = render::plan(paths, &every_surface(), &policy)?;
    if dry_run {
        return Ok(json!({
            "names": names,
            "tier": tier,
            "changed": changed,
            "sets": sets,
            "rows": rows,
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
        "tier": tier,
        "changed": changed,
        "sets": sets,
        "rows": rows,
        "plan": plan,
        "render": render,
    }))
}

/// Resolve `--only`/`--not` against the harness table, `omp` as `agents`.
fn harness_references(policy: &Policy, requested: &[String]) -> Result<Vec<String>> {
    let mut references: Vec<String> = Vec::new();
    for name in requested {
        let canonical = if name == policy::OMP {
            policy::AGENTS.to_string()
        } else {
            name.clone()
        };
        if canonical != policy::AGENTS && !policy.harnesses.iter().any(|row| row.name == canonical)
        {
            return Err(Error::policy_unknown_harness(name, None));
        }
        if !references.contains(&canonical) {
            references.push(canonical);
        }
    }
    Ok(references)
}

/// Refuse a row no harness would read on any platform — `--only grok` under
/// the default table, where grok has tier `none` and no surface of its own —
/// instead of saving it and reporting success for a skill nothing sees.
fn require_a_reader(policy: &Policy, name: &str) -> Result<()> {
    for platform in [Platform::Mac, Platform::Linux] {
        for harness in &policy.harnesses {
            if policy::visible_set(policy, &harness.name, platform)?
                .iter()
                .any(|visible| visible == name)
            {
                return Ok(());
            }
        }
    }
    let row = policy.rows.iter().find(|row| row.name == name);
    let reason = match row {
        Some(row) if !row.only.is_empty() => {
            let named: Vec<String> = row
                .only
                .iter()
                .map(|harness| {
                    let tier = policy
                        .harness(harness)
                        .map(|found| format!("{:?}", found.tier).to_lowercase())
                        .unwrap_or_else(|| "unknown".to_string());
                    format!("only:{harness} (tier {tier})")
                })
                .collect();
            format!(
                "{}: no named harness serves this row, so no harness would see it; give the harness a tier in .globals/harnesses, or name one that serves this tier",
                named.join(", ")
            )
        }
        _ => "with these tokens no harness in the table would see it on any platform".to_string(),
    };
    Err(Error::policy_unrepresentable(name, &reason))
}

/// A render of every surface, every skill.
fn every_surface() -> render::RenderOpts {
    render::RenderOpts {
        surfaces: Vec::new(),
        dry_run: false,
        only: None,
    }
}
