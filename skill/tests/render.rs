//! End-to-end tests of surface rendering.
//!
//! Every test builds its own HOME: a pool under `~/.agents/skill-library` and
//! the six harness roots. Policies are built from `model.rs` types, so these
//! tests do not depend on the parser for `.globals/sets`.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use skillherd::hash::tree_hash;
use skillherd::model::{Policy, SetRow};
use skillherd::paths::Paths;
use skillherd::policy::default_policy;
use skillherd::render::{self, RenderOpts, RenderReport};
use skillherd::surfaces;

fn row(name: &str, main: bool, lane: bool) -> SetRow {
    SetRow {
        name: name.to_string(),
        main,
        lane,
        not_platform: Vec::new(),
        only: Vec::new(),
        not: Vec::new(),
        line: 1,
    }
}

struct Fixture {
    home: TempDir,
    paths: Paths,
}

impl Fixture {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        let paths = Paths::fixture(home.path());
        fs::create_dir_all(&paths.library).unwrap();
        fs::create_dir_all(&paths.agents_dir).unwrap();
        Self { home, paths }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn claude(&self) -> PathBuf {
        self.paths.claude_shared.join("skills")
    }

    fn agents(&self) -> PathBuf {
        self.paths.agents_dir.clone()
    }

    fn grok(&self) -> PathBuf {
        self.paths.grok_home.join("skills")
    }

    fn try_render(
        &self,
        surfaces: &[&str],
        dry_run: bool,
        policy: &Policy,
    ) -> skillherd::Result<RenderReport> {
        render::run_with_policy(
            &self.paths,
            RenderOpts {
                surfaces: surfaces.iter().map(|name| (*name).to_string()).collect(),
                dry_run,
                only: None,
            },
            policy,
        )
    }

    fn make_dir(&self, path: &Path) {
        fs::create_dir_all(path).unwrap();
    }

