//! Subagent definitions an upstream repo ships beside a skill.
//!
//! Some upstreams publish harness subagents next to their skills
//! (`pbakaus/impeccable` ships `.claude/agents/impeccable-*.md`). Reconcile
//! imports the ones that belong to a skill into the pool beside it, under
//! `library/<name>/.harness-agents/<harness>/`, from the same shallow clone the
//! variant pass makes, and records a tree hash per harness in the import
//! record (`harnessAgents`). Render then serves each file of every skill active
//! on that harness, **and listed in `library/.globals/agents`**, as a symlink
//! in the harness's agents directory.
//!
//! **Serving is opt-in.** A subagent is loaded into every session of its
//! harness, which is more reach than the skill it came with, so importing one
//! serves nothing by itself. `.globals/agents` names the skills whose agents
//! are served, one per line (`#` comments, same grammar as `.globals/house`).
//! A skill with imported agents that is active but not listed is reported in
//! the render's `agents[].held`, and any link of ours to its agents is removed.
//!
//! **Which files belong to a skill.** An upstream file
//! `<harness dir>/agents/<file>.md` belongs to skill `n` when its stem is `n`
//! or starts with `n-` (checked against the canonical name and the upstream
//! directory name). The prefix keeps a multi-skill repo from spraying every
//! agent it ships into every skill it hosts.
//!
//! **What render may touch.** The agents directory is shared with files other
//! tools install. Render treats an entry as its own
//! only when it is a symlink whose lexical target is exactly
//! `library/<skill>/.harness-agents/<harness>/<same file name>` — a stricter
//! proof than the surface-wide managed rule — and re-proves that identity
//! immediately before it replaces or removes anything (`surfaces::apply_ops`).
//! A regular file or any other link at a path an agent needs is reported as
//! `blocked` and never replaced.
//!
//! Currently Claude only. The mapping below is the one place a
//! harness is added; `.cursor/agents`, `.grok/agents`, `.github/agents` and
//! Codex's TOML agents are not imported.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hash::committed_tree_hash;
use crate::managed::{self, Entry, EntryKind, Expected};
use crate::model::{HarnessRow, NpxMeta, PlanOp, Refusal};
use crate::names::validate_name;
use crate::npx::remove_path;
use crate::output;
use crate::paths::{Paths, normalize};
use crate::surfaces::{self, PlannedOp};

/// Where an entry keeps its imported agents, one subdirectory per harness.
/// Excluded from the entry's tree hash like `.variants`.
pub const AGENTS_DIR: &str = ".harness-agents";

/// The opt-in list inside the library: the skills whose agents are served.
pub const OPT_IN_REL: &str = ".globals/agents";

