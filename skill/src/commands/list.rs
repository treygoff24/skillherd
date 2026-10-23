//! `skillherd list` — List skills for a harness, a tier, or the whole catalog.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::cli::{ListArgs, TierArg};
use crate::error::{Error, Result};
use crate::library::{self, SkillEntry};
use crate::model::Policy;
use crate::paths::Paths;
use crate::policy;

pub fn run(args: &ListArgs, paths: &Paths, _dry_run: bool) -> Result<Value> {
    let policy = policy::load_for_inspection(paths)?;

    if args.all {
        if args.harness.is_some() || args.tier.is_some() {
            return Err(Error::usage(
                "`--all` lists the whole catalog; it cannot be combined with `--harness` or `--tier`",
            ));
        }
        return catalog_view(&policy, paths);
    }
    if let Some(harness) = &args.harness {
        return harness_view(&policy, paths, harness);
    }
    row_view(&policy, paths, args.tier)
}

/// The policy rows (`--tier` narrows to one tier's own rows) with the catalog's
/// description beside each, so a row whose entry is gone is visible.
fn row_view(policy: &Policy, paths: &Paths, tier: Option<TierArg>) -> Result<Value> {
    let catalog = library::catalog(paths)?;
    let by_name: BTreeMap<&str, &SkillEntry> = catalog
        .iter()
        .map(|entry| (entry.name.as_str(), entry))
        .collect();

    let mut skills = Vec::new();
    for row in &policy.rows {
        let wanted = match tier {
            None => true,
            Some(TierArg::Main) => row.main,
            Some(TierArg::Lane) => row.lane,
        };
        if !wanted {
            continue;
        }
        let entry = by_name.get(row.name.as_str());
        skills.push(json!({
            "name": row.name,
            "line": row.line,
            "tiers": library::tiers_of(row),
            "description": entry.map(|entry| entry.description.clone()).unwrap_or_default(),
            "missing_entry": entry.is_none(),
        }));
    }

    Ok(json!({
        "mode": if tier.is_none() { "tiers" } else { "tier" },
        "tier": tier.map(|tier| match tier { TierArg::Main => "main", TierArg::Lane => "lane" }),
        "count": skills.len(),
        "skills": skills,
    }))
}

/// One harness's view: the union of its read roots, with the path each name is
/// actually loaded from and the root it arrives through.
fn harness_view(policy: &Policy, paths: &Paths, harness: &str) -> Result<Value> {
    let roots = library::harness_roots(policy, paths, harness)?;

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut names: Vec<&str> = Vec::new();
    for root in &roots {
        for name in &root.names {
            if seen.insert(name.as_str()) {
                names.push(name.as_str());
            }
        }
    }
    names.sort();

    let mut skills = Vec::new();
    for name in names {
        let Some(root) = roots
            .iter()
            .find(|root| root.names.iter().any(|candidate| candidate == name))
        else {
            continue;
        };
        skills.push(json!({
            "name": name,
            "source": library::source_for(root, paths, name),
            "via": root.via,
        }));
    }

    Ok(json!({
        "mode": "harness",
        "harness": harness,
        "tier": library::tier_of(policy, harness),
        "count": skills.len(),
        "roots": roots.iter().map(|root| json!({
            "via": root.via,
            "path": root.path,
            "count": root.names.len(),
        })).collect::<Vec<Value>>(),
        "skills": skills,
    }))
}

/// The whole pool, each entry with the harnesses that see it.
fn catalog_view(policy: &Policy, paths: &Paths) -> Result<Value> {
    let catalog = library::catalog(paths)?;
    let visibility = library::visibility_index(policy, paths.platform);
    let skills: Vec<Value> = catalog
        .iter()
        .map(|entry| {
            json!({
                "name": entry.name,
                "path": entry.path,
                "description": entry.description,
                "is_symlink": entry.is_symlink,
                "link_target": entry.link_target,
                "variants": entry.variants,
                "local_edit": entry.local_edit,
                "active_in": visibility.get(&entry.name).cloned().unwrap_or_default(),
            })
        })
        .collect();

    Ok(json!({
        "mode": "all",
        "count": skills.len(),
        "skills": skills,
    }))
}
