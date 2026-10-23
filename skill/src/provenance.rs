//! Provenance: every pool entry is either tracked upstream or declared house.
//!
//! A pool skill has one of three origins. An npx import carries
//! `.npx/<name>.json`, and `skillherd update` keeps it current; so does a skill
//! tracked as a GitHub release asset (`sourceType: "github-release"`, see
//! `release.rs`), whose record is the same file. A house skill was
//! written here (or is a deliberate fork whose edits outrank upstream) and is
//! named, one per line, in `library/.globals/house`. A linked entry is a
//! symlink whose target repo is its provenance. Anything else is a copy whose
//! origin nobody recorded, which is how a skill silently stops getting
//! upstream fixes: doctor reports it (`unknown_provenance`, or
//! `untracked_upstream` when the live npx lock still names a source).
//!
//! The file format is the policy files' format: `#` starts a comment, blank
//! lines are ignored, and the first word of a line is the name, so a line may
//! carry a note (`slidev  # fork of slidevjs/slidev: house edits`).

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::names::validate_name;
use crate::paths::Paths;

/// The house list inside the library.
pub fn house_path(paths: &Paths) -> PathBuf {
    paths.library.join(".globals/house")
}

/// The pool-relative path of the house list, for commits.
pub const HOUSE_REL: &str = ".globals/house";

/// The declared house names; a missing file declares none.
pub fn load_house(paths: &Paths) -> Result<BTreeSet<String>> {
    let path = house_path(paths);
    let text = match crate::fsx::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(err) => return Err(Error::io(&format!("reading {}", path.display()), &err)),
    };
    Ok(parse_house(&text))
}

/// Names from house-list text; a line whose first word is not a valid skill
/// name is ignored rather than fatal, so one typo cannot hide the rest.
pub fn parse_house(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| {
            let body = line.split('#').next().unwrap_or("").trim();
            let name = body.split_whitespace().next()?;
            validate_name(name).ok().map(|_| name.to_string())
        })
        .collect()
}

/// Append `name` (with an optional note) unless it is already declared.
/// Returns whether the file changed.
pub fn declare_house(paths: &Paths, name: &str, note: Option<&str>) -> Result<bool> {
    validate_name(name)?;
    let path = house_path(paths);
    let mut text = match crate::fsx::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::from(
            "# House skills: written here, or deliberate forks whose edits outrank upstream.\n# One name per line; `skillherd doctor` reports pool entries that are neither\n# tracked upstream (.npx/<name>.json) nor named here.\n",
        ),
        Err(err) => return Err(Error::io(&format!("reading {}", path.display()), &err)),
    };
    if parse_house(&text).contains(name) {
        return Ok(false);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    match note {
        Some(note) => text.push_str(&format!("{name}  # {note}\n")),
        None => text.push_str(&format!("{name}\n")),
    }
    crate::fsx::atomic_write(&path, text.as_bytes())
        .map_err(|err| Error::io(&format!("writing {}", path.display()), &err))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_takes_the_first_word_and_skips_comments_and_bad_names() {
        let names = parse_house("# header\n\nslidev  # fork of x\n  deslop\nbad/name\n../evil\n");
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            vec!["deslop".to_string(), "slidev".to_string()]
        );
    }
}
