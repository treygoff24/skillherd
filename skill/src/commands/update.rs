//! `skillherd update` — Update installed skills through a staged npx.
//!
//! Same staged flow as `install`: npx runs in the stage home, and everything it
//! leaves in the stage inbox is reconciled back into the pool, variants and all.
//! Skills tracked as GitHub release assets (`release.rs`) are checked in the
//! same stage: a newer stable tag is fetched into the inbox, the same tag is a
//! no-op, and npx is not asked about them.

use std::collections::BTreeSet;

use serde_json::Value;

/// Local imports have no fetchable remote. Invalid records are left for
/// reconcile to report, rather than silently reinterpreted as remote sources.
fn local_imports(paths: &Paths) -> Result<BTreeSet<String>> {
    let dir = paths.library.join(".npx");
    let read = match std::fs::read_dir(&dir) {
        Ok(read) => read,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(err) => return Err(crate::error::Error::io("reading import records", &err)),
    };
    let mut names = BTreeSet::new();
    for entry in read {
        let entry = entry.map_err(|err| crate::error::Error::io("reading import record", &err))?;
        let file = entry.file_name().to_string_lossy().into_owned();
        let Some(name) = file
            .strip_suffix(".json")
            .filter(|name| validate_name(name).is_ok())
        else {
            continue;
        };
        let Ok(text) = crate::fsx::read_to_string(&entry.path()) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<crate::model::NpxMeta>(&text) else {
            continue;
        };
        if record.source_type == "local" {
            names.insert(name.to_string());
        }
    }
    Ok(names)
}

use crate::cli::UpdateArgs;
use crate::commands::install::{Resolve, Upstream, staged};
use crate::commands::reconcile::Via;
use crate::error::Result;
use crate::names::validate_name;
use crate::npx::Verb;
use crate::paths::Paths;
use crate::release::{self, Target};

/// Update the named skills (or every tracked skill), then reconcile.
pub fn run(args: &UpdateArgs, paths: &Paths, dry_run: bool) -> Result<Value> {
    for name in &args.names {
        validate_name(name)?;
    }
    staged(
        paths,
        dry_run,
        || {
            let tracked = release::tracked(paths)?;
            let wanted = |name: &String| args.names.is_empty() || args.names.contains(name);
            let local = local_imports(paths)?;
            for name in local.iter().filter(|name| wanted(name)) {
                crate::output::warn("local_update_skipped", format!(
                    "{name} was imported from a local path and has no remote to update; use `skillherd install <local-path> --skill {name}` to import it again."
                ));
            }
            let releases: Vec<Target> = tracked
                .iter()
                .filter(|(name, _)| wanted(name))
                .filter_map(|(name, record)| {
                    Some(Target {
                        name: name.clone(),
                        source: record.release.clone()?,
                        previous: Some(record.clone()),
                        force: false,
                    })
                })
                .collect();
            // npx updates everything it tracks when no names are given; named,
            // it gets only the names that are not release-tracked.
            let npx_names: Vec<String> = args
                .names
                .iter()
                .filter(|name| !tracked.contains_key(*name) && !local.contains(*name))
                .cloned()
                .collect();
            let npx = (args.names.is_empty() || !npx_names.is_empty())
                .then_some(Verb::Update { names: npx_names });
            Ok(Upstream { npx, releases })
        },
        Resolve {
            adopt: &args.adopt,
            keep: &args.keep,
            via: Via::Update,
        },
    )?
    .into_value()
}