/// The skills whose imported agents may be served; a missing file opts in none.
pub fn load_opt_in(paths: &Paths) -> Result<std::collections::BTreeSet<String>> {
    let path = paths.library.join(OPT_IN_REL);
    match fs::read_to_string(&path) {
        Ok(text) => Ok(crate::provenance::parse_house(&text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(err) => Err(Error::io(&format!("reading {}", path.display()), &err)),
    }
}

/// The file extension every imported agent carries.
pub const AGENT_EXTENSION: &str = "md";

/// One harness whose upstream agents are imported and served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentHarness {
    /// The harness row name, which is also the storage key.
    pub harness: &'static str,
    /// Where upstream keeps this harness's agents, relative to the repo root.
    pub upstream: &'static str,
}

/// The harnesses agents are imported for. Adding one here also needs a
/// [`served_dir`] arm.
pub const AGENT_HARNESSES: [AgentHarness; 1] = [AgentHarness {
    harness: "claude",
    upstream: ".claude/agents",
}];

/// The directory a harness loads agents from, when agents are served to it.
/// Claude Code reads each config dir's `agents`, and every config dir links
/// that to `{claude_shared}/agents`.
pub fn served_dir(paths: &Paths, harness: &str) -> Option<PathBuf> {
    match harness {
        "claude" => Some(paths.claude_shared.join("agents")),
        _ => None,
    }
}

/// `<library>/<name>/.harness-agents/<harness>`.
pub fn stored_agents(paths: &Paths, name: &str, harness: &str) -> PathBuf {
    paths.library.join(name).join(AGENTS_DIR).join(harness)
}

/// True when the upstream agent file `file` belongs to one of `owners`: its
/// stem is the owner's name, or the owner's name followed by `-` and more.
pub fn belongs_to(file: &str, owners: &[&str]) -> bool {
    let Some(stem) = file
        .strip_suffix(AGENT_EXTENSION)
        .and_then(|rest| rest.strip_suffix('.'))
    else {
        return false;
    };
    owners.iter().any(|owner| {
        !owner.is_empty()
            && (stem == *owner
                || stem
                    .strip_prefix(owner)
                    .and_then(|rest| rest.strip_prefix('-'))
                    .is_some_and(|rest| !rest.is_empty()))
    })
}

/// True when `file` is a name an agent may be stored and served under: one
/// safe path component ending in `.md`.
fn is_agent_file_name(file: &str) -> bool {
    validate_name(file).is_ok()
        && Path::new(file)
            .extension()
            .is_some_and(|ext| ext == AGENT_EXTENSION)
}

/// The upstream agent files in `clone` that belong to `owners`, as
/// `(file name, resolved path)`, sorted. A symlinked entry is resolved and
/// kept only when it lands on a regular file inside the clone.
fn upstream_agents(clone: &Path, upstream: &str, owners: &[&str]) -> Vec<(String, PathBuf)> {
    let Ok(root) = fs::canonicalize(clone) else {
        return Vec::new();
    };
    let Ok(read) = fs::read_dir(clone.join(upstream)) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = read
        .filter_map(|dirent| dirent.ok())
        .filter_map(|dirent| {
            let file = dirent.file_name().to_string_lossy().into_owned();
            if !is_agent_file_name(&file) || !belongs_to(&file, owners) {
                return None;
            }
            let resolved = fs::canonicalize(dirent.path()).ok()?;
            (resolved.starts_with(&root) && resolved.is_file()).then_some((file, resolved))
        })
        .collect();
    found.sort();
    found
}

/// What one agent import pass produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentOutcome {
    /// Harness → tree hash of the stored agents, after the pass.
    pub recorded: BTreeMap<String, String>,
    /// Agent trees left alone because they carry local edits.
    pub refusals: Vec<Refusal>,
    /// What the pass did, or what a dry run would have done.
    pub plan: Vec<PlanOp>,
}

