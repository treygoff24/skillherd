//! `skillherd search` — Search skill names and descriptions.

use serde_json::{Value, json};

use crate::cli::SearchArgs;
use crate::error::{Error, Result};
use crate::library::{self, SkillEntry};
use crate::paths::Paths;

pub fn run(args: &SearchArgs, paths: &Paths, _dry_run: bool) -> Result<Value> {
    let needle = args.query.trim().to_lowercase();
    if needle.is_empty() {
        return Err(Error::usage(
            "`search` needs a non-empty query; `skillherd list --all` prints the whole catalog",
        ));
    }

    // The catalog is sorted by name, so both hit lists are deterministic and
    // a name hit always sorts ahead of a description-only hit.
    let catalog = library::catalog(paths)?;
    let mut name_hits: Vec<(&SkillEntry, &str)> = Vec::new();
    let mut description_hits: Vec<(&SkillEntry, &str)> = Vec::new();
    for entry in &catalog {
        let in_name = entry.name.to_lowercase().contains(&needle);
        let in_description = entry.description.to_lowercase().contains(&needle);
        if in_name {
            let matched = if in_description {
                "name+description"
            } else {
                "name"
            };
            name_hits.push((entry, matched));
        } else if in_description {
            description_hits.push((entry, "description"));
        }
    }

    let hits: Vec<(&SkillEntry, &str)> = name_hits.into_iter().chain(description_hits).collect();
    if args.names {
        let names: Vec<&str> = hits.iter().map(|(entry, _)| entry.name.as_str()).collect();
        return Ok(json!({
            "query": args.query,
            "count": names.len(),
            "names": names,
        }));
    }

    let skills: Vec<Value> = hits
        .iter()
        .map(|(entry, matched)| {
            json!({
                "name": entry.name,
                "description": entry.description,
                "match": matched,
            })
        })
        .collect();
    Ok(json!({
        "query": args.query,
        "count": skills.len(),
        "skills": skills,
    }))
}
