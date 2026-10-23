//! `skillherd info` — Describe one skill.

use serde_json::{Value, json};

use crate::cli::InfoArgs;
use crate::error::Result;
use crate::library;
use crate::names::validate_name;
use crate::paths::Paths;
use crate::policy;

pub fn run(args: &InfoArgs, paths: &Paths, _dry_run: bool) -> Result<Value> {
    let name = validate_name(&args.name)?;
    let catalog = library::catalog(paths)?;
    let policy = policy::load(paths)?;

    let Some(entry) = catalog.iter().find(|entry| entry.name == name) else {
        return Err(library::missing_skill(paths, &policy, &catalog, name));
    };
    let row = policy.row(name);

    Ok(json!({
        "name": entry.name,
        "path": entry.path,
        "description": entry.description,
        "is_symlink": entry.is_symlink,
        "link_target": entry.link_target,
        "variants": entry.variants,
        "npx": entry.npx,
        "local_edit": entry.local_edit,
        "tiers": row.map(library::tiers_of).unwrap_or_default(),
        "line": row.map(|row| row.line),
        "visible_in": library::visible_in(&policy, name, paths.platform),
    }))
}