    /// A library skill: `<library>/<name>/SKILL.md`.
    fn skill(&self, name: &str, body: &str) {
        let dir = self.paths.library.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    /// A harness variant of an existing library skill.
    fn variant(&self, name: &str, key: &str, body: &str) {
        let dir = self.paths.library.join(name).join(".variants").join(key);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    /// A real directory in a surface that the tool does not own.
    fn foreign_dir(&self, surface: &Path, name: &str, body: &str) {
        let dir = surface.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    fn link(&self, path: &Path, target: &Path) {
        std::os::unix::fs::symlink(target, path).unwrap();
    }

    /// The default harness table plus these set rows.
    fn policy(&self, rows: Vec<SetRow>) -> Policy {
        let mut policy = default_policy(&self.paths);
        policy.rows = rows;
        policy
    }

    fn render(&self, surfaces: &[&str], dry_run: bool, policy: &Policy) -> RenderReport {
        render::run_with_policy(
            &self.paths,
            RenderOpts {
                surfaces: surfaces.iter().map(|name| (*name).to_string()).collect(),
                dry_run,
                only: None,
            },
            policy,
        )
        .unwrap()
    }
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn target_of(path: &Path) -> PathBuf {
    fs::read_link(path).unwrap_or_else(|err| panic!("{} is not a link: {err}", path.display()))
}

/// The relative link the renderer writes points at the variant, not the
/// universal copy, and the lane tier never leaks into a main surface.
#[test]
fn links_are_relative_and_serve_the_variant() {
    let fixture = Fixture::new();
    fixture.skill("plain", "# plain\n\nNEUTRAL\n");
    fixture.skill("varianted", "# varianted universal\n\nUNIVERSAL\n");
    fixture.variant(
        "varianted",
        "claude",
        "# varianted claude\n\nCLAUDE-VARIANT\n",
    );
    fixture.skill("laneonly", "# laneonly\n\nLANE\n");

    let policy = fixture.policy(vec![
        row("plain", true, false),
        row("varianted", true, false),
        row("laneonly", true, true),
    ]);
    let report = fixture.render(&["claude", "agents"], false, &policy);

    let claude = &report.surfaces[0];
    assert_eq!(claude.surface, "claude");
    assert_eq!(claude.linked, ["laneonly", "plain", "varianted"]);
    assert!(claude.removed.is_empty() && claude.refreshed.is_empty());
    assert!(claude.unchanged.is_empty());
    assert!(claude.foreign.is_empty());

    let link = target_of(&fixture.claude().join("varianted"));
    assert_eq!(
        link,
        PathBuf::from("../../.agents/skill-library/varianted/.variants/claude"),
        "the variant is served through a relative link"
    );
    assert_eq!(
        fs::canonicalize(fixture.claude().join("varianted")).unwrap(),
        fs::canonicalize(fixture.paths.library.join("varianted/.variants/claude")).unwrap()
    );
    assert_eq!(
        target_of(&fixture.claude().join("plain")),
        PathBuf::from("../../.agents/skill-library/plain")
    );

    let agents = &report.surfaces[1];
    assert_eq!(agents.surface, "agents");
    assert_eq!(
        agents.linked,
        ["laneonly"],
        "the lane tier serves the lane rows only"
    );
    assert!(
        !fixture.agents().join("varianted").exists(),
        "a main-only row never reaches the shared surface"
    );
    assert_eq!(
        target_of(&fixture.agents().join("laneonly")),
        PathBuf::from("../skill-library/laneonly")
    );

    assert_eq!(
        agents.stripper.as_ref().unwrap().unchanged,
        ["laneonly"],
        "nothing on the shared surface needs stripping"
    );
}

/// Only desired names are planned, so an unowned entry — even a link to a
/// fenced skill — is untouched.
#[test]
fn foreign_entries_are_listed_and_left_alone() {
    let fixture = Fixture::new();
    fixture.skill("plain", "# plain\n");
    fixture.skill(
        "fenced",
        "# fenced\n\n<!-- claude-only -->\nCLAUDE-TEXT\n<!-- /claude-only -->\n\nNEUTRAL-TEXT\n",
    );
    fixture.make_dir(&fixture.claude());
    fixture.foreign_dir(
        &fixture.claude(),
        "handmade",
        "# handmade\n\n<!-- mac-only -->\nMINE\n<!-- /mac-only -->\n",
    );
    let outside = fixture.home().join("Code/thing");
    fixture.make_dir(&outside);
    fs::write(
        outside.join("SKILL.md"),
        "# elsewhere\n\n<!-- cell-only -->\nTHEIRS\n<!-- /cell-only -->\n",
    )
    .unwrap();
    let elsewhere = fixture.claude().join("elsewhere");
    fixture.link(&elsewhere, &outside);

    let policy = fixture.policy(vec![row("plain", true, false), row("fenced", true, false)]);
    let report = fixture.render(&["claude"], false, &policy);

    let surface = &report.surfaces[0];
    let foreign: Vec<&str> = surface
        .foreign
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(foreign, ["elsewhere", "handmade"]);
    assert!(
        surface.foreign.iter().all(|entry| !entry.managed),
        "nothing foreign is ever managed"
    );
    assert_eq!(surface.linked, ["fenced", "plain"]);
    assert!(
        !surface.unchanged.contains(&"handmade".to_string()),
        "a foreign entry with a desired name is counted nowhere else"
    );

    // The foreign real directory is byte-identical: not stripped, not replaced.
    assert!(fixture.claude().join("handmade").is_dir());
    assert_eq!(
        fs::read_to_string(fixture.claude().join("handmade/SKILL.md")).unwrap(),
        "# handmade\n\n<!-- mac-only -->\nMINE\n<!-- /mac-only -->\n"
    );
    // The foreign link is still a link to its own fenced target: had it been
    // planned for, it would now be a served copy.
    assert!(
        exists(&elsewhere)
            && fs::symlink_metadata(&elsewhere)
                .unwrap()
                .file_type()
                .is_symlink()
    );
    assert_eq!(target_of(&elsewhere), outside);
    assert_eq!(
        fs::read_to_string(outside.join("SKILL.md")).unwrap(),
        "# elsewhere\n\n<!-- cell-only -->\nTHEIRS\n<!-- /cell-only -->\n",
        "nothing reached outside the surface"
    );
    // ... while the desired fenced skill was materialised.
    assert!(
        !fs::symlink_metadata(fixture.claude().join("fenced"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// What is not desired goes: a stale managed link, a dangling managed link, and
/// a desired name pointing at the wrong source.
#[test]
fn managed_entries_are_corrected_and_pruned() {
    let fixture = Fixture::new();
    fixture.skill("plain", "# plain\n");
    fixture.skill("oldname", "# oldname\n");
    fixture.skill("other", "# other\n");
    let claude = fixture.claude();
    fixture.make_dir(&claude);
    fixture.link(
        &claude.join("oldname"),
        Path::new("../../.agents/skill-library/oldname"),
    );
    fixture.link(
        &claude.join("ghost"),
        Path::new("../../.agents/skill-library/ghost"),
    );
    fixture.link(
        &claude.join("plain"),
        Path::new("../../.agents/skill-library/other"),
    );

    let policy = fixture.policy(vec![row("plain", true, false)]);
    let report = fixture.render(&["claude"], false, &policy);

    let surface = &report.surfaces[0];
    assert_eq!(surface.linked, ["plain"], "the wrong target is relinked");
    assert_eq!(surface.removed, ["ghost", "oldname"]);
    assert!(
        !exists(&claude.join("ghost")),
        "a dangling managed link goes"
    );
    assert!(
        !exists(&claude.join("oldname")),
        "a stale managed link goes"
    );
    assert_eq!(
        target_of(&claude.join("plain")),
        PathBuf::from("../../.agents/skill-library/plain")
    );
    assert_eq!(
        fs::canonicalize(claude.join("plain")).unwrap(),
        fs::canonicalize(fixture.paths.library.join("plain")).unwrap()
    );
}

/// `tier none` empties the managed entries of a surface and nothing else.
#[test]
fn a_tier_none_surface_empties_only_what_we_own() {
    let fixture = Fixture::new();
    fixture.skill("plain", "# plain\n");
    let grok = fixture.grok();
    fixture.make_dir(&grok);
    fixture.link(
        &grok.join("plain"),
        Path::new("../../.agents/skill-library/plain"),
    );
    fixture.foreign_dir(&grok, "handmade", "# handmade\n");

    let policy = fixture.policy(vec![row("plain", true, false)]);
    let report = fixture.render(&[], false, &policy);

    let grok_row = report
        .surfaces
        .iter()
        .find(|surface| surface.surface == "grok")
        .unwrap();
    assert_eq!(grok_row.removed, ["plain"]);
    assert!(grok_row.linked.is_empty() && grok_row.unchanged.is_empty());
    assert_eq!(grok_row.foreign.len(), 1);
    assert_eq!(grok_row.foreign[0].name, "handmade");
    assert!(!exists(&grok.join("plain")));
    assert!(grok.join("handmade/SKILL.md").is_file());

    let claude_row = report
        .surfaces
        .iter()
        .find(|surface| surface.surface == "claude")
        .unwrap();
    assert_eq!(
        claude_row.linked,
        ["plain"],
        "the main surface still serves it"
    );
}

/// A dry run reports the plan and writes nothing at all.
#[test]
fn a_dry_run_leaves_the_tree_byte_identical() {
    let fixture = Fixture::new();
    fixture.skill("plain", "# plain\n");
    fixture.skill(
        "fenced",
        "# fenced\n\n<!-- claude-only -->\nCLAUDE-TEXT\n<!-- /claude-only -->\n\nNEUTRAL\n",
    );
    fixture.skill("oldname", "# oldname\n");
    let claude = fixture.claude();
    fixture.make_dir(&claude);
    fixture.link(
        &claude.join("oldname"),
        Path::new("../../.agents/skill-library/oldname"),
    );
    fixture.foreign_dir(&claude, "handmade", "# handmade\n");

    let policy = fixture.policy(vec![row("plain", true, false), row("fenced", true, false)]);
    let before = tree_hash(fixture.home()).unwrap();
    let report = fixture.render(&[], true, &policy);
    let after = tree_hash(fixture.home()).unwrap();

    assert_eq!(before, after, "a dry run must not touch the tree");
    assert_eq!(report.surfaces.len(), 6);
    let claude_row = &report.surfaces[0];
    assert_eq!(claude_row.linked, ["fenced", "plain"]);
    assert_eq!(claude_row.removed, ["oldname"]);
    assert!(!exists(&claude.join("plain")), "nothing was linked");
    assert!(!exists(&claude.join("fenced")));
    assert!(exists(&claude.join("oldname")), "nothing was removed");
    let ops: Vec<&str> = report
        .plan
        .iter()
        .filter(|op| op.path.starts_with(&claude))
        .map(|op| op.op.as_str())
        .collect();
    assert!(ops.contains(&"link") && ops.contains(&"remove"), "{ops:?}");
}

/// A malformed fence is a portability failure: the render fails, 65, and it
/// fails before anything is written on any surface — no link of a good skill,
/// no removal of a stale one.
#[test]
fn a_malformed_fence_fails_the_render_before_anything_is_written() {
    let fixture = Fixture::new();
    fixture.skill("plain", "# plain\n");
    fixture.skill(
        "boom",
        "# boom\n\n<!-- cell-only -->\none\n<!-- mac-only -->\ntwo\n<!-- /mac-only -->\n<!-- /cell-only -->\n",
    );
    fixture.skill("oldname", "# oldname\n");
    let claude = fixture.claude();
    fixture.make_dir(&claude);
    fixture.link(
        &claude.join("oldname"),
        Path::new("../../.agents/skill-library/oldname"),
    );

    let policy = fixture.policy(vec![row("plain", true, true), row("boom", true, false)]);
    let before = tree_hash(fixture.home()).unwrap();
    let err = fixture.try_render(&[], false, &policy).unwrap_err();

    assert_eq!(err.code(), "render_failed");
    assert_eq!(err.exit_code(), 65);
    assert_eq!(err.details()["errors"][0]["name"], "boom");
    assert_eq!(
        err.details()["errors"][0]["message"],
        "SKILL.md: line 5: <!-- mac-only --> opened while <!-- cell-only --> block from line 3 is still open (nesting is not allowed)"
    );
    assert_eq!(
        err.details()["surface"],
        fixture.claude().display().to_string()
    );
    assert_eq!(
        tree_hash(fixture.home()).unwrap(),
        before,
        "a failed render writes nothing anywhere"
    );
}

/// A desired name the pool does not hold is skipped, not linked to nowhere.
#[test]
fn a_desired_name_with_no_pool_entry_is_skipped() {
    let fixture = Fixture::new();
    fixture.skill("plain", "# plain\n");

    let policy = fixture.policy(vec![row("plain", true, false), row("absent", true, false)]);
    let report = fixture.render(&["claude"], false, &policy);

    let surface = &report.surfaces[0];
    assert_eq!(surface.linked, ["plain"]);
    assert!(surface.unchanged.is_empty() && surface.removed.is_empty());
    assert!(!exists(&fixture.claude().join("absent")));
}

/// The contract: a second run of an unchanged pool and policy changes nothing,
/// including the fenced skill served as a copy on the first run.
#[test]
fn the_second_run_is_all_unchanged() {
    let fixture = Fixture::new();
    fixture.skill("plain", "# plain\n\nNEUTRAL\n");
    fixture.skill(
        "fenced",
        "---\nname: fenced\n---\n\n<!-- claude-only -->\nCLAUDE-TEXT\n<!-- /claude-only -->\n\nNEUTRAL-TEXT\n",
    );
    fixture.skill("varianted", "# varianted universal\n");
    fixture.variant("varianted", "claude", "# varianted claude\n");
    fixture.skill("laneonly", "# laneonly\n");
    fixture.make_dir(&fixture.claude());
    fixture.foreign_dir(&fixture.claude(), "handmade", "# handmade\n");

    let policy = fixture.policy(vec![
        row("plain", true, false),
        row("fenced", true, false),
        row("varianted", true, false),
        row("laneonly", true, true),
    ]);
    let first = fixture.render(&["claude", "agents"], false, &policy);
    assert_eq!(
        first.surfaces[0].linked,
        ["fenced", "laneonly", "plain", "varianted"]
    );
    assert_eq!(
        first.surfaces[1].linked,
        ["laneonly"],
        "the shared lane surface serves the lane rows only"
    );

    let fenced = fixture.claude().join("fenced");
    assert!(
        !fs::symlink_metadata(&fenced)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the fenced skill is served as a real directory"
    );
    let rendered = fs::read_to_string(fenced.join("SKILL.md")).unwrap();
    assert!(rendered.contains("CLAUDE-TEXT") && rendered.contains("NEUTRAL-TEXT"));
    assert!(
        !rendered.contains("<!--"),
        "no marker lines survive a render"
    );
    assert!(fenced.join(".skill-render.json").is_file());

    let second = fixture.render(&["claude", "agents"], false, &policy);
    for surface in &second.surfaces {
        assert!(
            surface.linked.is_empty() && surface.removed.is_empty() && surface.refreshed.is_empty(),
            "{} changed on the second run: {surface:?}",
            surface.surface
        );
    }
    assert_eq!(
        second.surfaces[0].unchanged,
        ["fenced", "laneonly", "plain", "varianted"]
    );
    assert_eq!(second.surfaces[1].unchanged, ["laneonly"]);
    assert_eq!(
        second.surfaces[0].stripper.as_ref().unwrap().unchanged,
        ["fenced", "laneonly", "plain", "varianted"],
        "no served name needed stripping work"
    );
    assert!(
        second.plan.is_empty(),
        "an unchanged tree plans no operations: {:?}",
        second.plan
    );
    assert!(fixture.claude().join("handmade/SKILL.md").is_file());
}

/// A harness-only change re-strips a served copy: the source is untouched, the
/// copy is rebuilt, and the surface reports `refreshed`.
#[test]
fn a_harness_change_refreshes_a_served_copy() {
    let fixture = Fixture::new();
    fixture.skill(
        "fenced",
        "<!-- claude-only -->\nCLAUDE-TEXT\n<!-- /claude-only -->\n\n\
         <!-- codex-only -->\nCODEX-TEXT\n<!-- /codex-only -->\n\nNEUTRAL-TEXT\n",
    );

    let mut policy = fixture.policy(vec![row("fenced", true, false)]);
    let first = fixture.render(&["claude"], false, &policy);
    assert_eq!(first.surfaces[0].linked, ["fenced"]);
    let fenced = fixture.claude().join("fenced");
    let rendered = fs::read_to_string(fenced.join("SKILL.md")).unwrap();
    assert!(rendered.contains("CLAUDE-TEXT") && !rendered.contains("CODEX-TEXT"));
    let recorded: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fenced.join(".skill-render.json")).unwrap())
            .unwrap();
    assert_eq!(recorded["harness"], "claude");
    assert_eq!(recorded["render_version"], 4);

    policy.harnesses[0].variant_key = Some("codex".to_string());
    let second = fixture.render(&["claude"], false, &policy);

    let surface = &second.surfaces[0];
    assert_eq!(surface.refreshed, ["fenced"], "the harness key changed");
    assert!(
        surface.unchanged.is_empty(),
        "a refreshed name is not also unchanged"
    );
    assert!(surface.linked.is_empty() && surface.removed.is_empty());
    let rendered = fs::read_to_string(fenced.join("SKILL.md")).unwrap();
    assert!(rendered.contains("CODEX-TEXT") && !rendered.contains("CLAUDE-TEXT"));
}

const FENCED: &str =
    "# fenced\n\n<!-- claude-only -->\nCLAUDE-TEXT\n<!-- /claude-only -->\n\nNEUTRAL-TEXT\n";

/// The names in `dir` that a render's staging or move-aside left behind.
fn leftovers(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".skill-render-"))
        .collect()
}

/// The served-copy symlink policy: a link inside the skill is materialised
/// only when it resolves inside the skill or the library. A link to a file
/// outside both (a credential, say) is left out of the copy, as is a link
/// back to one of its own ancestors; a link into another library skill is
/// copied as the file it points at.
#[test]
fn a_served_copy_follows_only_links_that_stay_in_the_library() {
    let fixture = Fixture::new();
    let outside = TempDir::new().unwrap();
    let secret = outside.path().join("id_ed25519");
    fs::write(&secret, "PRIVATE KEY\n").unwrap();
    fixture.skill("other", "# other\n");
    fs::write(fixture.paths.library.join("other/shared.md"), "SHARED\n").unwrap();
    fixture.skill("fenced", FENCED);
    let source = fixture.paths.library.join("fenced");
    fixture.link(&source.join("key"), &secret);
    fixture.link(&source.join("shared.md"), Path::new("../other/shared.md"));
    fixture.link(&source.join("loop"), Path::new("."));
    fixture.link(&source.join("gone"), Path::new("nowhere"));

    let policy = fixture.policy(vec![row("fenced", true, false)]);
    let report = fixture.render(&["claude"], false, &policy);
    assert_eq!(report.surfaces[0].linked, ["fenced"]);

    let copy = fixture.claude().join("fenced");
    assert!(copy.join(".skill-render.json").is_file());
    assert!(
        !exists(&copy.join("key")),
        "a link outside the skill and the library is not materialised"
    );
    assert!(!exists(&copy.join("loop")), "an ancestor link is left out");
    assert!(!exists(&copy.join("gone")), "a dangling link is left out");
    let shared = copy.join("shared.md");
    assert!(
        !fs::symlink_metadata(&shared).unwrap().is_symlink(),
        "a followed link lands as a regular file"
    );
    assert_eq!(fs::read_to_string(&shared).unwrap(), "SHARED\n");
    assert_eq!(
        fs::read_to_string(copy.join("SKILL.md")).unwrap(),
        "# fenced\n\nCLAUDE-TEXT\n\nNEUTRAL-TEXT\n"
    );
    assert!(leftovers(&fixture.claude()).is_empty());
}

/// Staging never writes through anything planted in the surface: a link at
/// the staging name the run would pick first is passed over, and what it
/// points at is not touched.
#[test]
fn a_planted_staging_link_is_not_written_through() {
    let fixture = Fixture::new();
    fixture.skill("fenced", FENCED);
    let claude = fixture.claude();
    fixture.make_dir(&claude);
    let outside = TempDir::new().unwrap();
    let planted = claude.join(format!(".skill-render-tmp-fenced-{}-0", std::process::id()));
    fixture.link(&planted, outside.path());

    let policy = fixture.policy(vec![row("fenced", true, false)]);
    let report = fixture.render(&["claude"], false, &policy);

    assert_eq!(report.surfaces[0].linked, ["fenced"]);
    assert!(claude.join("fenced/.skill-render.json").is_file());
    assert_eq!(
        fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing was written through the planted link"
    );
    assert_eq!(
        target_of(&planted),
        outside.path(),
        "the planted link stays"
    );
}

/// A served copy that is replaced by a user's own directory between planning
/// and applying is refused: the refresh deletes nothing and leaves no staging
/// directory behind.
#[test]
fn a_served_copy_swapped_after_planning_is_refused() {
    let fixture = Fixture::new();
    fixture.skill("fenced", FENCED);
    let policy = fixture.policy(vec![row("fenced", true, false)]);
    fixture.render(&["claude"], false, &policy);
    let claude = fixture.claude();
    let copy = claude.join("fenced");
    assert!(copy.join(".skill-render.json").is_file());

    // The source changes, so the next plan refreshes the copy.
    fs::write(
        fixture.paths.library.join("fenced/SKILL.md"),
        format!("{FENCED}MORE\n"),
    )
    .unwrap();
    let harness = policy.harness("claude").unwrap();
    let plan = surfaces::plan_surface(harness, &["fenced".to_string()], &fixture.paths).unwrap();
    assert_eq!(plan.report.refreshed, ["fenced"]);
    assert_eq!(plan.ops[0].op.op, "render");

    fs::remove_dir_all(&copy).unwrap();
    fixture.foreign_dir(&claude, "fenced", "# mine\n");
    let before = tree_hash(fixture.home()).unwrap();

    let refusals = surfaces::apply_ops(&claude, &plan.ops, &fixture.paths).unwrap();

    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert_eq!(refusals[0].code, "foreign_entry");
    assert!(
        refusals[0].message.contains("is no longer ours"),
        "{refusals:?}"
    );
    assert_eq!(
        fs::read_to_string(copy.join("SKILL.md")).unwrap(),
        "# mine\n"
    );
    assert!(leftovers(&claude).is_empty(), "{:?}", leftovers(&claude));
    assert_eq!(tree_hash(fixture.home()).unwrap(), before);
}

/// A source that gains markers turns its link into a served copy; one that
/// loses them turns the copy back into a link. Both are refreshes.
#[test]
fn a_source_moves_between_link_and_served_copy_as_its_markers_change() {
    let fixture = Fixture::new();
    fixture.skill("skill", "# skill\n");
    let policy = fixture.policy(vec![row("skill", true, false)]);
    let served = fixture.claude().join("skill");

    fixture.render(&["claude"], false, &policy);
    assert!(fs::symlink_metadata(&served).unwrap().is_symlink());

    fs::write(fixture.paths.library.join("skill/SKILL.md"), FENCED).unwrap();
    let report = fixture.render(&["claude"], false, &policy);
    assert_eq!(report.surfaces[0].refreshed, ["skill"]);
    assert_eq!(
        report.surfaces[0].stripper.as_ref().unwrap().rendered,
        ["skill"]
    );
    assert!(!fs::symlink_metadata(&served).unwrap().is_symlink());
    assert!(served.join(".skill-render.json").is_file());

    fs::write(
        fixture.paths.library.join("skill/SKILL.md"),
        "# plain again\n",
    )
    .unwrap();
    let report = fixture.render(&["claude"], false, &policy);
    assert_eq!(report.surfaces[0].refreshed, ["skill"]);
    assert_eq!(
        target_of(&served),
        Path::new("../../.agents/skill-library/skill")
    );
    assert!(leftovers(&fixture.claude()).is_empty());
}

/// `opts.only` narrows the run to the named skills and leaves the rest of the
/// surface — including what a full render would have pruned — alone.
#[test]
fn only_restricts_the_run_and_prunes_nothing() {
    let fixture = Fixture::new();
    fixture.skill("plain", "# plain\n");
    fixture.skill("oldname", "# oldname\n");
    let claude = fixture.claude();
    fixture.make_dir(&claude);
    fixture.link(
        &claude.join("oldname"),
        Path::new("../../.agents/skill-library/oldname"),
    );

    let policy = fixture.policy(vec![row("plain", true, false)]);
    let report = render::run_with_policy(
        &fixture.paths,
        RenderOpts {
            surfaces: vec!["claude".to_string()],
            dry_run: false,
            only: Some(vec!["plain".to_string()]),
        },
        &policy,
    )
    .unwrap();

    let surface = &report.surfaces[0];
    assert_eq!(surface.linked, ["plain"]);
    assert!(
        surface.removed.is_empty(),
        "a restricted run prunes nothing"
    );
    assert!(exists(&claude.join("oldname")));
}

/// A surface op re-proves ownership at the moment it runs: an entry another
/// process swapped in after the plan was made is refused, never deleted.
///
/// The plan is made, the tree is mutated, and only then is it applied — the
/// check/use window itself, driven through the two halves of the renderer.
#[test]
fn an_entry_that_changes_after_planning_is_refused_not_deleted() {
    let fixture = Fixture::new();
    fixture.skill("plain", "# plain\n");
    fixture.skill("other", "# other\n");
    let claude = fixture.claude();
    fixture.make_dir(&claude);
    // Planned for a relink: a managed link pointing at the wrong source.
    fixture.link(
        &claude.join("plain"),
        Path::new("../../.agents/skill-library/other"),
    );
    // Planned for removal: a stale managed link the desired set does not name.
    fixture.link(
        &claude.join("oldname"),
        Path::new("../../.agents/skill-library/oldname"),
    );

    let policy = fixture.policy(vec![row("plain", true, false)]);
    let harness = policy.harness("claude").unwrap();
    let plan = surfaces::plan_surface(harness, &["plain".to_string()], &fixture.paths).unwrap();
    let ops = plan.ops;
    assert_eq!(plan.report.linked, ["plain"], "the wrong target is planned");
    assert_eq!(
        plan.report.removed,
        ["oldname"],
        "the stale link is planned"
    );

    // Another process replaces both managed links with real user directories.
    for name in ["plain", "oldname"] {
        let path = claude.join(name);
        fs::remove_file(&path).unwrap();
        fixture.foreign_dir(&claude, name, "# mine\n");
    }
    let before = tree_hash(fixture.home()).unwrap();

    let refusals = surfaces::apply_ops(&claude, &ops, &fixture.paths).unwrap();

    assert_eq!(
        tree_hash(fixture.home()).unwrap(),
        before,
        "a refused op deletes and replaces nothing at all"
    );
    let names: Vec<&str> = refusals
        .iter()
        .map(|refusal| refusal.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["plain", "oldname"],
        "both ops are refused, in order"
    );
    assert!(
        refusals
            .iter()
            .all(|refusal| refusal.code == "foreign_entry"),
        "{refusals:?}"
    );
    assert!(
        refusals
            .iter()
            .all(|refusal| refusal.message.contains("is no longer ours")),
        "the refusal says what it found: {refusals:?}"
    );
    for name in ["plain", "oldname"] {
        let path = claude.join(name);
        assert!(
            fs::read_to_string(path.join("SKILL.md")).unwrap() == "# mine\n",
            "{} is byte-identical",
            path.display()
        );
        assert!(
            !fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

/// A dry run over a HOME with no surfaces at all still reports what it would
/// build, and creates nothing.
#[test]
fn a_dry_run_on_a_fresh_home_reports_the_plan() {
    let fixture = Fixture::new();
    fixture.skill("plain", "# plain\n");

    let policy = fixture.policy(vec![row("plain", true, false)]);
    let report = fixture.render(&[], true, &policy);

    assert_eq!(report.surfaces.len(), 6);
    let claude_row = &report.surfaces[0];
    assert_eq!(claude_row.linked, ["plain"]);
    assert_eq!(
        claude_row.stripper.as_ref().unwrap().unchanged,
        ["plain"],
        "the plain skill needs no stripping"
    );
    assert!(!exists(&fixture.claude()), "a dry run creates no surface");
    let grok_row = report
        .surfaces
        .iter()
        .find(|surface| surface.surface == "grok")
        .unwrap();
    assert!(grok_row.linked.is_empty(), "tier none builds nothing");
}

/// A real run takes the library lock before it reads the policy, so it can
/// never build surfaces from a snapshot a concurrent `on`/`off` has already
/// superseded. With the lock held by someone else, the run is `library_busy`
/// before the policy file is even opened — an unreadable policy proves the
/// order, because reading it first would fail on the policy instead.
#[test]
fn a_real_run_locks_before_it_reads_the_policy() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.paths.library.join(".globals")).unwrap();
    fs::write(
        fixture.paths.library.join(".globals/sets"),
        "plain main this-is-not-a-policy-token\n",
    )
    .unwrap();
    let held = skillherd::lockfile::lock_library(&fixture.paths).unwrap();

    let opts = || RenderOpts {
        surfaces: vec!["claude".to_string()],
        dry_run: false,
        only: None,
    };
    let error = render::run(&fixture.paths, opts()).unwrap_err();
    assert_eq!(error.code(), "library_busy", "{error}");

    drop(held);
    let error = render::run(&fixture.paths, opts()).unwrap_err();
    assert_ne!(
        error.code(),
        "library_busy",
        "with the lock free, the unreadable policy is what fails: {error}"
    );
}

/// The real binary under a scratch HOME with nothing inherited but `PATH`.
fn skill_bin(home: &Path) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("skillherd").unwrap();
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    cmd
}

/// `npx skills add -g` installs into the shared `~/.agents/skills` and links
/// `~/.claude/skills/<name>` to it. That link is another tool's: the first
/// `skillherd on` of an unrelated skill must leave it exactly as it was, and must
/// not claim to have removed it.
#[test]
fn an_npx_global_link_survives_skill_on() {
    let home = TempDir::new().unwrap();
    let library = home.path().join(".agents/skill-library");
    fs::create_dir_all(library.join("foo")).unwrap();
    fs::write(library.join("foo/SKILL.md"), "# foo\n").unwrap();
    let installed = home.path().join(".agents/skills/web-design-guidelines");
    fs::create_dir_all(&installed).unwrap();
    fs::write(installed.join("SKILL.md"), "# from npx\n").unwrap();
    let claude = home.path().join(".claude/skills");
    fs::create_dir_all(&claude).unwrap();
    let link = claude.join("web-design-guidelines");
    std::os::unix::fs::symlink("../../.agents/skills/web-design-guidelines", &link).unwrap();

    let output = skill_bin(home.path()).args(["on", "foo"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "skill on failed: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    for surface in envelope["data"]["render"]["surfaces"].as_array().unwrap() {
        assert!(
            !surface["removed"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == "web-design-guidelines"),
            "the npx link is reported removed: {surface}"
        );
    }
    assert_eq!(
        target_of(&link),
        PathBuf::from("../../.agents/skills/web-design-guidelines"),
        "the npx link is untouched"
    );
    assert_eq!(
        fs::read_to_string(installed.join("SKILL.md")).unwrap(),
        "# from npx\n"
    );
    assert_eq!(
        target_of(&claude.join("foo")),
        PathBuf::from("../../.agents/skill-library/foo"),
        "the activated skill is served beside it"
    );
}

/// An imported Claude agent of a library skill.
/// List `skills` in `.globals/agents`, the opt-in for serving their agents.
fn opt_in_agents(fixture: &Fixture, skills: &[&str]) {
    let globals = fixture.paths.library.join(".globals");
    fs::create_dir_all(&globals).unwrap();
    let mut text = String::from("# skills whose agents are served\n");
    for skill in skills {
        text.push_str(skill);
        text.push('\n');
    }
    fs::write(globals.join("agents"), text).unwrap();
}

fn pool_agent(fixture: &Fixture, skill: &str, file: &str, body: &str) -> PathBuf {
    let dir = fixture
        .paths
        .library
        .join(skill)
        .join(".harness-agents/claude");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(file), body).unwrap();
    dir.join(file)
}

fn sorted_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|dirent| dirent.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The agents of every skill served to Claude are linked into the shared
/// agents directory; a file render did not create is never replaced, a link
/// into the pool that is not an agent link of ours is never touched, and
/// turning the skill off removes exactly the links render made.
#[test]
fn agents_of_served_skills_link_into_the_pool_and_nothing_else_is_touched() {
    let fixture = Fixture::new();
    fixture.skill("imp", "# imp\n");
    fixture.skill("other", "# other\n");
    for file in ["imp-a.md", "imp-b.md", "imp-c.md"] {
        pool_agent(&fixture, "imp", file, &format!("agent {file}\n"));
    }
    opt_in_agents(&fixture, &["imp"]);

    let dir = fixture.paths.claude_shared.join("agents");
    fixture.make_dir(&dir);
    fs::write(dir.join("worker.md"), "a house agent\n").unwrap();
    fs::write(dir.join("imp-b.md"), "hand placed\n").unwrap();
    // Managed under the surface-wide rule, but not an agent link of ours.
    fixture.link(
        &dir.join("stray.md"),
        &fixture.paths.library.join("imp/SKILL.md"),
    );
    // As deep as an agent link, but into a variant tree: not ours either.
    fixture.link(
        &dir.join("variant.md"),
        &fixture.paths.library.join("imp/.variants/claude/SKILL.md"),
    );
    // Ours, pointing at the wrong skill: relinked.
    fixture.link(
        &dir.join("imp-c.md"),
        &fixture
            .paths
            .library
            .join("old/.harness-agents/claude/imp-c.md"),
    );
    // Ours, provided by nothing served: removed.
    fixture.link(
        &dir.join("gone.md"),
        &fixture
            .paths
            .library
            .join("gone/.harness-agents/claude/gone.md"),
    );

    let policy = fixture.policy(vec![row("imp", true, false)]);
    let report = fixture.render(&["claude"], false, &policy);
    assert_eq!(report.agents.len(), 1);
    let agents = &report.agents[0];
    assert_eq!(agents.harness, "claude");
    assert_eq!(agents.linked, ["imp-a.md", "imp-c.md"]);
    assert_eq!(agents.removed, ["gone.md"]);
    let blocked: Vec<&str> = agents
        .blocked
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(blocked, ["imp-b.md"]);
    assert!(agents.refused.is_empty());
    for file in ["imp-a.md", "imp-c.md"] {
        assert_eq!(
            target_of(&dir.join(file)),
            PathBuf::from(format!(
                "../../.agents/skill-library/imp/.harness-agents/claude/{file}"
            ))
        );
    }
    assert_eq!(
        fs::read_to_string(dir.join("imp-b.md")).unwrap(),
        "hand placed\n",
        "a file render did not create is never replaced"
    );
    assert_eq!(
        fs::read_to_string(dir.join("worker.md")).unwrap(),
        "a house agent\n"
    );
    assert_eq!(
        target_of(&dir.join("stray.md")),
        fixture.paths.library.join("imp/SKILL.md")
    );
    assert_eq!(
        target_of(&dir.join("variant.md")),
        fixture.paths.library.join("imp/.variants/claude/SKILL.md")
    );
    assert!(!exists(&dir.join("gone.md")));

    // Idempotent.
    let again = fixture.render(&["claude"], false, &policy);
    assert!(again.plan.is_empty(), "{:?}", again.plan);
    assert_eq!(again.agents[0].unchanged, ["imp-a.md", "imp-c.md"]);
    assert!(again.agents[0].linked.is_empty() && again.agents[0].removed.is_empty());

    // Another harness serves no agents.
    let codex = fixture.render(&["codex"], false, &policy);
    assert!(codex.agents.is_empty());

    // Off: exactly the links render made go.
    let off = fixture.policy(vec![row("other", true, false)]);
    let report = fixture.render(&["claude"], false, &off);
    assert_eq!(report.agents[0].removed, ["imp-a.md", "imp-c.md"]);
    assert_eq!(
        sorted_names(&dir),
        ["imp-b.md", "stray.md", "variant.md", "worker.md"]
    );
}

#[test]
fn a_dry_run_plans_agent_links_and_writes_nothing() {
    let fixture = Fixture::new();
    fixture.skill("imp", "# imp\n");
    pool_agent(&fixture, "imp", "imp-a.md", "agent\n");
    opt_in_agents(&fixture, &["imp"]);
    let policy = fixture.policy(vec![row("imp", true, false)]);

    let before = tree_hash(fixture.home()).unwrap();
    let report = fixture.render(&["claude"], true, &policy);
    let link = fixture.paths.claude_shared.join("agents/imp-a.md");
    assert!(
        report
            .plan
            .iter()
            .any(|op| op.op == "link" && op.path == link),
        "{:?}",
        report.plan
    );
    assert_eq!(report.agents[0].linked, ["imp-a.md"]);
    assert_eq!(tree_hash(fixture.home()).unwrap(), before);
}

/// Serving a skill's imported agents is opt-in: an active skill
/// whose agents `.globals/agents` does not list is reported `held` and serves
/// none; listing it serves them; unlisting it again removes the links.
#[test]
fn agents_are_served_only_for_skills_opted_in() {
    let fixture = Fixture::new();
    fixture.skill("imp", "# imp\n");
    pool_agent(&fixture, "imp", "imp-a.md", "agent\n");
    let policy = fixture.policy(vec![row("imp", true, false)]);
    let link = fixture.paths.claude_shared.join("agents/imp-a.md");

    let report = fixture.render(&["claude"], false, &policy);
    assert_eq!(report.agents[0].held, ["imp"]);
    assert!(report.agents[0].linked.is_empty());
    assert!(!exists(&link), "nothing is served without the opt-in");
    assert!(exists(&fixture.claude().join("imp")), "the skill itself is");

    opt_in_agents(&fixture, &["imp"]);
    let report = fixture.render(&["claude"], false, &policy);
    assert!(report.agents[0].held.is_empty());
    assert_eq!(report.agents[0].linked, ["imp-a.md"]);
    assert!(exists(&link));

    opt_in_agents(&fixture, &[]);
    let report = fixture.render(&["claude"], false, &policy);
    assert_eq!(report.agents[0].removed, ["imp-a.md"]);
    assert_eq!(report.agents[0].held, ["imp"]);
    assert!(!exists(&link));
}

/// Run the binary in `home` and parse its one envelope (success or error).
fn run_skill(home: &Path, args: &[&str]) -> (i32, serde_json::Value) {
    let output = skill_bin(home).args(args).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let envelope = serde_json::from_str(stdout.trim())
        .or_else(|_| serde_json::from_str(stderr.trim()))
        .unwrap_or_else(|err| panic!("no envelope ({err}): {stdout:?} {stderr:?}"));
    (output.status.code().unwrap(), envelope)
}

/// `skillherd on` of a skill whose fence is malformed fails before it writes
/// anything: no `sets` row, no link of the other names, nothing on any
/// surface. Preflight must finish before either the row or links are written.
#[test]
fn on_with_a_malformed_fence_writes_neither_the_row_nor_a_link() {
    let home = TempDir::new().unwrap();
    let library = home.path().join(".agents/skill-library");
    for (name, body) in [
        ("good", "# good\n"),
        (
            "boom",
            "# boom\n\n<!-- cell-only -->\none\n<!-- mac-only -->\ntwo\n<!-- /mac-only -->\n<!-- /cell-only -->\n",
        ),
    ] {
        fs::create_dir_all(library.join(name)).unwrap();
        fs::write(library.join(name).join("SKILL.md"), body).unwrap();
    }
    let before = tree_hash(home.path()).unwrap();

    let (code, envelope) = run_skill(home.path(), &["on", "good", "boom"]);

    assert_eq!(code, 65, "{envelope}");
    assert_eq!(envelope["error"]["code"], "render_failed", "{envelope}");
    assert!(!library.join(".globals/sets").exists(), "no row was saved");
    assert!(!exists(&home.path().join(".claude/skills/good")), "no link");
    // The lock file is the only thing a failed real run may leave.
    let _ = fs::remove_file(home.path().join(".agents/skill-library/.skill.lock"));
    assert_eq!(tree_hash(home.path()).unwrap(), before, "nothing changed");
}

/// `--only` naming a harness that serves nothing (grok has tier `none` in
/// the default table) is refused instead of saving a row nothing reads and
/// exiting 0.
#[test]
fn on_only_for_a_harness_that_serves_nothing_is_refused() {
    let home = TempDir::new().unwrap();
    let library = home.path().join(".agents/skill-library");
    fs::create_dir_all(library.join("foo")).unwrap();
    fs::write(library.join("foo/SKILL.md"), "# foo\n").unwrap();

    let (code, envelope) = run_skill(home.path(), &["on", "foo", "--only", "grok"]);

    assert_eq!(code, 65, "{envelope}");
    assert_eq!(envelope["error"]["code"], "policy_unrepresentable");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("only:grok (tier none)") && message.contains("no harness would see it"),
        "{message}"
    );
    assert!(!library.join(".globals/sets").exists(), "no row was saved");

    // A harness with a surface is fine.
    let (code, envelope) = run_skill(home.path(), &["on", "foo", "--only", "claude"]);
    assert_eq!(code, 0, "{envelope}");
}

/// `--json` is accepted by every command and keeps the envelope; piped output
/// is the envelope with or without it.
#[test]
fn render_json_flag_keeps_the_envelope() {
    let home = TempDir::new().unwrap();
    fs::create_dir_all(home.path().join(".agents/skill-library")).unwrap();
    for args in [&["render", "--json"][..], &["render"][..]] {
        let (code, envelope) = run_skill(home.path(), args);
        assert_eq!(code, 0, "{envelope}");
        assert_eq!(envelope["ok"], true, "{args:?}");
        assert!(envelope["data"]["surfaces"].is_array());
    }
}

/// Every file under `dir`, as `(relative path, contents)`, links followed.
fn files_under(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        for entry in fs::read_dir(dir.join(&rel)).unwrap() {
            let entry = entry.unwrap();
            let child = rel.join(entry.file_name());
            let meta = fs::metadata(entry.path()).unwrap();
            if meta.is_dir() {
                stack.push(child);
            } else {
                found.push((child, fs::read(entry.path()).unwrap()));
            }
        }
    }
    found.sort();
    found
}

fn paths_under(dir: &Path) -> Vec<PathBuf> {
    files_under(dir).into_iter().map(|(rel, _)| rel).collect()
}

fn warning_codes() -> Vec<String> {
    skillherd::output::warnings()
        .into_iter()
        .map(|warning| warning.code)
        .collect()
}

/// A `.variants/<harness>` that resolves outside its skill (a link to a
/// directory holding a secret, or `../..`, the library itself) is ignored:
/// the surface serves the universal tree, never the outside one, and says so
/// in a warning. A variant that is a link to a tree inside the skill is still
/// served.
#[test]
fn a_variant_that_leaves_its_skill_is_ignored() {
    let fixture = Fixture::new();
    let library = fixture.paths.library.clone();
    let outside = TempDir::new().unwrap();
    let fenced_out = outside.path().join("fenced");
    fs::create_dir_all(&fenced_out).unwrap();
    fs::write(fenced_out.join("SKILL.md"), FENCED).unwrap();
    fs::write(fenced_out.join("secret.txt"), "PRIVATE KEY\n").unwrap();
    let plain_out = outside.path().join("plain");
    fs::create_dir_all(&plain_out).unwrap();
    fs::write(plain_out.join("SKILL.md"), "# outside\n").unwrap();

    fixture.skill("other", "# other\n\nOTHER-SKILL\n");
    fixture.skill("secretvar", FENCED);
    fixture.make_dir(&library.join("secretvar/.variants"));
    fixture.link(&library.join("secretvar/.variants/claude"), &fenced_out);
    fixture.skill("poolvar", FENCED);
    fixture.make_dir(&library.join("poolvar/.variants"));
    fixture.link(
        &library.join("poolvar/.variants/claude"),
        Path::new("../.."),
    );
    fixture.skill("plainvar", "# plainvar\n");
    fixture.make_dir(&library.join("plainvar/.variants"));
    fixture.link(&library.join("plainvar/.variants/claude"), &plain_out);
    fixture.skill("innervar", "# innervar universal\n");
    fixture.make_dir(&library.join("innervar/claude-tree"));
    fs::write(
        library.join("innervar/claude-tree/SKILL.md"),
        "# innervar claude\n",
    )
    .unwrap();
    fixture.make_dir(&library.join("innervar/.variants"));
    fixture.link(
        &library.join("innervar/.variants/claude"),
        Path::new("../claude-tree"),
    );

    skillherd::output::clear_warnings();
    let policy = fixture.policy(vec![
        row("secretvar", true, false),
        row("poolvar", true, false),
        row("plainvar", true, false),
        row("innervar", true, false),
    ]);
    fixture.render(&["claude"], false, &policy);
    let claude = fixture.claude();

    let secret_copy = claude.join("secretvar");
    assert!(secret_copy.join(".skill-render.json").is_file());
    assert!(
        !files_under(&secret_copy)
            .iter()
            .any(|(_, bytes)| bytes == b"PRIVATE KEY\n"),
        "the outside variant's secret is not copied: {:?}",
        paths_under(&secret_copy)
    );
    assert_eq!(
        fs::read_to_string(secret_copy.join("SKILL.md")).unwrap(),
        "# fenced\n\nCLAUDE-TEXT\n\nNEUTRAL-TEXT\n",
        "the universal tree is served"
    );
    let marker: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(secret_copy.join(".skill-render.json")).unwrap())
            .unwrap();
    assert_eq!(
        PathBuf::from(marker["source_dir"].as_str().unwrap()),
        fs::canonicalize(library.join("secretvar")).unwrap()
    );

    let pool_copy = claude.join("poolvar");
    assert!(
        !files_under(&pool_copy)
            .iter()
            .any(|(_, bytes)| bytes == b"# other\n\nOTHER-SKILL\n"),
        "the rest of the pool is not copied: {:?}",
        paths_under(&pool_copy)
    );

    assert_eq!(
        fs::canonicalize(claude.join("plainvar")).unwrap(),
        fs::canonicalize(library.join("plainvar")).unwrap(),
        "a plain skill links to its universal tree, never outside"
    );
    assert_eq!(
        fs::read_to_string(claude.join("innervar/SKILL.md")).unwrap(),
        "# innervar claude\n",
        "a variant link that stays inside the skill is served"
    );

    let codes = warning_codes();
    assert_eq!(
        codes
            .iter()
            .filter(|code| *code == "variant_outside_skill")
            .count(),
        3,
        "{codes:?}"
    );
}

/// A directory link back to the library (`up -> ..`, or the library's
/// absolute path) or, inside a variant, back to the skill is left out of a
/// served copy, so the copy never holds the rest of the pool or of the skill.
/// A file link to a sibling skill is still copied.
#[test]
fn a_link_back_to_the_library_or_the_skill_is_not_copied() {
    let fixture = Fixture::new();
    let library = fixture.paths.library.clone();
    fixture.skill("other", "# other\n\nOTHER-SKILL\n");
    fixture.skill("fenced", FENCED);
    let source = library.join("fenced");
    fixture.link(&source.join("up"), Path::new(".."));
    fixture.link(&source.join("pool"), &library);
    fixture.link(&source.join("shared.md"), Path::new("../other/SKILL.md"));
    fixture.skill("varied", "# varied universal\n\nUNIVERSAL-ONLY\n");
    let variant = library.join("varied/.variants/claude");
    fixture.make_dir(&variant);
    fs::write(variant.join("SKILL.md"), FENCED).unwrap();
    fixture.link(&variant.join("skill"), Path::new("../.."));
    fixture.link(&variant.join("variants"), Path::new(".."));

    let policy = fixture.policy(vec![row("fenced", true, false), row("varied", true, false)]);
    fixture.render(&["claude"], false, &policy);
    let claude = fixture.claude();

    let copy = claude.join("fenced");
    assert!(copy.join(".skill-render.json").is_file());
    assert!(!exists(&copy.join("up")), "{:?}", paths_under(&copy));
    assert!(!exists(&copy.join("pool")), "{:?}", paths_under(&copy));
    assert_eq!(
        fs::read_to_string(copy.join("shared.md")).unwrap(),
        "# other\n\nOTHER-SKILL\n",
        "a file link to a sibling skill is copied"
    );

    let varied = claude.join("varied");
    assert!(varied.join(".skill-render.json").is_file());
    assert!(!exists(&varied.join("skill")), "{:?}", paths_under(&varied));
    assert!(
        !exists(&varied.join("variants")),
        "{:?}",
        paths_under(&varied)
    );
    assert!(
        !files_under(&varied)
            .iter()
            .any(|(_, bytes)| bytes.windows(14).any(|w| w == b"UNIVERSAL-ONLY")),
        "{:?}",
        paths_under(&varied)
    );
}

/// Turning a name off when its pool entry is a link to the served copy itself
/// would delete the only tree: the removal is refused and the tree stays. A
/// pool entry that links to a separate development repo is still removed from
/// the surface as usual, and the repo is untouched.
#[test]
fn a_served_copy_that_is_its_own_pool_entry_is_not_deleted() {
    let fixture = Fixture::new();
    let library = fixture.paths.library.clone();
    let dev = TempDir::new().unwrap();
    let repo = dev.path().join("devskill");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("SKILL.md"), FENCED).unwrap();
    fixture.link(&library.join("devskill"), &repo);
    fixture.skill("fenced", FENCED);
    let policy = fixture.policy(vec![
        row("fenced", true, false),
        row("devskill", true, false),
    ]);
    fixture.render(&["claude"], false, &policy);
    let claude = fixture.claude();
    let copy = claude.join("fenced");
    assert!(copy.join(".skill-render.json").is_file());
    assert!(claude.join("devskill/.skill-render.json").is_file());

    fs::remove_dir_all(library.join("fenced")).unwrap();
    fixture.link(&library.join("fenced"), &copy);
    fs::write(copy.join("notes.md"), "ONLY COPY\n").unwrap();

    let report = fixture.render(&["claude"], false, &fixture.policy(Vec::new()));

    assert_eq!(
        fs::read_to_string(copy.join("notes.md")).unwrap(),
        "ONLY COPY\n",
        "the only tree survives"
    );
    let refused: Vec<&str> = report.surfaces[0]
        .refused
        .iter()
        .map(|refusal| refusal.name.as_str())
        .collect();
    assert_eq!(refused, ["fenced"], "{:?}", report.surfaces[0]);
    assert!(
        !exists(&claude.join("devskill")),
        "a dev-repo pool entry's served copy is removed"
    );
    assert_eq!(fs::read_to_string(repo.join("SKILL.md")).unwrap(), FENCED);
    assert!(leftovers(&claude).is_empty(), "{:?}", leftovers(&claude));
}
