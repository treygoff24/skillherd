//! Surface rendering: pool plus policy in, one served set per harness out.
//!
//! The algorithm:
//!
//! 1. Read the policy. A real run takes the library lock first.
//! 2. **Plan every surface** (`surfaces::plan_surface`): for each desired name,
//!    the stripper (`strip.rs`, pure) decides between a link into the library
//!    and a stripped served copy, and the plan records the identity of every
//!    entry it will replace or remove. Nothing is written in this phase, so a
//!    malformed fence anywhere fails the whole run before any surface changes.
//! 3. **Apply** each surface's plan through `surfaces::apply_ops`, which
//!    re-proves each identity immediately before it acts and writes with
//!    no-replace renames.
//!
//! Idempotency is the contract: a second run of an unchanged pool and policy
//! reports every served name `unchanged`, including a fenced skill served as a
//! copy, because a copy whose record matches the source hash, platform,
//! harness and render version is current.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::agents::{self, AgentSurfaceReport};
use crate::error::{Error, Result};
use crate::lockfile;
use crate::managed::Entry;
use crate::model::{HarnessRow, PlanOp, Policy, Refusal, StripperReport};
use crate::paths::Paths;
use crate::policy;
use crate::surfaces::{self, PlannedOp};

/// What to render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RenderOpts {
    /// Harness names to restrict the render to; empty means every row.
    pub surfaces: Vec<String>,
    pub dry_run: bool,
    /// Restrict the render to these skill names.
    pub only: Option<Vec<String>>,
}

/// What happened on one surface.
///
/// `surface` is the harness name (the value `--surface` takes), not the path:
/// the resolved roots travel in `meta.paths`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SurfaceReport {
    pub surface: String,
    /// Names given a new entry, or an entry pointing at a different source.
    pub linked: Vec<String>,
    pub removed: Vec<String>,
    /// Names served in place whose served bytes changed: a served copy
    /// re-stripped, a link that became a copy, or a copy that went back to a
    /// link.
    pub refreshed: Vec<String>,
    pub unchanged: Vec<String>,
    /// Foreign entries: reported, never touched.
    pub foreign: Vec<Entry>,
    /// Operations the apply step refused because the entry at the path was no
    /// longer the one the plan recorded. Nothing was deleted for them.
    pub refused: Vec<Refusal>,
    /// The stripping pass's view of the served names: `rendered` became a
    /// served copy, `refreshed` was a copy and was rebuilt or reverted to a
    /// link, `unchanged` needed no stripping work. `errors` is always empty in
    /// a report: a malformed fence fails the run instead.
    pub stripper: Option<StripperReport>,
}

/// What happened across the run, plus the plan a dry run would execute.
///
/// `plan` is the change set of the run: the ops applied, or the ops a dry run
/// would have applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RenderReport {
    pub surfaces: Vec<SurfaceReport>,
    pub plan: Vec<PlanOp>,
    /// One report per harness whose agents directory this run served
    /// (`agents.rs`): upstream subagents of the skills active on it.
    #[serde(default)]
    pub agents: Vec<AgentSurfaceReport>,
}

/// One harness row's plan.
#[derive(Debug, Clone)]
struct RowPlan {
    row: HarnessRow,
    ops: Vec<PlannedOp>,
    report: SurfaceReport,
    agents: Option<(std::path::PathBuf, Vec<PlannedOp>, AgentSurfaceReport)>,
}

/// A planned render: everything read and decided, nothing written.
///
/// Planning fails as a whole on a malformed fence, so a caller that plans
/// before it writes anything of its own (`on`, `off`, `add`) never leaves a
/// half-applied state behind.
#[derive(Debug, Clone)]
pub struct RenderPlan {
    rows: Vec<RowPlan>,
}

impl RenderPlan {
    /// The ops this plan would apply, in order.
    pub fn ops(&self) -> Vec<PlanOp> {
        let mut plan = Vec::new();
        for row in &self.rows {
            plan.extend(row.ops.iter().map(|planned| planned.op.clone()));
            if let Some((_, ops, _)) = &row.agents {
                plan.extend(ops.iter().map(|planned| planned.op.clone()));
            }
        }
        plan
    }