/// Store, refresh or drop the upstream agents of one pool entry from a clone
/// the variant pass already made.
///
/// `scratch` is the variant pass's scratch directory; the desired tree is
/// assembled there so it hashes exactly as the stored copy will. A stored tree
/// whose hash is not the recorded one was edited here and is refused, never
/// overwritten or deleted — the same guard `.variants` has.
pub fn import_from_clone(
    paths: &Paths,
    name: &str,
    dir_name: &str,
    record: &NpxMeta,
    clone: &Path,
    scratch: &Path,
    dry_run: bool,
) -> Result<AgentOutcome> {
    let mut outcome = AgentOutcome {
        recorded: record.harness_agents.clone(),
        ..AgentOutcome::default()
    };
    let owners: Vec<&str> = if dir_name == name {
        vec![name]
    } else {
        vec![name, dir_name]
    };

    for spec in AGENT_HARNESSES {
        let harness = spec.harness;
        let files = upstream_agents(clone, spec.upstream, &owners);
        let stored_at = format!("{name}/{AGENTS_DIR}/{harness}");
        let stored = stored_agents(paths, name, harness);
        let current = if stored.is_dir() {
            Some(committed_tree_hash(
                &paths.library,
                &stored_at,
                &stored,
                &[],
            )?)
        } else {
            None
        };
        let recorded = record.harness_agents.get(harness);
        let edited = current
            .as_ref()
            .is_some_and(|current| recorded != Some(current));

        if files.is_empty() {
            let Some(_) = current else {
                outcome.recorded.remove(harness);
                continue;
            };
            if edited {
                outcome.refusals.push(agent_refusal(name, harness));
                continue;
            }
            outcome.plan.push(PlanOp {
                op: "remove".to_string(),
                path: stored.clone(),
                target: None,
            });
            if !dry_run {
                remove_path(&stored)?;
                remove_if_empty(&paths.library.join(name).join(AGENTS_DIR))?;
            }
            outcome.recorded.remove(harness);
            continue;
        }

        let built = scratch.join(format!("{name}.agents-{harness}"));
        remove_path(&built)?;
        fs::create_dir_all(&built)
            .map_err(|err| Error::io(&format!("creating {}", built.display()), &err))?;
        for (file, source) in &files {
            fs::copy(source, built.join(file))
                .map_err(|err| Error::io(&format!("copying {}", source.display()), &err))?;
        }
        let desired = committed_tree_hash(&paths.library, &stored_at, &built, &[])?;

        if current.as_deref() == Some(desired.as_str()) {
            outcome.recorded.insert(harness.to_string(), desired);
            remove_path(&built)?;
            continue;
        }
        if edited {
            outcome.refusals.push(agent_refusal(name, harness));
            remove_path(&built)?;
            continue;
        }
        outcome.plan.push(PlanOp {
            op: if current.is_some() { "refresh" } else { "copy" }.to_string(),
            path: stored.clone(),
            target: Some(clone.join(spec.upstream)),
        });
        if !dry_run {
            crate::variants::copy_tree(&built, &stored, &[])?;
        }
        outcome.recorded.insert(harness.to_string(), desired);
        remove_path(&built)?;
    }
    Ok(outcome)
}

fn remove_if_empty(dir: &Path) -> Result<()> {
    let empty = fs::read_dir(dir).is_ok_and(|mut read| read.next().is_none());
    if !empty {
        return Ok(());
    }
    fs::remove_dir(dir).map_err(|err| Error::io(&format!("removing {}", dir.display()), &err))
}

fn agent_refusal(name: &str, harness: &str) -> Refusal {
    Refusal {
        name: name.to_string(),
        code: "local_edit_refused".to_string(),
        message: format!(
            "{name} carries a locally edited {AGENTS_DIR}/{harness} copy, which no upstream agent will overwrite; `skillherd remove {name} --force` discards it"
        ),
    }
}

// ---- render -------------------------------------------------------------

/// What happened in one harness's agents directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentSurfaceReport {
    /// The harness row name.
    pub harness: String,
    /// Agent file names linked (or relinked) this run.
    pub linked: Vec<String>,
    /// Agent links removed because no active skill provides them any more.
    pub removed: Vec<String>,
    pub unchanged: Vec<String>,
    /// Entries this tool does not own at a path an active skill's agent
    /// needs: reported, never replaced; the agent is not served.
    pub blocked: Vec<Entry>,
    /// Operations refused at apply time because the entry changed since the
    /// plan; nothing was deleted for them.
    pub refused: Vec<Refusal>,
    /// Active skills with imported agents for this harness that are not
    /// served because `.globals/agents` does not list them (serving is
    /// opt-in). Left out of the JSON when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<String>,
}