    /// The report of a run that writes nothing.
    pub fn dry_report(self) -> RenderReport {
        let plan = self.ops();
        let mut surfaces = Vec::new();
        let mut agent_reports = Vec::new();
        for row in self.rows {
            surfaces.push(row.report);
            if let Some((_, _, report)) = row.agents {
                agent_reports.push(report);
            }
        }
        RenderReport {
            surfaces,
            plan,
            agents: agent_reports,
        }
    }

    /// Apply the plan. The guard proves the caller holds the library lock.
    pub fn apply(self, paths: &Paths, _guard: &lockfile::MutatorGuard) -> Result<RenderReport> {
        self.apply_unlocked(paths)
    }

    fn apply_unlocked(self, paths: &Paths) -> Result<RenderReport> {
        let plan = self.ops();
        let mut surfaces = Vec::new();
        let mut agent_reports = Vec::new();
        for row in self.rows {
            let mut report = row.report;
            let refusals = surfaces::apply_ops(&row.row.surface, &row.ops, paths)?;
            fold_refusals(&mut report, refusals);
            surfaces.push(report);
            if let Some((dir, ops, mut agent_report)) = row.agents {
                agents::apply(&dir, &ops, paths, &mut agent_report)?;
                agent_reports.push(agent_report);
            }
        }
        Ok(RenderReport {
            surfaces,
            plan,
            agents: agent_reports,
        })
    }
}

/// Rebuild every owned surface.
///
/// A real run takes the library lock *before* it reads the policy, so the
/// surfaces it builds reflect the policy as of the moment nobody else can
/// change it: a render that read first and locked second could apply a
/// snapshot an `on`/`off` had already superseded, undoing that change on the
/// surfaces. A dry run reads without the lock; it writes nothing.
pub fn run(paths: &Paths, opts: RenderOpts) -> Result<RenderReport> {
    if opts.dry_run {
        let policy = policy::load(paths)?;
        return Ok(plan(paths, &opts, &policy)?.dry_report());
    }
    let guard = lockfile::lock_library(paths)?;
    let policy = policy::load(paths)?;
    run_locked(paths, opts, &policy, &guard)
}

/// Render against a policy the caller loaded while holding the library lock.
///
/// The guard is the proof: `lock_library` is not reentrant, so a mutator that
/// already holds the lock hands it in here instead of releasing and re-locking
/// around a window in which its own policy write could be overtaken.
pub fn run_locked(
    paths: &Paths,
    opts: RenderOpts,
    policy: &Policy,
    guard: &lockfile::MutatorGuard,
) -> Result<RenderReport> {
    let planned = plan(paths, &opts, policy)?;
    if opts.dry_run {
        return Ok(planned.dry_report());
    }
    planned.apply(paths, guard)
}

/// Render a policy snapshot the caller vouches for: a dry run, or a test that
/// builds its policy from `model.rs`. A real run still takes the lock for the
/// writes, but the snapshot may predate it — use `run` or `run_locked` from
/// any code path a second mutator can race.
pub fn run_with_policy(paths: &Paths, opts: RenderOpts, policy: &Policy) -> Result<RenderReport> {
    let planned = plan(paths, &opts, policy)?;
    if opts.dry_run {
        return Ok(planned.dry_report());
    }
    let _guard = lockfile::lock_library(paths)?;
    planned.apply_unlocked(paths)
}

/// Plan a render of `policy`: read everything, decide everything, write
/// nothing. A malformed fence in any served source is `render_failed`.
pub fn plan(paths: &Paths, opts: &RenderOpts, policy: &Policy) -> Result<RenderPlan> {
    let rows = rows_for(policy, &opts.surfaces)?;
    let mut planned = Vec::new();
    let mut failed: Option<(std::path::PathBuf, Vec<(String, String)>)> = None;
    for row in rows {
        if !crate::paths::surface_available(&row.surface) {
            continue;
        }
        let mut desired = policy::effective_set(policy, &row.name, paths.platform)?;
        if let Some(only) = &opts.only {
            let allowed: BTreeSet<&str> = only.iter().map(String::as_str).collect();
            desired.retain(|name| allowed.contains(name.as_str()));
        }
        let surface = if opts.only.is_some() {
            surfaces::plan_surface_restricted(&row, &desired, paths)?
        } else {
            surfaces::plan_surface(&row, &desired, paths)?
        };
        if !surface.errors.is_empty() {
            let (_, errors) = failed.get_or_insert_with(|| (row.surface.clone(), Vec::new()));
            for error in surface.errors {
                if !errors.contains(&error) {
                    errors.push(error);
                }
            }
            continue;
        }
        // The upstream agents of the same served set, in the harness's agents
        // directory. Never stripped: agents are single files served as links.
        let agents = agents::plan_agents(&row, &desired, paths, opts.only.is_some())?;
        planned.push(RowPlan {
            row,
            ops: surface.ops,
            report: surface.report,
            agents,
        });
    }
    if let Some((surface, errors)) = failed {
        return Err(Error::render_failed(&surface, &errors));
    }
    Ok(RenderPlan { rows: planned })
}

/// Convenience for callers that just want every surface rendered.
pub fn all_surfaces(paths: &Paths, dry_run: bool) -> Result<RenderReport> {
    run(
        paths,
        RenderOpts {
            surfaces: Vec::new(),
            dry_run,
            only: None,
        },
    )
}

/// The rows to render: every row, or the named ones in the order given.
fn rows_for(policy: &Policy, wanted: &[String]) -> Result<Vec<HarnessRow>> {
    if wanted.is_empty() {
        return Ok(policy.harnesses.clone());
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut rows = Vec::new();
    for name in wanted {
        if !seen.insert(name.as_str()) {
            continue;
        }
        let row = policy
            .harness(name)
            .cloned()
            .ok_or_else(|| Error::policy_unknown_harness(name, None))?;
        rows.push(row);
    }
    Ok(rows)
}

/// Record the ops the apply step refused: their names are counted in none of
/// `linked`, `refreshed` or `removed`, so the report never claims a change
/// that did not happen.
fn fold_refusals(report: &mut SurfaceReport, refusals: Vec<Refusal>) {
    if refusals.is_empty() {
        return;
    }
    for refusal in &refusals {
        report.linked.retain(|name| name != &refusal.name);
        report.refreshed.retain(|name| name != &refusal.name);
        report.removed.retain(|name| name != &refusal.name);
        if let Some(stripper) = &mut report.stripper {
            stripper.rendered.retain(|name| name != &refusal.name);
            stripper.refreshed.retain(|name| name != &refusal.name);
            stripper.unchanged.retain(|name| name != &refusal.name);
        }
    }
    report.refused = refusals;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> SurfaceReport {
        SurfaceReport {
            surface: "claude".to_string(),
            linked: Vec::new(),
            removed: Vec::new(),
            refreshed: Vec::new(),
            unchanged: Vec::new(),
            foreign: Vec::new(),
            refused: Vec::new(),
            stripper: None,
        }
    }

    /// A refused op is counted nowhere: the report must not claim a change that
    /// did not happen.
    #[test]
    fn a_refused_op_leaves_the_report_alone() {
        let mut report = SurfaceReport {
            linked: vec!["a".to_string(), "b".to_string()],
            refreshed: vec!["e".to_string()],
            removed: vec!["c".to_string()],
            unchanged: vec!["d".to_string()],
            stripper: Some(StripperReport {
                rendered: vec!["b".to_string()],
                refreshed: vec!["e".to_string()],
                unchanged: vec!["a".to_string(), "d".to_string()],
                errors: Vec::new(),
            }),
            ..report()
        };
        let refusal = |name: &str| Refusal {
            name: name.to_string(),
            code: "foreign_entry".to_string(),
            message: format!("{name} is no longer ours"),
        };
        fold_refusals(&mut report, vec![refusal("b"), refusal("c"), refusal("e")]);

        assert_eq!(report.linked, ["a"]);
        assert!(report.removed.is_empty() && report.refreshed.is_empty());
        assert_eq!(report.refused.len(), 3);
        let stripper = report.stripper.unwrap();
        assert!(stripper.rendered.is_empty() && stripper.refreshed.is_empty());
        assert_eq!(stripper.unchanged, ["a", "d"]);
    }

    #[test]
    fn an_unknown_surface_is_reported_against_the_policy() {
        let paths = Paths::fixture("/sandbox/user");
        let policy = policy::default_policy(&paths);
        let err = rows_for(&policy, &["bogus".to_string()]).unwrap_err();
        assert_eq!(err.code(), "policy_unknown_harness");
    }

    #[test]
    fn repeated_surface_names_render_once() {
        let paths = Paths::fixture("/sandbox/user");
        let policy = policy::default_policy(&paths);
        let rows = rows_for(&policy, &["claude".to_string(), "claude".to_string()]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "claude");
    }
}