/// The pool file an agent link of ours points at, as `(skill, file)`, when
/// `entry` is exactly such a link for `harness`; `None` for anything else.
fn owned_link(entry: &Entry, paths: &Paths, harness: &str) -> Option<(String, String)> {
    let EntryKind::Symlink { target, .. } = &entry.kind else {
        return None;
    };
    let relative = normalize(target)
        .strip_prefix(&paths.library)
        .ok()?
        .to_path_buf();
    let parts: Vec<String> = relative
        .components()
        .map(|part| match part {
            Component::Normal(text) => Some(text.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Option<Vec<String>>>()?;
    match parts.as_slice() {
        [skill, dir, key, file]
            if dir == AGENTS_DIR
                && key == harness
                && *file == entry.name
                && validate_name(skill).is_ok() =>
        {
            Some((skill.clone(), file.clone()))
        }
        _ => None,
    }
}

/// The agent files the active skills provide: file name → source path. Two
/// skills providing the same file name is a collision: the first skill in
/// name order serves it and the collision is warned about.
///
/// Only skills in `opted_in` contribute; the others that have agents to offer
/// are returned in `held`.
fn desired_agents(
    paths: &Paths,
    harness: &str,
    skills: &[String],
    opted_in: &std::collections::BTreeSet<String>,
    held: &mut Vec<String>,
) -> Result<BTreeMap<String, PathBuf>> {
    let mut sorted: Vec<&String> = skills.iter().collect();
    sorted.sort();
    sorted.dedup();
    let mut desired: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    for skill in sorted {
        validate_name(skill)?;
        let dir = stored_agents(paths, skill, harness);
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<String> = read
            .filter_map(|dirent| dirent.ok())
            .filter(|dirent| dirent.path().is_file())
            .map(|dirent| dirent.file_name().to_string_lossy().into_owned())
            .filter(|file| is_agent_file_name(file))
            .collect();
        if files.is_empty() {
            continue;
        }
        if !opted_in.contains(skill.as_str()) {
            held.push(skill.clone());
            continue;
        }
        files.sort();
        for file in files {
            if let Some(first) = owner.get(&file) {
                output::warn(
                    "agent_collision",
                    format!(
                        "{skill} and {first} both provide the {harness} agent {file}; {first} serves it"
                    ),
                );
                continue;
            }
            owner.insert(file.clone(), skill.clone());
            desired.insert(file.clone(), dir.join(&file));
        }
    }
    Ok(desired)
}

/// Plan one harness's agents directory for the skills active on it.
///
/// `None` when agents are not served to this harness. `restrict` mirrors the
/// renderer's `--only`: the links of skills outside the run are not swept.
pub fn plan_agents(
    row: &HarnessRow,
    skills: &[String],
    paths: &Paths,
    restrict: bool,
) -> Result<Option<(PathBuf, Vec<PlannedOp>, AgentSurfaceReport)>> {
    if !AGENT_HARNESSES.iter().any(|spec| spec.harness == row.name) {
        return Ok(None);
    }
    let Some(dir) = served_dir(paths, &row.name) else {
        return Ok(None);
    };
    let opted_in = load_opt_in(paths)?;
    let mut held = Vec::new();
    let desired = desired_agents(paths, &row.name, skills, &opted_in, &mut held)?;
    let mut report = AgentSurfaceReport {
        harness: row.name.clone(),
        linked: Vec::new(),
        removed: Vec::new(),
        unchanged: Vec::new(),
        blocked: Vec::new(),
        refused: Vec::new(),
        held,
    };

    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    for entry in managed::list_entries(&dir, paths)? {
        entries.insert(entry.name.clone(), entry);
    }

    let mut ops = Vec::new();
    for (file, source) in &desired {
        let link = dir.join(file);
        let text = surfaces::link_text(&dir, source, &paths.home);
        let expect = match entries.get(file) {
            None => Expected::Absent,
            Some(entry) => match owned_link(entry, paths, &row.name) {
                Some(_)
                    if entry
                        .symlink_target()
                        .is_some_and(|target| normalize(target) == normalize(source)) =>
                {
                    report.unchanged.push(file.clone());
                    continue;
                }
                Some(_) => Expected::ManagedSymlink {
                    target: entry
                        .symlink_target()
                        .map(Path::to_path_buf)
                        .unwrap_or_default(),
                },
                None => {
                    report.blocked.push(entry.clone());
                    continue;
                }
            },
        };
        ops.push(PlannedOp {
            op: PlanOp {
                op: "link".to_string(),
                path: link,
                target: Some(text),
            },
            expect,
            copy: None,
        });
        report.linked.push(file.clone());
    }

    if !restrict {
        for (file, entry) in &entries {
            if desired.contains_key(file) || owned_link(entry, paths, &row.name).is_none() {
                continue;
            }
            let target = entry
                .symlink_target()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            ops.push(PlannedOp {
                op: PlanOp {
                    op: "remove".to_string(),
                    path: entry.path.clone(),
                    target: None,
                },
                expect: Expected::ManagedSymlink { target },
                copy: None,
            });
            report.removed.push(file.clone());
        }
    }
    Ok(Some((dir, ops, report)))
}

/// Apply an agents plan and fold the refusals into its report, so the report
/// never claims a change that did not happen.
pub fn apply(
    dir: &Path,
    ops: &[PlannedOp],
    paths: &Paths,
    report: &mut AgentSurfaceReport,
) -> Result<()> {
    let refusals = surfaces::apply_ops(dir, ops, paths)?;
    for refusal in &refusals {
        report.linked.retain(|name| name != &refusal.name);
        report.removed.retain(|name| name != &refusal.name);
    }
    report.refused = refusals;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_is_serialized_only_when_something_is_held() {
        let mut report = AgentSurfaceReport {
            harness: "claude".to_string(),
            linked: Vec::new(),
            removed: Vec::new(),
            unchanged: Vec::new(),
            blocked: Vec::new(),
            refused: Vec::new(),
            held: Vec::new(),
        };
        let quiet = serde_json::to_value(&report).unwrap();
        assert!(quiet.get("held").is_none(), "{quiet}");
        assert_eq!(quiet["linked"], serde_json::json!([]));
        let back: AgentSurfaceReport = serde_json::from_value(quiet).unwrap();
        assert!(back.held.is_empty());
        report.held.push("imp".to_string());
        assert_eq!(
            serde_json::to_value(&report).unwrap()["held"],
            serde_json::json!(["imp"])
        );
    }

    #[test]
    fn a_file_belongs_to_the_skill_it_is_named_for() {
        let owners = ["impeccable"];
        assert!(belongs_to("impeccable.md", &owners));
        assert!(belongs_to("impeccable-documenter.md", &owners));
        assert!(
            !belongs_to("impeccable-.md", &owners),
            "a bare dash names nothing"
        );
        assert!(
            !belongs_to("impeccables-x.md", &owners),
            "a longer word is not a prefix match"
        );
        assert!(!belongs_to("other-agent.md", &owners));
        assert!(!belongs_to("impeccable-documenter.toml", &owners));
        assert!(!belongs_to("impeccable-documenter.agent", &owners));
        assert!(belongs_to("design-audit.md", &["impeccable", "design"]));
        assert!(!belongs_to("x.md", &[""]));
    }

    #[test]
    fn only_an_exact_pool_agent_path_is_an_owned_link() {
        let paths = Paths::fixture("/sandbox/user");
        let dir = paths.claude_shared.join("agents");
        let link = |name: &str, target: PathBuf| Entry {
            name: name.to_string(),
            path: dir.join(name),
            kind: EntryKind::Symlink {
                target,
                dangling: false,
            },
            managed: true,
        };
        let pool = |rest: &str| paths.library.join(rest);

        let ours = link("imp-a.md", pool("imp/.harness-agents/claude/imp-a.md"));
        assert_eq!(
            owned_link(&ours, &paths, "claude"),
            Some(("imp".to_string(), "imp-a.md".to_string()))
        );
        for (name, target) in [
            ("imp-a.md", pool("imp/.harness-agents/cursor/imp-a.md")),
            ("imp-a.md", pool("imp/.harness-agents/claude/imp-b.md")),
            ("imp-a.md", pool("imp/imp-a.md")),
            ("imp-a.md", pool("imp/.variants/claude/imp-a.md")),
            (
                "imp-a.md",
                pool("imp/nested/.harness-agents/claude/imp-a.md"),
            ),
            ("imp-a.md", PathBuf::from("/elsewhere/imp-a.md")),
        ] {
            assert_eq!(
                owned_link(&link(name, target.clone()), &paths, "claude"),
                None,
                "{}",
                target.display()
            );
        }
        let file = Entry {
            name: "worker.md".to_string(),
            path: dir.join("worker.md"),
            kind: EntryKind::File,
            managed: false,
        };
        assert_eq!(owned_link(&file, &paths, "claude"), None);
    }
}
