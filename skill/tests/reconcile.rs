//! Reconcile, staged npx and variant import, end to end in a temp HOME.
//!
//! Every test builds a real HOME: the library is a real git repository with a
//! seeded `.gitignore` and `sets`, the inbox is an owned run stage, the live lock is a
//! fake `skills` lock, and the upstream is a local bare repository the lock
//! points at through a `file://` URL. The `fixtures/` scripts build the
//! upstream and stand in for `npx`.
//!
//! The staged-npx tests use the stub, which records the `HOME` and argv it was
//! given and then does what `npx skills …` would have done in that stage.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use skillherd::cli::{InstallArgs, ReconcileArgs, RemoveArgs, UpdateArgs};
use skillherd::commands::{install, reconcile as reconcile_cmd, remove, update};
use skillherd::hash::tree_hash;
use skillherd::model::NpxMeta;
use skillherd::paths::Paths;
use skillherd::reconcile::{self, ReconcileOpts};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn piece(dir: &Path, name: &str, body: &str) -> PathBuf {
    let skill = dir.join(name);
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), piece_body(name, body)).unwrap();
    skill
}

fn names(imported: &[skillherd::model::Imported]) -> Vec<String> {
    imported.iter().map(|row| row.name.clone()).collect()
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// Move `library/<name>` into `.npx/backup/<name>` the way a crash between
/// the displace and the publish leaves it.
fn displace(env: &Env, name: &str) -> PathBuf {
    let backup = env.paths.library.join(".npx/backup").join(name);
    fs::create_dir_all(backup.parent().unwrap()).unwrap();
    fs::rename(env.paths.library.join(name), &backup).unwrap();
    backup
}

fn warning_codes() -> Vec<String> {
    skillherd::output::warnings()
        .iter()
        .map(|warning| warning.code.clone())
        .collect()
}

struct Env {
    home: TempDir,
    paths: Paths,
    log: PathBuf,
}

impl Env {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        let mut paths = Paths::fixture(home.path());
        let bin = home.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let stub = bin.join("npx");
        fs::copy(format!("{FIXTURES}/npx-stub.sh"), &stub).unwrap();
        let mode = fs::metadata(&stub).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&stub, fs::Permissions::from_mode(mode.mode() | 0o755)).unwrap();
        }
        paths.npx = stub;
        let env = Self {
            log: bin.join("npx.log"),
            home,
            paths,
        };
        env.seed_library();
        env
    }

    fn seed_library(&self) {
        let library = &self.paths.library;
        fs::create_dir_all(library.join(".globals")).unwrap();
        fs::write(
            library.join(".gitignore"),
            "# bridge lines (never commit these\ngit)\nbridge-one\nbridge-two\n# a house note\n",
        )
        .unwrap();
        fs::write(
            library.join(".globals/sets"),
            "# curated 2026-09-21\nhouse  main\npost  main lane\n",
        )
        .unwrap();
        piece(&library.join("house"), "house", "house copy");
        self.git(&["init", "-q"]);
        self.git(&["config", "user.email", "fixture@example.invalid"]);
        self.git(&["config", "user.name", "fixture"]);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "seed"]);
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.paths.library)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn commits(&self) -> Vec<String> {
        self.git(&["log", "--format=%s"])
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn inbox_root(&self) -> PathBuf {
        self.paths
            .stage
            .join("run-fixture")
            .join(skillherd::npx::STAGE_INBOX)
    }

    fn seed_inbox(&self) {
        let root = self.paths.stage.join("run-fixture");
        fs::create_dir_all(self.inbox_root()).unwrap();
        // A real npx stage carries its own lock snapshot. Recovery may use
        // the upstream hash to prove this is the currently recorded arrival.
        if self.paths.lock.is_file() {
            fs::copy(&self.paths.lock, root.join(skillherd::npx::STAGE_LOCK)).unwrap();
        }
        for dir in [&self.paths.stage, &root] {
            fs::write(
                dir.join(skillherd::npx::STAGE_MARKER),
                "skill-stage-owner/v1 skill 00000000000000000000000000000000\n",
            )
            .unwrap();
        }
    }

    fn inbox(&self, name: &str, body: &str) -> PathBuf {
        self.seed_inbox();
        piece(&self.inbox_root(), name, body)
    }

    fn entry(&self, name: &str, body: &str) -> PathBuf {
        piece(&self.paths.library, name, body)
    }

    fn record_path(&self, name: &str) -> PathBuf {
        self.paths.library.join(".npx").join(format!("{name}.json"))
    }

    fn record(&self, name: &str) -> NpxMeta {
        let text = fs::read_to_string(self.record_path(name)).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn entry_hash(&self, name: &str) -> String {
        tree_hash(&self.paths.library.join(name)).unwrap()
    }

    /// Write the live `skills` lock: one entry per `(name, source url)`.
    fn lock(&self, entries: &[(&str, &str)]) {
        let skills: BTreeMap<String, serde_json::Value> = entries
            .iter()
            .map(|(name, url)| {
                (
                    (*name).to_string(),
                    serde_json::json!({
                        "source": "fixture",
                        "sourceType": "github",
                        "sourceUrl": url,
                        "skillPath": format!(".agents/skills/{name}/SKILL.md"),
                        "skillFolderHash": format!("stub-{name}"),
                    }),
                )
            })
            .collect();
        if let Some(parent) = self.paths.lock.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(
            &self.paths.lock,
            serde_json::to_string(&serde_json::json!({ "version": 3, "skills": skills })).unwrap(),
        )
        .unwrap();
    }

    fn live_lock(&self) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(&self.paths.lock).unwrap()).unwrap()
    }

    fn fixture(args: &[&str]) {
        let output = Command::new(format!("{FIXTURES}/make-upstream.sh"))
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "make-upstream {args:?}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn upstream_work(&self) -> PathBuf {
        self.home().join("upstream")
    }

    /// Build (or rebuild) the upstream work tree for `name`.
    fn upstream_work_tree(&self, name: &str, variant: &str) -> PathBuf {
        let work = self.upstream_work();
        Self::fixture(&["init", &work.to_string_lossy(), name, variant]);
        work
    }

    /// Publish the work tree into `bare` (replacing it).
    fn publish_work(&self, bare: &Path) {
        Self::fixture(&[
            "publish",
            &self.upstream_work().to_string_lossy(),
            &bare.to_string_lossy(),
        ]);
    }

    /// Build the upstream for `name` at `bare` and return it.
    fn publish_at(&self, name: &str, variant: &str, bare: &Path) -> PathBuf {
        self.upstream_work_tree(name, variant);
        self.publish_work(bare);
        bare.to_path_buf()
    }

    /// Build the upstream for `name` at its default bare path.
    fn upstream(&self, name: &str, variant: &str) -> PathBuf {
        self.publish_at(name, variant, &self.home().join(format!("{name}.git")))
    }

    fn conf(&self, lines: &[&str]) {
        let mut text = format!("log={}\n", self.log.display());
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        fs::write(self.paths.npx.parent().unwrap().join("npx.conf"), text).unwrap();
    }

    /// The stage HOME a run was given: one run directory under the stage root,
    /// never the root itself. A run that finished has already cleared it, so
    /// only the name is left to check.
    fn assert_stage_home(&self, home: &str) {
        let root = self.paths.stage.display().to_string();
        assert!(
            home.starts_with(&format!("{root}/run-")),
            "{home} is not a run directory under {root}"
        );
    }

    /// Run the real binary against this HOME, stopping at the failpoint `point`
    /// when one is given. A crash test needs a process that really exits, so it
    /// cannot run in-process.
    fn cli(&self, args: &[&str], point: Option<&str>) -> std::process::Output {
        self.cli_with_env(args, point, &[])
    }

    /// `cli` with extra variables in the binary's environment.
    fn cli_with_env(
        &self,
        args: &[&str],
        point: Option<&str>,
        extra: &[(String, String)],
    ) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_skillherd"));
        command
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.home())
            .env("SKILLHERD_NPX", &self.paths.npx)
            .env("SKILLHERD_PLATFORM", "mac");
        for (key, value) in extra {
            command.env(key, value);
        }
        if let Some(point) = point {
            command.env("SKILLHERD_RECONCILE_FAILPOINT", point);
        }
        command.output().unwrap()
    }

    /// What the stub saw of the harness-home overrides on its last run.
    fn npx_env(&self) -> Vec<String> {
        fs::read_to_string(self.log.parent().unwrap().join("npx.env"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// How many commits carry this subject prefix.
    fn commits_starting_with(&self, prefix: &str) -> usize {
        self.commits()
            .into_iter()
            .filter(|subject| subject.starts_with(prefix))
            .count()
    }

    /// The stub's log, one `(HOME, ARGV)` pair per run.
    fn npx_runs(&self) -> Vec<(String, String)> {
        match fs::read_to_string(&self.log) {
            Ok(text) => text
                .lines()
                .collect::<Vec<_>>()
                .chunks(2)
                .map(|pair| {
                    (
                        pair[0].trim_start_matches("HOME=").to_string(),
                        pair[1].trim_start_matches("ARGV=").to_string(),
                    )
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

fn opts(adopt: &[&str], keep: &[&str], dry_run: bool) -> ReconcileOpts {
    ReconcileOpts {
        dry_run,
        adopt: strings(adopt),
        keep: strings(keep),
        restore: Vec::new(),
        inbox: Vec::new(),
        fetched: Default::default(),
    }
}

fn restore(names: &[&str], dry_run: bool) -> ReconcileOpts {
    ReconcileOpts {
        restore: strings(names),
        ..opts(&[], &[], dry_run)
    }
}

#[test]
fn every_classification_lands_where_the_plan_says() {
    let env = Env::new();
    let bare = env.upstream("newcomer", "claude variant of newcomer");
    env.lock(&[("newcomer", &file_url(&bare))]);

    // bridge: the pool holds the npx bridge link into the shared dir.
    let bridge_target = env.inbox("bridge-one", "bridge body");
    std::os::unix::fs::symlink(&bridge_target, env.paths.library.join("bridge-one")).unwrap();
    // new: only the inbox has it.
    env.inbox("newcomer", "universal copy of newcomer");
    // already imported: identical bytes on both sides.
    env.entry("already", "already body");
    env.inbox("already", "already body");

    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(names(&report.imported), ["already", "newcomer"]);
    assert_eq!(report.refused.len(), 1, "{:?}", report.refused);
    assert_eq!(report.refused[0].name, "bridge-one");
    assert!(env.paths.library.join("bridge-one").is_symlink());
    assert!(env.inbox_root().join("bridge-one/SKILL.md").is_file());
    assert!(report.variants_pending.is_empty());

    for name in ["already", "newcomer"] {
        let entry = env.paths.library.join(name);
        assert!(entry.is_dir(), "{name} is a real directory");
        assert!(
            !fs::symlink_metadata(&entry)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            !env.inbox_root().join(name).exists(),
            "{name}'s inbox copy is gone"
        );
    }
    assert!(
        fs::read_to_string(env.paths.library.join("bridge-one/SKILL.md"))
            .unwrap()
            .contains("bridge body"),
        "the foreign library link and its target survive"
    );

    let record = env.record("newcomer");
    assert_eq!(record.imported_hash, env.entry_hash("newcomer"));
    assert_eq!(record.source_url, file_url(&bare));
    assert_eq!(record.variants.keys().collect::<Vec<_>>(), ["claude"]);
    assert!(!record.variants_pending);
    assert!(!record.revision.is_empty(), "the clone HEAD is recorded");
    assert!(record.imported_at.ends_with('Z'));
    assert!(
        fs::read_to_string(env.paths.library.join("newcomer/.variants/claude/SKILL.md"))
            .unwrap()
            .contains("claude variant of newcomer")
    );
    assert!(
        !env.paths.library.join("newcomer/.variants/agents").exists(),
        "the universal copy is not a variant"
    );
    assert_eq!(env.record("already").source_type, "local");

    let ignore = fs::read_to_string(env.paths.library.join(".gitignore")).unwrap();
    assert_eq!(
        ignore,
        "# bridge lines (never commit these\ngit)\nbridge-one\nbridge-two\n# a house note\n",
        "only the imported name's line goes"
    );

    let subjects = env.commits();
    assert_eq!(
        subjects.len(),
        3,
        "seed + two imports, no lock snapshot by default: {subjects:?}"
    );
    assert!(
        subjects
            .iter()
            .all(|subject| subject != "npx: lock snapshot")
    );
    assert!(subjects.contains(&format!(
        "npx: import newcomer ({})",
        reconcile::hash8(&record.imported_hash)
    )));

    // Idempotent: a second run has nothing to do.
    let again = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(again.imported.is_empty());
    assert!(again.plan.is_empty(), "{:?}", again.plan);
    assert_eq!(env.commits().len(), 3);
}

#[test]
fn an_upstream_move_replaces_the_entry_and_a_local_edit_is_refused() {
    let env = Env::new();
    env.entry("newer", "v1");
    env.inbox("newer", "v1");
    env.entry("edited", "v1");
    env.inbox("edited", "v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();

    // Upstream moves for both; one of the two pool copies was edited by hand.
    env.inbox("newer", "v2");
    piece(&env.paths.library, "edited", "house edit");
    env.inbox("edited", "v2");

    let mut current = opts(&[], &[], false);
    current.inbox = vec![env.inbox_root()];
    let report = reconcile::run(&env.paths, current).unwrap();
    assert_eq!(names(&report.imported), ["newer"]);
    assert_eq!(report.refused.len(), 1);
    assert_eq!(report.refused[0].name, "edited");
    assert_eq!(report.refused[0].code, "local_edit_refused");
    assert!(
        report.refused[0].message.contains("--adopt")
            && report.refused[0].message.contains("--keep"),
        "{}",
        report.refused[0].message
    );

    assert!(
        fs::read_to_string(env.paths.library.join("newer/SKILL.md"))
            .unwrap()
            .contains("v2"),
        "the newer arrival replaced the entry"
    );
    assert!(
        fs::read_to_string(env.paths.library.join("edited/SKILL.md"))
            .unwrap()
            .contains("house edit"),
        "the house edit survived"
    );
    assert!(
        env.inbox_root().join("edited").is_dir(),
        "the refused arrival is kept so the refusal can be resolved"
    );
    assert_eq!(
        env.record("edited").imported_hash,
        env.record("edited").imported_hash
    );

    // The command layer reports the refusal as invalid data, report included.
    let args = ReconcileArgs {
        discard: Vec::new(),
        adopt: Vec::new(),
        keep: Vec::new(),
        restore: Vec::new(),
    };
    let error = reconcile_cmd::run(&args, &env.paths, true).unwrap_err();
    assert_eq!(error.code(), "local_edit_refused");
    assert_eq!(error.exit_code(), 65);
    assert!(
        error.details()["refusals"][0]["name"] == "edited",
        "{}",
        error.details()
    );
    let fix = error.suggested_fix().unwrap_or("");
    assert!(fix.contains("--adopt") && fix.contains("edited"), "{fix}");
    assert!(!fix.contains("--restore"), "{fix}");
}

/// The command's aggregate error advises the remedy for what was refused:
/// a displaced copy gets `--restore`, an occupied backup path gets hand
/// cleanup, and neither is told to `--adopt` or `--keep`.
#[test]
fn the_aggregate_refusal_advises_the_remedy_for_each_kind() {
    let env = Env::new();
    env.inbox("moved", "v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    env.inbox("moved", "v2");
    displace(&env, "moved");
    env.inbox("blocked", "v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    env.inbox("blocked", "v2");
    let blocking = env.paths.library.join(".npx/backup/blocked");
    piece(blocking.parent().unwrap(), "blocked", "a foreign tree");

    let args = ReconcileArgs {
        discard: Vec::new(),
        adopt: Vec::new(),
        keep: Vec::new(),
        restore: Vec::new(),
    };
    let error = reconcile_cmd::run(&args, &env.paths, true).unwrap_err();
    assert_eq!(error.code(), "local_edit_refused");
    let fix = error.suggested_fix().unwrap_or("").to_string();
    assert!(fix.contains("--restore") && fix.contains("moved"), "{fix}");
    assert!(
        fix.contains("remove the backup by hand") && fix.contains("blocked"),
        "{fix}"
    );
    assert!(!fix.contains("--adopt") && !fix.contains("--keep"), "{fix}");
    let codes: Vec<String> = error.details()["refusals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|refusal| refusal["code"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(codes, ["stale_backup_refused", "displaced_refused"]);
}

#[test]
fn adopt_takes_upstream_and_keep_forgets_the_lock_entry() {
    let env = Env::new();
    let bare = env.upstream("adopted", "claude variant");
    env.lock(&[
        ("adopted", &file_url(&bare)),
        ("kept", "file:///nowhere.git"),
    ]);
    env.entry("adopted", "v1");
    env.inbox("adopted", "v1");
    env.entry("kept", "v1");
    env.inbox("kept", "v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();

    // Both pool copies are edited by hand; upstream moved for `adopted`.
    piece(&env.paths.library, "adopted", "house edit");
    env.inbox("adopted", "v2");
    piece(&env.paths.library, "kept", "house edit");
    // Modelled upstream moves: the pool copies are edited by hand and the
    // arrivals came back.
    env.inbox("adopted", "v2");
    env.inbox("kept", "v1");
    env.conf(&[]);

    let report = reconcile::run(&env.paths, opts(&["adopted"], &["kept"], false)).unwrap();
    assert_eq!(names(&report.imported), ["adopted"]);
    assert_eq!(report.kept, vec!["kept".to_string()]);
    assert!(report.refused.is_empty());
    assert!(
        fs::read_to_string(env.paths.library.join("adopted/SKILL.md"))
            .unwrap()
            .contains("v2"),
        "--adopt takes upstream"
    );
    assert!(
        fs::read_to_string(env.paths.library.join("kept/SKILL.md"))
            .unwrap()
            .contains("house edit"),
        "--keep leaves the house copy alone"
    );
    assert!(
        !env.record_path("kept").exists(),
        "a kept name stops being tracked upstream"
    );
    assert!(
        !env.inbox_root().join("kept").exists(),
        "the inbox copy is consumed either way"
    );
    let skills = env.live_lock()["skills"].clone();
    assert!(
        skills.get("kept").is_none(),
        "the staged npx remove made the lock forget it: {skills}"
    );
    let runs = env.npx_runs();
    assert_eq!(runs.len(), 1, "one staged run: {runs:?}");
    env.assert_stage_home(&runs[0].0);
    assert!(runs[0].1.contains("remove kept"), "{}", runs[0].1);

    // A kept copy is declared house, in the keep's own commit, so doctor knows
    // its provenance; an adopted one is tracked and not declared.
    let house = fs::read_to_string(env.paths.library.join(".globals/house")).unwrap();
    let declared = skillherd::provenance::parse_house(&house);
    assert!(declared.contains("kept"), "{house}");
    assert!(!declared.contains("adopted"), "{house}");
    assert!(house.contains("kept over upstream fixture"), "{house}");
    assert_eq!(
        env.git(&["log", "-1", "--format=%s", "--", ".globals/house"])
            .trim(),
        "npx: keep kept"
    );
    assert!(
        env.git(&["status", "--porcelain", "--", ".globals/house"])
            .is_empty(),
        "the house line is committed, not left dirty"
    );
}

#[test]
fn a_refused_update_names_a_retry_that_reaches_its_arrival() {
    let env = Env::new();
    let source = env.home().join("stub-source");
    piece(&source.join(".agents/skills"), "orphan", "upstream v1");
    let bare = env.upstream("orphan", "claude variant");
    env.conf(&[
        &format!("source_dir={}", source.display()),
        &format!("source_url={}", file_url(&bare)),
        "skill_path=.agents/skills/orphan/SKILL.md",
    ]);
    // npx tracks it, but the pool copy arrived before import records existed.
    env.lock(&[("orphan", &file_url(&bare))]);
    env.entry("orphan", "copied in by hand");
    env.git(&["add", "orphan"]);
    env.git(&["commit", "-q", "-m", "hand copy"]);

    let args = |adopt: &[&str]| UpdateArgs {
        names: strings(&["orphan"]),
        adopt: strings(adopt),
        keep: Vec::new(),
    };
    let error = update::run(&args(&[]), &env.paths, false).unwrap_err();
    assert_eq!(error.code(), "local_edit_refused");
    let fix = error.suggested_fix().unwrap_or("").to_string();
    assert!(
        fix.contains("`skillherd update n --adopt n`"),
        "the hint names the command that fetches the arrival again: {fix}"
    );
    assert!(!fix.contains("skillherd reconcile --adopt"), "{fix}");
    for refusal in error.details()["refusals"].as_array().unwrap() {
        let message = refusal["message"].as_str().unwrap();
        assert!(
            !message.contains("skill reconcile --adopt"),
            "no refusal names a retry that cannot reach the staged arrival: {message}"
        );
    }

    // The owned stage is now the only inbox. A plain reconcile can recover
    // its refused arrival, without fetching upstream again.
    let recovered = reconcile_cmd::run(
        &ReconcileArgs {
            discard: Vec::new(),
            adopt: strings(&["orphan"]),
            keep: Vec::new(),
            restore: Vec::new(),
        },
        &env.paths,
        false,
    )
    .unwrap();
    assert_eq!(recovered["imported"][0]["name"], "orphan");
    assert!(
        fs::read_to_string(env.paths.library.join("orphan/SKILL.md"))
            .unwrap()
            .contains("upstream v1")
    );

    // The hinted retry also succeeds and keeps tracking upstream.
    update::run(&args(&["orphan"]), &env.paths, false).unwrap();
    assert!(
        fs::read_to_string(env.paths.library.join("orphan/SKILL.md"))
            .unwrap()
            .contains("upstream v1")
    );
    assert_eq!(env.record("orphan").imported_hash, env.entry_hash("orphan"));
    assert!(!env.paths.stage.exists(), "the resolved stage is cleared");
}

#[test]
fn keep_refuses_a_name_that_is_not_a_conflict() {
    let env = Env::new();
    env.entry("plain", "same");
    env.inbox("plain", "same");
    let error = reconcile::run(&env.paths, opts(&[], &["plain"], false)).unwrap_err();
    assert_eq!(error.code(), "usage");
    assert_eq!(error.exit_code(), 2);

    let error = reconcile::run(&env.paths, opts(&[], &["missing"], false)).unwrap_err();
    assert_eq!(error.code(), "usage");
}

#[test]
fn an_identical_variant_transition_removes_the_stored_copy() {
    let env = Env::new();
    let bare = env.upstream("shifty", "claude variant");
    env.lock(&[("shifty", &file_url(&bare))]);
    env.inbox("shifty", "universal copy");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(env.record("shifty").variants.contains_key("claude"));

    // Upstream now ships the same bytes for Claude: no variant any more.
    let variant = env.upstream_work().join(".claude/skills/shifty/SKILL.md");
    fs::write(
        &variant,
        fs::read_to_string(env.upstream_work().join(".agents/skills/shifty/SKILL.md")).unwrap(),
    )
    .unwrap();
    env.publish_work(&bare);

    env.inbox("shifty", "universal copy");
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(report.refused.is_empty());
    assert!(
        !env.paths.library.join("shifty/.variants/claude").exists(),
        "the stored variant is gone"
    );
    let record = env.record("shifty");
    assert!(record.variants.is_empty(), "{:?}", record.variants);
    assert!(!record.revision.is_empty());
}

#[test]
fn a_locally_edited_variant_is_refused() {
    let env = Env::new();
    let bare = env.upstream("protected", "variant v1");
    env.lock(&[("protected", &file_url(&bare))]);
    env.inbox("protected", "universal");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    let stored = env.paths.library.join("protected/.variants/claude");
    fs::write(
        stored.join("SKILL.md"),
        piece_body("protected", "hand edit"),
    )
    .unwrap();
    let recorded = env.record("protected").variants["claude"].clone();

    // Upstream moves, so the pass would want to replace the variant.
    env.publish_at("protected", "variant v2", &bare);
    env.inbox("protected", "universal");
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(report.refused.len(), 1, "{:?}", report.refused);
    assert!(report.refused[0].message.contains(".variants/claude"));
    assert!(
        fs::read_to_string(stored.join("SKILL.md"))
            .unwrap()
            .contains("hand edit"),
        "the edited variant survived"
    );
    assert_eq!(env.record("protected").variants["claude"], recorded);
}

#[test]
fn pending_variants_complete_on_a_later_run() {
    let env = Env::new();
    let bare = env.home().join("later.git");
    env.lock(&[("late", &file_url(&bare))]);
    env.inbox("late", "universal");

    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(names(&report.imported), ["late"]);
    assert_eq!(report.variants_pending, vec!["late".to_string()]);
    let record = env.record("late");
    assert!(record.variants_pending);
    assert!(!env.paths.library.join("late/.variants").exists());
    assert!(report.plan.iter().any(|op| op.op == "git"));

    // Upstream appears at the URL the record holds; the next reconcile retries
    // every pending record even though the inbox is empty.
    env.publish_at("late", "claude variant", &bare);
    let commits = env.commits().len();
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(report.variants_pending.is_empty(), "{:?}", report);
    let record = env.record("late");
    assert!(!record.variants_pending);
    assert!(record.variants.contains_key("claude"));
    assert!(!record.revision.is_empty());
    assert!(
        env.paths
            .library
            .join("late/.variants/claude/SKILL.md")
            .is_file(),
        "the retry stored the variant"
    );
    assert_eq!(env.commits().len(), commits + 1, "one commit for the retry");

    // Converged: a third run changes nothing at all.
    let after = env.commits().len();
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(report.plan.is_empty(), "{:?}", report.plan);
    assert_eq!(env.commits().len(), after);
}

/// The state a crash-interruption test starts from: an imported entry whose
/// Claude variant was edited by hand, upstream moved on, and the newer arrival
/// is back in the inbox. Returns the variant directory.
fn interrupted_replacement(env: &Env) -> PathBuf {
    let bare = env.upstream("crash", "claude variant v1");
    env.lock(&[("crash", &file_url(&bare))]);
    env.inbox("crash", "universal v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();

    let variant = env.paths.library.join("crash/.variants/claude");
    assert!(
        variant.join("SKILL.md").is_file(),
        "the baseline import carries the upstream variant"
    );
    fs::write(variant.join("SKILL.md"), piece_body("crash", "hand edit")).unwrap();
    env.publish_at("crash", "claude variant v2", &bare);
    env.inbox("crash", "universal v2");
    variant
}

/// The complete copy of one entry: the live tree, or the backup a crash
/// between the displace and the publish left for the next run to restore.
fn complete_copy(env: &Env, name: &str) -> PathBuf {
    let live = env.paths.library.join(name);
    if live.join("SKILL.md").is_file() {
        return live;
    }
    let backup = env.paths.library.join(".npx/backup").join(name);
    assert!(
        backup.join("SKILL.md").is_file(),
        "neither {} nor {} holds a complete copy",
        live.display(),
        backup.display()
    );
    backup
}

/// Every step of a replacement, in the order a run reaches them.
const IMPORT_FAILPOINTS: [&str; 8] = [
    reconcile::failpoint::AFTER_COPY_TO_TMP,
    reconcile::failpoint::AFTER_VARIANT_TRANSFER,
    reconcile::failpoint::AFTER_DISPLACE,
    reconcile::failpoint::AFTER_PUBLISH,
    reconcile::failpoint::AFTER_RECORD_WRITE,
    reconcile::failpoint::AFTER_GIT_ADD,
    reconcile::failpoint::AFTER_COMMIT,
    reconcile::failpoint::BEFORE_INBOX_DELETE,
];

#[test]
fn every_import_interruption_point_keeps_a_complete_copy_and_converges() {
    for point in IMPORT_FAILPOINTS {
        let env = Env::new();
        let variant = interrupted_replacement(&env);
        let hand = tree_hash(&variant).unwrap();
        let entry = env.paths.library.join("crash");
        let inbox = env.inbox_root().join("crash");
        let baseline = env.commits_starting_with("npx: import crash");

        let crashed = env.cli(&["reconcile"], Some(point));
        assert!(
            !crashed.status.success(),
            "{point}: the run was not stopped ({})",
            String::from_utf8_lossy(&crashed.stdout)
        );
        assert_eq!(
            crashed.status.code(),
            Some(70),
            "{point}: {}",
            String::from_utf8_lossy(&crashed.stderr)
        );

        // The pool still holds a complete copy of the entry — the old tree or
        // the new one, never neither — and never without the hand-edited
        // variant, which the hash guard protects. Between the displace and the
        // publish that copy is the backup, which the next run restores.
        let copy = complete_copy(&env, "crash");
        let kept_variant = copy.join(".variants/claude");
        assert!(
            kept_variant.join("SKILL.md").is_file(),
            "{point}: the variant is gone from {}",
            copy.display()
        );
        assert_eq!(
            tree_hash(&kept_variant).unwrap(),
            hand,
            "{point}: the hand edit was lost"
        );
        assert!(
            inbox.is_dir(),
            "{point}: the inbox copy went before the commit landed"
        );

        // The next run converges: one record, and exactly one commit for the
        // replacement (the baseline import is the other one). Between the
        // displace and the publish the backup is the only copy, and no run
        // moves it unasked: that point refuses until `--restore crash`.
        let next = if point == reconcile::failpoint::AFTER_DISPLACE {
            let refused = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
            assert_eq!(refused.refused.len(), 1, "{point}: {:?}", refused.refused);
            assert!(
                refused.refused[0].message.contains("--restore crash"),
                "{point}: {}",
                refused.refused[0].message
            );
            assert!(!entry.exists(), "{point}: something restored unasked");
            restore(&["crash"], false)
        } else {
            opts(&[], &[], false)
        };
        let report = reconcile::run(&env.paths, next).unwrap();
        assert_eq!(names(&report.imported), ["crash"], "{point}");
        assert_eq!(
            env.commits_starting_with("npx: import crash"),
            baseline + 1,
            "{point}: the replacement did not land in exactly one commit: {:?}",
            env.commits()
        );
        assert_eq!(
            env.record("crash").imported_hash,
            env.entry_hash("crash"),
            "{point}: the record does not describe the entry"
        );
        assert!(entry.join("SKILL.md").is_file(), "{point}");
        assert_eq!(
            tree_hash(&variant).unwrap(),
            hand,
            "{point}: the hand edit was lost on the retry"
        );
        assert!(!inbox.exists(), "{point}: the inbox copy survives");
        assert!(
            !env.paths.library.join(".npx/tmp/crash").exists(),
            "{point}: scratch is left behind"
        );
        assert!(
            !env.paths.stage.exists(),
            "{point}: the stage root is left behind"
        );

        // The backup: a run deletes only the copy it displaced itself. Up to
        // the displace, and after `--restore`, the converging run displaced
        // and so cleaned up. From the publish on, the crashed run's backup
        // sits beside the finished entry; the converging run reports it and
        // leaves it for a human.
        let backup = env.paths.library.join(".npx/backup/crash");
        // (BEFORE_INBOX_DELETE comes after that run's own cleanup, so its
        // backup is already gone.)
        let crashed_after_publish = [
            reconcile::failpoint::AFTER_PUBLISH,
            reconcile::failpoint::AFTER_RECORD_WRITE,
            reconcile::failpoint::AFTER_GIT_ADD,
            reconcile::failpoint::AFTER_COMMIT,
        ]
        .contains(&point);
        if crashed_after_publish {
            assert!(
                backup.join("SKILL.md").is_file(),
                "{point}: an earlier run's backup was deleted"
            );
            assert!(
                !report
                    .plan
                    .iter()
                    .any(|op| op.op == "delete" && op.path == backup),
                "{point}: {:?}",
                report.plan
            );
            let again = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
            assert!(again.plan.is_empty(), "{point}: {:?}", again.plan);
            assert!(
                backup.join("SKILL.md").is_file(),
                "{point}: a later run deleted the stale backup"
            );
            // The human compares and removes it.
            fs::remove_dir_all(&backup).unwrap();
        } else {
            assert!(
                !backup.exists(),
                "{point}: this run's own backup is left behind"
            );
        }

        // Settled: a further run has nothing left to do.
        let settled = env.commits().len();
        skillherd::output::clear_warnings();
        let again = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
        assert!(again.plan.is_empty(), "{point}: {:?}", again.plan);
        assert!(again.refused.is_empty(), "{point}: {:?}", again.refused);
        assert!(
            !warning_codes().contains(&"stale_backup".to_string()),
            "{point}: a settled run still reports a backup"
        );
        assert_eq!(
            env.commits().len(),
            settled,
            "{point}: a settled run commits"
        );
    }
}

#[test]
fn a_dirty_unrelated_staged_path_is_refused_before_any_write() {
    let env = Env::new();
    env.inbox("arrival", "v1");
    piece(&env.paths.library, "staged-house", "house copy");
    env.git(&["add", "--", "staged-house"]);

    let error = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap_err();
    assert_eq!(error.code(), "library_dirty_unrelated");
    assert_eq!(error.exit_code(), 65);
    assert!(
        !error.details()["paths"].as_array().unwrap().is_empty(),
        "{}",
        error.details()
    );
    assert!(
        !env.paths.library.join("arrival").exists(),
        "nothing was written"
    );
    assert!(!env.paths.library.join(".npx").exists());
    assert!(env.inbox_root().join("arrival").is_dir());
    assert_eq!(env.commits().len(), 1, "the seed commit only");
}

#[test]
fn a_dry_run_plans_everything_and_writes_nothing() {
    let env = Env::new();
    let bare = env.upstream("planned", "claude variant");
    env.lock(&[("planned", &file_url(&bare))]);
    env.inbox("planned", "universal");
    let before = tree_hash(env.home()).unwrap();

    let report = reconcile::run(&env.paths, opts(&[], &[], true)).unwrap();
    assert_eq!(names(&report.imported), ["planned"]);
    assert!(
        report.plan.iter().any(|op| op.op == "git"),
        "{:?}",
        report.plan
    );
    assert!(
        report.plan.iter().any(|op| op.op == "rename"),
        "{:?}",
        report.plan
    );
    assert!(
        report.render.is_some(),
        "a dry run still plans the render, without writing"
    );
    assert_eq!(
        tree_hash(env.home()).unwrap(),
        before,
        "a dry run is a read"
    );
    assert!(env.inbox_root().join("planned").is_dir());
    assert!(!env.paths.library.join(".npx").exists());
}

#[test]
fn the_staged_child_never_sees_a_harness_home_override() {
    // `skills update` re-installs to whatever agent homes it can find. With
    // CLAUDE_CONFIG_DIR inherited it found the live Claude surface through
    // the config dir's `skills` link and wrote a dangling run-dir link there.
    let env = Env::new();
    let source = env.home().join("stub-source");
    piece(&source.join(".agents/skills"), "staged", "universal");
    let bare = env.upstream("staged", "claude variant");
    env.conf(&[
        &format!("source_dir={}", source.display()),
        &format!("source_url={}", file_url(&bare)),
        "skill_path=.agents/skills/staged/SKILL.md",
    ]);
    // Distinct, creatable paths: the binary itself resolves CODEX_HOME and
    // GROK_HOME to surfaces and refuses two harnesses on one path.
    let scrubbed = [
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "GROK_HOME",
        "CURSOR_CONFIG_DIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
    ];
    let overrides: Vec<(String, String)> = scrubbed
        .iter()
        .map(|key| {
            (
                key.to_string(),
                env.home().join("override").join(key).display().to_string(),
            )
        })
        .collect();

    let output = env.cli_with_env(&["install", "owner/repo"], None, &overrides);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let seen = env.npx_env();
    assert_eq!(seen.len(), scrubbed.len(), "{seen:?}");
    for line in &seen {
        assert!(line.ends_with("=unset"), "the child inherited {line}");
    }
    // npx is `#!/usr/bin/env node`: its own directory leads the child's PATH so
    // the node beside it is found, not a version manager's shim that cannot
    // resolve a version once HOME is the stage; resolve the real executable first.
    let path_head = fs::read_to_string(env.log.parent().unwrap().join("npx.path")).unwrap();
    assert_eq!(
        Path::new(path_head.trim()),
        env.paths.npx.parent().unwrap(),
        "the npx directory leads the staged child's PATH"
    );
}

#[test]
fn install_runs_npx_in_the_stage_and_imports_what_it_left() {
    let env = Env::new();
    // A same-name foreign skill on two surfaces the renderer manages but must
    // never touch a foreign entry of: the foreign trees stay byte-identical.
    let foreign_pi = piece(&env.paths.pi_skills, "staged", "foreign pi copy");
    let foreign_cursor = piece(&env.paths.cursor_skills, "staged", "foreign cursor copy");
    let pi_hash = tree_hash(&foreign_pi).unwrap();
    let cursor_hash = tree_hash(&foreign_cursor).unwrap();

    let source = env.home().join("stub-source");
    piece(&source.join(".agents/skills"), "staged", "universal");
    let bare = env.upstream("staged", "claude variant");
    env.conf(&[
        &format!("source_dir={}", source.display()),
        &format!("source_url={}", file_url(&bare)),
        "skill_path=.agents/skills/staged/SKILL.md",
    ]);

    let data = install::run(
        &InstallArgs {
            source: "owner/repo".to_string(),
            skill: Vec::new(),
            release_asset: None,
            tag_prefix: None,
            release_dir: None,
            adopt: Vec::new(),
            keep: Vec::new(),
        },
        &env.paths,
        false,
    )
    .unwrap();

    let runs = env.npx_runs();
    assert_eq!(runs.len(), 1, "{runs:?}");
    env.assert_stage_home(&runs[0].0);
    assert!(runs[0].1.starts_with("-y skills@1.7.0 add owner/repo"));
    assert!(
        runs[0].1.contains("-a universal"),
        "the add targets the canonical dir only: {}",
        runs[0].1
    );
    assert!(data["plan"].is_array());
    assert_eq!(data["npx"]["exit"], 0);
    env.assert_stage_home(data["npx"]["home"].as_str().unwrap());

    // The pool took the arrival, with its variants, and the stage is gone.
    assert!(env.paths.library.join("staged/SKILL.md").is_file());
    assert!(
        env.paths
            .library
            .join("staged/.variants/claude/SKILL.md")
            .is_file()
    );
    let record = env.record("staged");
    assert_eq!(record.source_url, file_url(&bare));
    assert_eq!(record.variants.keys().collect::<Vec<_>>(), ["claude"]);
    assert!(!env.paths.stage.exists());
    assert!(env.live_lock()["skills"].get("staged").is_some());

    // Nothing outside the stage was touched.
    assert_eq!(tree_hash(&foreign_pi).unwrap(), pi_hash);
    assert_eq!(tree_hash(&foreign_cursor).unwrap(), cursor_hash);
    assert!(
        fs::read_to_string(foreign_pi.join("SKILL.md"))
            .unwrap()
            .contains("foreign pi")
    );
    assert!(
        fs::read_to_string(foreign_cursor.join("SKILL.md"))
            .unwrap()
            .contains("foreign cursor")
    );
    assert!(
        !env.inbox_root().join("staged").exists(),
        "the shared dir was not used as the install target"
    );

    // update: same stage, no agent flag, and the pool follows upstream.
    fs::write(
        source.join(".agents/skills/staged/SKILL.md"),
        piece_body("staged", "universal v2"),
    )
    .unwrap();
    update::run(
        &UpdateArgs {
            names: vec!["staged".to_string()],
            adopt: Vec::new(),
            keep: Vec::new(),
        },
        &env.paths,
        false,
    )
    .unwrap();
    let runs = env.npx_runs();
    assert_eq!(runs.len(), 2);
    assert!(runs[1].1.starts_with("-y skills@1.7.0 update staged"));
    assert!(
        !runs[1].1.contains("-a universal"),
        "update fans out through npx itself: {}",
        runs[1].1
    );
    assert!(
        fs::read_to_string(env.paths.library.join("staged/SKILL.md"))
            .unwrap()
            .contains("universal v2")
    );
    assert_eq!(env.record("staged").imported_hash, env.entry_hash("staged"));
    assert_eq!(tree_hash(&foreign_pi).unwrap(), pi_hash);
    assert_eq!(tree_hash(&foreign_cursor).unwrap(), cursor_hash);

    // remove: the entry, the record and the policy row go; nothing else does.
    assert!(
        fs::read_to_string(env.paths.library.join(".globals/sets"))
            .unwrap()
            .contains("post  main lane")
    );
    let data = remove::run(
        &RemoveArgs {
            names: vec!["staged".to_string()],
            force: false,
        },
        &env.paths,
        false,
    )
    .unwrap();
    assert_eq!(data["removed"][0], "staged");
    assert!(data["plan"].is_array());
    assert!(!env.paths.library.join("staged").exists());
    assert!(!env.record_path("staged").exists());
    assert!(env.live_lock()["skills"].get("staged").is_none());
    assert!(
        !env.inbox_root().join("staged").exists(),
        "a removal never reaches the shared dir"
    );
    assert_eq!(tree_hash(&foreign_pi).unwrap(), pi_hash);
    assert_eq!(tree_hash(&foreign_cursor).unwrap(), cursor_hash);
    assert!(
        env.commits()
            .iter()
            .any(|subject| subject == "npx: remove staged")
    );
    // The removal commit carries the tree deletion, not just the record: a
    // deleted directory is known to the index only through its files.
    assert_eq!(
        env.git(&["status", "--porcelain", "--", "staged"]).trim(),
        "",
        "the tree deletion was left unstaged"
    );
    assert_eq!(
        env.git(&["ls-tree", "--name-only", "HEAD", "--", "staged"])
            .trim(),
        "",
        "HEAD still carries the removed tree"
    );
}

#[test]
fn remove_drops_the_policy_row_and_refuses_local_edits() {
    let env = Env::new();
    let bare = env.upstream("rowed", "claude variant");
    env.lock(&[("rowed", &file_url(&bare))]);
    env.inbox("rowed", "universal");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    let sets = env.paths.library.join(".globals/sets");
    let mut text = fs::read_to_string(&sets).unwrap();
    text.push_str("rowed  main lane\n");
    fs::write(&sets, &text).unwrap();

    // A hand-edited variant blocks the removal until --force.
    let variant = env.paths.library.join("rowed/.variants/claude/SKILL.md");
    fs::write(&variant, piece_body("rowed", "hand edit")).unwrap();
    let error = remove::run(
        &RemoveArgs {
            names: vec!["rowed".to_string()],
            force: false,
        },
        &env.paths,
        false,
    )
    .unwrap_err();
    assert_eq!(error.code(), "local_edit_refused");
    assert!(error.suggested_fix().unwrap().contains("--force"));
    assert!(env.paths.library.join("rowed").is_dir());

    remove::run(
        &RemoveArgs {
            names: vec!["rowed".to_string()],
            force: true,
        },
        &env.paths,
        false,
    )
    .unwrap();
    assert!(!env.paths.library.join("rowed").exists());
    assert_eq!(
        fs::read_to_string(&sets).unwrap(),
        "# curated 2026-09-21\nhouse  main\npost  main lane\n",
        "only the removed row goes"
    );
    assert!(
        !variant.exists(),
        "the entry and its variants went together"
    );
}

#[test]
fn remove_tolerates_an_entry_the_library_never_committed() {
    let env = Env::new();
    // An entry and a record that were never committed: git has nothing to do,
    // and naming them must not fail the removal with a bad pathspec.
    piece(&env.paths.library, "raw", "body");
    fs::create_dir_all(env.paths.library.join(".npx")).unwrap();
    fs::write(
        env.record_path("raw"),
        serde_json::to_string_pretty(&NpxMeta {
            source: String::new(),
            source_type: "local".to_string(),
            source_url: String::new(),
            r#ref: None,
            skill_path: String::new(),
            skill_folder_hash: String::new(),
            imported_hash: env.entry_hash("raw"),
            imported_at: "2026-09-21T00:00:00Z".to_string(),
            revision: String::new(),
            variants: BTreeMap::new(),
            variants_pending: false,
            harness_agents: BTreeMap::new(),
            release: None,
        })
        .unwrap(),
    )
    .unwrap();

    let data = remove::run(
        &RemoveArgs {
            names: vec!["raw".to_string()],
            force: false,
        },
        &env.paths,
        false,
    )
    .unwrap();
    assert_eq!(data["removed"][0], "raw");
    assert!(!env.paths.library.join("raw").exists());
    assert!(!env.record_path("raw").exists());
    assert_eq!(
        env.commits().len(),
        1,
        "nothing was committed: {}",
        env.commits().len()
    );
}

#[test]
fn remove_refuses_a_name_that_npx_does_not_track() {
    let env = Env::new();
    let error = remove::run(
        &RemoveArgs {
            names: vec!["house".to_string()],
            force: false,
        },
        &env.paths,
        false,
    )
    .unwrap_err();
    assert_eq!(error.code(), "input_not_found");
    assert_eq!(error.exit_code(), 66);
    assert!(
        error
            .suggested_fix()
            .unwrap()
            .contains("skillherd off house")
    );
    assert!(env.paths.library.join("house").is_dir());
}

#[test]
fn a_dry_run_install_plans_the_npx_run_without_making_it() {
    let env = Env::new();
    let before = tree_hash(env.home()).unwrap();
    let data = install::run(
        &InstallArgs {
            source: "owner/repo".to_string(),
            skill: vec!["post".to_string()],
            release_asset: None,
            tag_prefix: None,
            release_dir: None,
            adopt: Vec::new(),
            keep: Vec::new(),
        },
        &env.paths,
        true,
    )
    .unwrap();
    assert!(env.npx_runs().is_empty(), "npx is not invoked");
    assert!(data.get("npx").is_none());
    let plan = data["plan"].as_array().unwrap();
    let npx_op = plan
        .iter()
        .find(|op| op["op"] == "npx")
        .expect("the plan names the npx run");
    assert_eq!(npx_op["path"], env.paths.npx.display().to_string());
    assert!(
        npx_op["target"]
            .as_str()
            .unwrap()
            .contains("add owner/repo --skill post -g -a universal -y"),
        "{npx_op}"
    );
    assert_eq!(tree_hash(env.home()).unwrap(), before);
    assert!(!env.paths.stage.exists());
}

#[test]
fn a_dry_run_remove_plans_the_removal_and_writes_nothing() {
    let env = Env::new();
    let bare = env.upstream("doomed", "claude variant");
    env.lock(&[("doomed", &file_url(&bare))]);
    env.inbox("doomed", "universal copy");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    let sets = env.paths.library.join(".globals/sets");
    let text = fs::read_to_string(&sets).unwrap();
    fs::write(&sets, format!("{text}doomed  main lane\n")).unwrap();
    let before = tree_hash(env.home()).unwrap();
    let commits = env.commits().len();
    let runs = env.npx_runs().len();

    let data = remove::run(
        &RemoveArgs {
            names: vec!["doomed".to_string()],
            force: false,
        },
        &env.paths,
        true,
    )
    .unwrap();
    assert_eq!(data["removed"][0], "doomed");
    assert!(data.get("npx").is_none(), "npx is not invoked: {data}");
    let ops: Vec<&str> = data["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op| op["op"].as_str().unwrap())
        .collect();
    assert!(ops.contains(&"npx"), "{ops:?}");
    assert!(ops.contains(&"delete"), "{ops:?}");
    assert!(
        data["plan"].as_array().unwrap().iter().any(
            |op| op["op"] == "write" && op["path"].as_str().unwrap().ends_with(".globals/sets")
        ),
        "the policy row drop is planned: {ops:?}"
    );
    assert!(
        fs::read_to_string(&sets)
            .unwrap()
            .contains("doomed  main lane")
    );
    assert_eq!(
        tree_hash(env.home()).unwrap(),
        before,
        "a dry run is a read"
    );
    assert_eq!(env.commits().len(), commits);
    assert_eq!(env.npx_runs().len(), runs);
    assert!(env.paths.library.join("doomed").is_dir());
    assert!(env.record_path("doomed").is_file());
}

#[test]
fn the_wrapper_refuses_the_local_bin_shim() {
    let env = Env::new();
    let shim_dir = env.home().join(".local/bin");
    fs::create_dir_all(&shim_dir).unwrap();
    fs::write(shim_dir.join("npx"), "#!/bin/sh\n").unwrap();
    let mut paths = env.paths.clone();
    paths.npx = shim_dir.join("npx");
    let error = install::run(
        &InstallArgs {
            source: "owner/repo".to_string(),
            skill: Vec::new(),
            release_asset: None,
            tag_prefix: None,
            release_dir: None,
            adopt: Vec::new(),
            keep: Vec::new(),
        },
        &paths,
        false,
    )
    .unwrap_err();
    assert_eq!(error.code(), "config_error");
    assert_eq!(error.exit_code(), 78);
    assert!(error.to_string().contains(".local/bin"), "{error}");
    assert!(!paths.stage.exists(), "nothing ran");
}

#[test]
fn a_stage_this_tool_did_not_create_is_refused_untouched() {
    let env = Env::new();
    // SKILLHERD_STAGE points at a directory full of operator data.
    fs::create_dir_all(&env.paths.stage).unwrap();
    let notes = env.paths.stage.join("notes.md");
    fs::write(&notes, b"operator data\n").unwrap();

    let error = install::run(
        &InstallArgs {
            source: "owner/repo".to_string(),
            skill: Vec::new(),
            release_asset: None,
            tag_prefix: None,
            release_dir: None,
            adopt: Vec::new(),
            keep: Vec::new(),
        },
        &env.paths,
        false,
    )
    .unwrap_err();
    assert_eq!(error.code(), "config_error");
    assert_eq!(error.exit_code(), 78);
    assert!(
        error.to_string().contains(".skill-stage-owner"),
        "the fix names the marker: {error}"
    );
    assert_eq!(
        fs::read(&notes).unwrap(),
        b"operator data\n",
        "the unowned tree is byte-identical"
    );
    assert!(env.npx_runs().is_empty(), "npx never ran");
}

#[test]
fn a_stage_with_unconsumed_data_survives_and_the_next_run_proceeds() {
    let env = Env::new();
    // A run that built a stage and died before clearing it: the thread-local
    // handle is simply dropped, as a crash drops it.
    let crashed = skillherd::npx::prepare_stage(&env.paths).unwrap();
    let half = crashed.inbox().join("half-written");
    fs::write(&half, b"x").unwrap();

    let source = env.home().join("stub-source");
    piece(&source.join(".agents/skills"), "staged", "universal");
    env.conf(&[&format!("source_dir={}", source.display())]);
    install::run(
        &InstallArgs {
            source: "owner/repo".to_string(),
            skill: Vec::new(),
            release_asset: None,
            tag_prefix: None,
            release_dir: None,
            adopt: Vec::new(),
            keep: Vec::new(),
        },
        &env.paths,
        false,
    )
    .unwrap();

    assert_eq!(fs::read(&half).unwrap(), b"x", "unconsumed data survives");
    assert!(crashed.home().is_dir(), "the interrupted stage survives");
    assert!(env.paths.library.join("staged/SKILL.md").is_file());
    let runs = env.npx_runs();
    assert_eq!(runs.len(), 1);
    env.assert_stage_home(&runs[0].0);
}

#[test]
fn a_displaced_entry_with_no_live_copy_is_refused_until_restored_then_re_imported() {
    let env = Env::new();
    let bare = env.upstream("moved", "claude variant");
    env.lock(&[("moved", &file_url(&bare))]);
    env.inbox("moved", "universal v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();

    // The state a crash between the displace and the publish leaves: the live
    // entry is in the backup, so the backup is the only complete copy.
    env.inbox("moved", "universal v2");
    let backup = displace(&env, "moved");
    assert!(backup.join(".variants/claude/SKILL.md").is_file());

    // Unasked, the run refuses the name and moves nothing.
    let refused = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(refused.refused.len(), 1, "{:?}", refused.refused);
    assert_eq!(refused.refused[0].name, "moved");
    assert!(refused.refused[0].message.contains("--restore moved"));
    assert!(!env.paths.library.join("moved").exists());
    assert!(backup.join("SKILL.md").is_file());

    let report = reconcile::run(&env.paths, restore(&["moved"], false)).unwrap();
    assert_eq!(names(&report.imported), ["moved"]);
    let entry = env.paths.library.join("moved");
    assert!(entry.join("SKILL.md").is_file(), "the entry is back");
    assert!(
        entry.join(".variants/claude/SKILL.md").is_file(),
        "the variants came with it"
    );
    assert!(
        fs::read_to_string(entry.join("SKILL.md"))
            .unwrap()
            .contains("universal v2"),
        "and the arrival was imported over it"
    );
    assert!(!backup.exists(), "the backup goes once the commit lands");
    assert_eq!(env.record("moved").imported_hash, env.entry_hash("moved"));
    assert_eq!(env.commits_starting_with("npx: import moved"), 2);
    assert!(!env.inbox_root().join("moved").exists());
}

#[test]
fn a_published_replacement_finishes_its_record_and_its_commit() {
    let env = Env::new();
    let bare = env.upstream("landed", "claude variant");
    env.lock(&[("landed", &file_url(&bare))]);
    env.inbox("landed", "universal v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();

    // A crash after the publish rename: the live entry is the new tree (with
    // the variants carried into it), the displaced copy waits in the backup,
    // and the record still describes the old tree — so without the backup this
    // would read as a local edit.
    env.inbox("landed", "universal v2");
    let entry = env.paths.library.join("landed");
    let backup = env.paths.library.join(".npx/backup/landed");
    fs::create_dir_all(backup.parent().unwrap()).unwrap();
    fs::rename(&entry, &backup).unwrap();
    skillherd::variants::copy_tree(&backup, &entry, &[]).unwrap();
    fs::write(entry.join("SKILL.md"), piece_body("landed", "universal v2")).unwrap();
    assert_ne!(env.record("landed").imported_hash, env.entry_hash("landed"));

    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(names(&report.imported), ["landed"]);
    assert!(report.refused.is_empty(), "{:?}", report.refused);
    assert_eq!(env.record("landed").imported_hash, env.entry_hash("landed"));
    assert_eq!(
        env.commits_starting_with("npx: import landed"),
        2,
        "one commit for the replacement: {:?}",
        env.commits()
    );
    // The crashed run's backup is not this run's to delete: reported, kept.
    assert!(
        backup.join("SKILL.md").is_file(),
        "an earlier run's backup was deleted"
    );
    assert!(
        !report
            .plan
            .iter()
            .any(|op| op.op == "delete" && op.path == backup),
        "{:?}",
        report.plan
    );
    assert!(
        entry.join(".variants/claude/SKILL.md").is_file(),
        "the variant survives the finish"
    );
    assert!(!env.inbox_root().join("landed").exists());
}

#[test]
fn an_incomplete_staged_copy_is_rebuilt_from_the_inbox() {
    let env = Env::new();
    let bare = env.upstream("half", "claude variant");
    env.lock(&[("half", &file_url(&bare))]);
    env.inbox("half", "universal v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    let old = env.entry_hash("half");

    // A crash after the copy: the scratch holds half the arrival, the live
    // entry is still the old tree, and the inbox copy is the retry input.
    env.inbox("half", "universal v2");
    let tmp = env.paths.library.join(".npx/tmp/half");
    fs::create_dir_all(&tmp).unwrap();
    fs::write(tmp.join("SKILL.md"), piece_body("half", "half a copy")).unwrap();

    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    let entry = env.paths.library.join("half");
    assert!(
        fs::read_to_string(entry.join("SKILL.md"))
            .unwrap()
            .contains("universal v2"),
        "the arrival replaced the old tree"
    );
    assert!(entry.join(".variants/claude/SKILL.md").is_file());
    assert!(!tmp.exists(), "the incomplete scratch copy is gone");
    assert_eq!(env.record("half").imported_hash, env.entry_hash("half"));
    assert_ne!(old, env.entry_hash("half"));
    assert_eq!(env.commits_starting_with("npx: import half"), 2);
}

/// Every step of a keep, in the order a run reaches them.
const KEEP_FAILPOINTS: [&str; 3] = [
    reconcile::failpoint::AFTER_KEEP_NPX,
    reconcile::failpoint::AFTER_GIT_ADD,
    reconcile::failpoint::AFTER_COMMIT,
];

#[test]
fn an_interrupted_keep_resumes_and_never_loses_the_house_copy() {
    for point in KEEP_FAILPOINTS {
        let env = Env::new();
        env.conf(&[]);
        let bare = env.upstream("kept", "claude variant");
        env.lock(&[("kept", &file_url(&bare))]);
        env.inbox("kept", "universal v1");
        reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();

        // Upstream moved on and the house copy was edited by hand: a conflict,
        // which `--keep` resolves by keeping the house copy and forgetting the
        // upstream record.
        piece(&env.paths.library, "kept", "house edit");
        env.inbox("kept", "universal v2");

        let crashed = env.cli(&["reconcile", "--keep", "kept"], Some(point));
        assert!(
            !crashed.status.success(),
            "{point}: the run was not stopped"
        );
        assert_eq!(
            crashed.status.code(),
            Some(70),
            "{point}: {}",
            String::from_utf8_lossy(&crashed.stderr)
        );

        // The house copy is intact, and the inbox copy is still the retry input
        // because the metadata commit had not landed.
        let house = || fs::read_to_string(env.paths.library.join("kept/SKILL.md")).unwrap();
        assert!(
            house().contains("house edit"),
            "{point}: the house copy went"
        );
        assert!(
            env.inbox_root().join("kept").is_dir(),
            "{point}: the inbox copy was consumed before the commit landed"
        );

        reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
        assert!(house().contains("house edit"), "{point}");
        assert_eq!(
            env.commits_starting_with("npx: keep kept"),
            1,
            "{point}: {:?}",
            env.commits()
        );
        assert!(
            env.live_lock()["skills"].get("kept").is_none(),
            "{point}: the lock still tracks it"
        );
        assert!(!env.record_path("kept").exists(), "{point}");
        assert!(!env.inbox_root().join("kept").exists(), "{point}");
        assert!(
            !env.paths.library.join(".npx/tmp/keep/kept").exists(),
            "{point}: the keep marker is left behind"
        );
        assert_eq!(
            env.npx_runs().len(),
            1,
            "{point}: the lock transition ran twice"
        );
        assert!(
            !env.paths.stage.exists(),
            "{point}: the stage root is left behind"
        );
    }
}

#[test]
fn a_change_to_another_names_record_is_refused() {
    let env = Env::new();
    env.inbox("arrival", "v1");
    fs::create_dir_all(env.record_path("other").parent().unwrap()).unwrap();
    fs::write(env.record_path("other"), "{}\n").unwrap();
    env.git(&["add", "-A"]);
    env.git(&["commit", "-q", "-m", "seed a record"]);
    fs::write(env.record_path("other"), "{\"hand\": \"edit\"}\n").unwrap();

    let error = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap_err();
    assert_eq!(error.code(), "library_dirty_unrelated");
    assert_eq!(error.exit_code(), 65);
    assert!(error.to_string().contains(".npx/other.json"), "{error}");
    assert!(
        env.inbox_root().join("arrival").is_dir(),
        "nothing was written"
    );
}

#[test]
fn an_untracked_record_for_another_name_is_refused() {
    let env = Env::new();
    env.inbox("arrival", "v1");
    // A wholly untracked `.npx/`: git collapses it to one line, and the rules
    // still have to see inside.
    fs::create_dir_all(env.record_path("other").parent().unwrap()).unwrap();
    fs::write(env.record_path("other"), "{}\n").unwrap();

    let error = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap_err();
    assert_eq!(error.code(), "library_dirty_unrelated");
    assert!(error.to_string().contains(".npx/other.json"), "{error}");
}

#[test]
fn a_lock_snapshot_change_is_refused() {
    let env = Env::new();
    env.inbox("arrival", "v1");
    env.lock(&[("elsewhere", "file:///nowhere.git")]);
    let snapshot = env
        .paths
        .library
        .join(skillherd::npx::lock_snapshot(&env.paths));
    fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
    fs::write(&snapshot, "{}\n").unwrap();
    env.git(&["add", "-A"]);
    env.git(&["commit", "-q", "-m", "seed the snapshot"]);
    fs::write(&snapshot, "{\"hand\": \"edit\"}\n").unwrap();

    let error = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap_err();
    assert_eq!(error.code(), "library_dirty_unrelated");
    assert!(error.to_string().contains("skill-lock."), "{error}");
}

#[test]
fn an_unrelated_gitignore_change_is_refused_and_our_own_is_not() {
    let env = Env::new();
    env.inbox("bridge-two", "v1");
    let path = env.paths.library.join(".gitignore");
    let head = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{head}house-note\n")).unwrap();

    let error = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap_err();
    assert_eq!(error.code(), "library_dirty_unrelated");
    assert!(error.to_string().contains(".gitignore"), "{error}");

    // This batch's own change — the arrival's line, and nothing else — passes,
    // and the import still lands.
    let stripped: String = head
        .split_inclusive('\n')
        .filter(|line| line.trim().trim_start_matches('/') != "bridge-two")
        .collect();
    fs::write(&path, &stripped).unwrap();
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(names(&report.imported), ["bridge-two"]);
    assert_eq!(fs::read_to_string(&path).unwrap(), stripped);
}

#[test]
fn a_staged_rename_is_refused_with_both_of_its_paths() {
    let env = Env::new();
    env.inbox("arrival", "v1");
    env.git(&["mv", "house", "house-moved"]);

    let error = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap_err();
    assert_eq!(error.code(), "library_dirty_unrelated");
    let paths: Vec<String> = error.details()["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| path.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        paths.len(),
        2,
        "a rename names both of its paths: {paths:?}"
    );
    assert!(
        paths.iter().any(|path| path.starts_with("house-moved/")),
        "{paths:?}"
    );
    assert!(
        paths.iter().any(|path| path.starts_with("house/")),
        "the rename's original path is named too: {paths:?}"
    );
    assert!(!env.paths.library.join("arrival").exists());
}

#[test]
fn the_scratch_is_exempt_from_the_preflight() {
    let env = Env::new();
    env.inbox("arrival", "v1");
    env.entry("other", "v1");
    env.git(&["add", "-A"]);
    env.git(&["commit", "-q", "-m", "seed other"]);

    // What a crashed run leaves in its own scratch: an incomplete copy, a
    // displaced entry, and a keep marker.
    let tmp = env.paths.library.join(".npx/tmp/arrival");
    fs::create_dir_all(&tmp).unwrap();
    fs::write(tmp.join("SKILL.md"), b"half a copy\n").unwrap();
    let backup = env.paths.library.join(".npx/backup/other");
    fs::create_dir_all(&backup).unwrap();
    fs::write(backup.join("SKILL.md"), b"displaced\n").unwrap();
    fs::create_dir_all(env.paths.library.join(".npx/tmp/keep")).unwrap();
    fs::write(
        env.paths.library.join(".npx/tmp/keep/elsewhere"),
        b"/nowhere\n",
    )
    .unwrap();

    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(names(&report.imported), ["arrival"]);
    assert!(
        fs::read_to_string(env.paths.library.join("arrival/SKILL.md"))
            .unwrap()
            .contains("v1")
    );
    assert!(
        backup.join("SKILL.md").is_file(),
        "a backup this run did not displace is never deleted"
    );
    assert!(!tmp.exists(), "this run's own scratch copy is gone");
    assert_eq!(
        fs::read(env.paths.library.join(".npx/tmp/keep/elsewhere")).unwrap(),
        b"/nowhere\n",
        "legacy shared-library markers are inert and untouched"
    );
}

/// The same bytes `fixtures/make-upstream.sh init` writes for the universal
/// copy, so a test can say "the arrival is exactly what upstream ships".
fn piece_body(name: &str, body: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: fixture skill for the reconcile tests\n---\n\n# {name}\n\n{body}\n"
    )
}

// ---- fix round 2 (Sol re-check: findings 3, 6 residuals; new finding 2) ----

/// A crash between the displace and the publish leaves `backup/<n>` as the only
/// copy; if something else then puts a *different* tree at the live path, the
/// next run must not read the backup's presence as "the publish landed" and
/// record the stranger as the import while deleting the displaced copy.
#[test]
fn an_overtaken_publish_is_a_conflict_that_keeps_both_copies() {
    let env = Env::new();
    let bare = env.upstream("taken", "claude variant");
    env.lock(&[("taken", &file_url(&bare))]);
    env.inbox("taken", "universal v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    let record_before = env.record("taken");
    let commits_before = env.commits().len();

    env.inbox("taken", "universal v2");
    let entry = env.paths.library.join("taken");
    let backup = env.paths.library.join(".npx/backup/taken");
    fs::create_dir_all(backup.parent().unwrap()).unwrap();
    fs::rename(&entry, &backup).unwrap();
    piece(&env.paths.library, "taken", "a tree somebody else put here");
    let stranger = tree_hash(&entry).unwrap();
    let displaced = tree_hash(&backup).unwrap();

    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(names(&report.imported).is_empty(), "{:?}", report.imported);
    assert_eq!(report.refused.len(), 1, "{:?}", report.refused);
    assert_eq!(report.refused[0].name, "taken");
    assert!(
        report.refused[0].message.contains(".npx/backup"),
        "{}",
        report.refused[0].message
    );
    assert_eq!(
        tree_hash(&entry).unwrap(),
        stranger,
        "the live tree is untouched"
    );
    assert_eq!(
        tree_hash(&backup).unwrap(),
        displaced,
        "the displaced copy is kept"
    );
    assert!(
        backup.join(".variants/claude/SKILL.md").is_file(),
        "the displaced copy still carries its variant"
    );
    assert_eq!(
        env.record("taken"),
        record_before,
        "the record is not rewritten"
    );
    assert_eq!(env.commits().len(), commits_before, "nothing was committed");
    assert!(
        env.inbox_root().join("taken").is_dir(),
        "the arrival stays in the inbox"
    );
}

/// The preflight allows or refuses every write. A stale scratch tree a crash
/// left for a name outside this batch is swept only after the preflight has
/// said yes; a refused run leaves it exactly where it was.
#[test]
fn a_stale_scratch_survives_a_refused_preflight() {
    let env = Env::new();
    let tmp = env.paths.library.join(".npx/tmp/house");
    fs::create_dir_all(&tmp).unwrap();
    fs::write(tmp.join("SKILL.md"), piece_body("house", "half a copy")).unwrap();
    env.inbox("arrival", "v1");
    piece(&env.paths.library, "staged-house", "house copy");
    env.git(&["add", "--", "staged-house"]);

    let error = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap_err();
    assert_eq!(error.code(), "library_dirty_unrelated");
    assert!(
        tmp.join("SKILL.md").is_file(),
        "the refused run swept the scratch"
    );
    assert!(!env.paths.library.join("arrival").exists());

    env.git(&["reset", "-q", "--", "staged-house"]);
    fs::remove_dir_all(env.paths.library.join("staged-house")).unwrap();
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(!tmp.exists(), "an allowed run sweeps the orphan scratch");
}

/// A run deletes under `.npx/backup` only what it displaced itself. Anything
/// an earlier run (or somebody) left there survives every path that used to
/// clear it: the sweep of a name with no arrival, the post-commit cleanup of
/// an entry that already equals the arrival, and the displace of a bridge
/// link or of an adopted conflict, which now refuse instead.
#[test]
fn a_backup_from_an_earlier_run_is_never_deleted() {
    let env = Env::new();

    // No arrival, live entry proven by its record: reported, kept.
    env.inbox("done", "v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(env.record("done").imported_hash, env.entry_hash("done"));
    let swept = env.paths.library.join(".npx/backup/done");
    piece(swept.parent().unwrap(), "done", "a tree of unknown origin");
    skillherd::output::clear_warnings();
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(swept.join("SKILL.md").is_file(), "the sweep deleted it");
    assert!(
        !report.plan.iter().any(|op| op.op == "delete"),
        "{:?}",
        report.plan
    );
    assert!(warning_codes().contains(&"stale_backup".to_string()));

    // Live entry equal to the arrival (the publish a crash landed): the
    // record and the commit are finished, the backup is reported and kept.
    env.inbox("landed", "v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    env.inbox("landed", "v1");
    let record = env.paths.library.join(".npx/landed.json");
    fs::remove_file(&record).unwrap();
    env.git(&["add", "-A", "--", ".npx/landed.json"]);
    env.git(&["commit", "-q", "-m", "drop the record"]);
    let leftover = env.paths.library.join(".npx/backup/landed");
    piece(leftover.parent().unwrap(), "landed", "the old tree");
    skillherd::output::clear_warnings();
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(names(&report.imported), ["landed"]);
    assert!(record.is_file(), "the record was not finished");
    assert!(
        leftover.join("SKILL.md").is_file(),
        "the cleanup deleted it"
    );
    assert!(
        !report
            .plan
            .iter()
            .any(|op| op.op == "delete" && op.path == leftover),
        "{:?}",
        report.plan
    );
    assert!(warning_codes().contains(&"stale_backup".to_string()));
    assert!(!env.inbox_root().join("landed").exists());

    // A bridge link with something at its backup path: refused, both kept.
    let inbox = env.inbox("bridged", "v1");
    std::os::unix::fs::symlink(&inbox, env.paths.library.join("bridged")).unwrap();
    let blocking = env.paths.library.join(".npx/backup/bridged");
    piece(blocking.parent().unwrap(), "bridged", "a foreign tree");
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(report.refused.len(), 1, "{:?}", report.refused);
    assert_eq!(report.refused[0].code, "stale_backup_refused");
    assert!(
        env.paths.library.join("bridged").is_symlink(),
        "the bridge was replaced"
    );
    assert!(
        blocking.join("SKILL.md").is_file(),
        "the backup was cleared"
    );

    // An adopted conflict with something at its backup path: still refused.
    env.inbox("edited", "v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    fs::write(
        env.paths.library.join("edited/SKILL.md"),
        piece_body("edited", "a house edit"),
    )
    .unwrap();
    env.git(&["add", "-A", "--", "edited"]);
    env.git(&["commit", "-q", "-m", "house edit"]);
    let house = env.entry_hash("edited");
    env.inbox("edited", "v2");
    let blocking = env.paths.library.join(".npx/backup/edited");
    fs::write(
        blocking.parent().unwrap().join("edited"),
        b"not even a directory\n",
    )
    .unwrap();
    let report = reconcile::run(&env.paths, opts(&["edited"], &[], false)).unwrap();
    let refusal = report
        .refused
        .iter()
        .find(|refusal| refusal.name == "edited")
        .unwrap_or_else(|| panic!("{:?}", report.refused));
    assert_eq!(refusal.code, "stale_backup_refused");
    assert_eq!(
        env.entry_hash("edited"),
        house,
        "the house copy was replaced"
    );
    assert!(blocking.is_file(), "the backup path was cleared");
    let error = reconcile::run(&env.paths, opts(&[], &["edited"], false)).unwrap_err();
    assert_eq!(error.code(), "usage", "{error}");
    assert!(error.to_string().contains(".npx/backup"), "{error}");
}

/// A stray file under `.npx/` (an editor's, a filesystem's) is tolerated while
/// it is untracked and unstaged; staged, it is somebody's half-made commit and
/// the run refuses like it does for any other staged path.
#[test]
fn a_staged_stray_npx_file_is_refused_and_an_unstaged_one_is_not() {
    let env = Env::new();
    let stray = env.paths.library.join(".npx/notes");
    fs::create_dir_all(stray.parent().unwrap()).unwrap();
    fs::write(&stray, "scratch notes\n").unwrap();
    env.inbox("first", "v1");
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(
        names(&report.imported),
        ["first"],
        "an unstaged stray is tolerated"
    );
    assert!(stray.is_file());

    env.inbox("second", "v1");
    env.git(&["add", "--", ".npx/notes"]);
    let error = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap_err();
    assert_eq!(error.code(), "library_dirty_unrelated");
    let paths = error.details()["paths"].clone();
    assert!(
        paths
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == ".npx/notes"),
        "{paths}"
    );
    assert!(
        !env.paths.library.join("second").exists(),
        "nothing was written"
    );
}

// ---- fix rounds 3–5 (Sol re-checks 2–4: a displaced copy moves only on request) ----

/// A displaced copy with no live entry is reported and never moved unasked:
/// not by a dry run, not by a real run. `--restore` plans it in a dry run
/// and does it in a real one.
#[test]
fn a_displaced_copy_moves_only_when_a_restore_is_requested() {
    let env = Env::new();
    env.inbox("still", "v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    env.inbox("still", "v2");
    let backup = displace(&env, "still");
    let entry = env.paths.library.join("still");
    let before = tree_hash(&backup).unwrap();

    for dry_run in [true, false] {
        skillherd::output::clear_warnings();
        let report = reconcile::run(&env.paths, opts(&[], &[], dry_run)).unwrap();
        assert!(
            !entry.exists(),
            "dry_run={dry_run}: moved unasked: {:?}",
            report.plan
        );
        assert_eq!(tree_hash(&backup).unwrap(), before, "dry_run={dry_run}");
        assert!(
            !report.plan.iter().any(|op| op.op == "restore"),
            "dry_run={dry_run}: {:?}",
            report.plan
        );
        assert_eq!(
            report.refused.len(),
            1,
            "dry_run={dry_run}: {:?}",
            report.refused
        );
        let codes = warning_codes();
        assert!(codes.contains(&"stage_displaced".to_string()), "{codes:?}");
        assert!(!codes.contains(&"stage_restored".to_string()), "{codes:?}");
    }

    let planned = reconcile::run(&env.paths, restore(&["still"], true)).unwrap();
    assert!(!entry.exists(), "a dry run restored: {:?}", planned.plan);
    assert!(
        planned.plan.iter().any(|op| op.op == "restore"
            && op.path == backup
            && op.target.as_deref() == Some(entry.as_path())),
        "{:?}",
        planned.plan
    );

    skillherd::output::clear_warnings();
    let mut recovery = restore(&["still"], false);
    recovery.adopt = strings(&["still"]); // Explicit consent for an unverified recovered arrival.
    let report = reconcile::run(&env.paths, recovery).unwrap();
    assert_eq!(names(&report.imported), ["still"]);
    assert!(warning_codes().contains(&"stage_restored".to_string()));
    assert!(!backup.exists());
    assert!(
        fs::read_to_string(entry.join("SKILL.md"))
            .unwrap()
            .contains("v2")
    );
}

/// `--restore` is refused when there is nothing to restore or when the live
/// path is taken; the run then changes nothing.
#[test]
fn a_restore_needs_a_backup_and_an_empty_live_path() {
    let env = Env::new();
    let error = reconcile::run(&env.paths, restore(&["absent"], false)).unwrap_err();
    assert_eq!(error.code(), "usage", "{error}");

    env.inbox("taken", "v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    let backup = env.paths.library.join(".npx/backup/taken");
    piece(backup.parent().unwrap(), "taken", "an older copy");
    let live = env.entry_hash("taken");
    let error = reconcile::run(&env.paths, restore(&["taken"], false)).unwrap_err();
    assert_eq!(error.code(), "usage", "{error}");
    assert!(error.to_string().contains("already exists"), "{error}");
    assert_eq!(env.entry_hash("taken"), live, "the live entry is untouched");
    assert!(backup.join("SKILL.md").is_file(), "the backup is untouched");
}

/// A crash right after the displace leaves the backup as the only copy; the
/// next run refuses that name, keeps every copy, and `--restore` converges.
#[test]
fn a_crash_after_the_displace_is_refused_until_restored() {
    let env = Env::new();
    interrupted_replacement(&env);
    let crashed = env.cli(&["reconcile"], Some(reconcile::failpoint::AFTER_DISPLACE));
    assert_eq!(crashed.status.code(), Some(70));
    let backup = env.paths.library.join(".npx/backup/crash");
    let entry = env.paths.library.join("crash");
    assert!(
        backup.join("SKILL.md").is_file(),
        "the backup is the only copy"
    );
    assert!(!entry.exists());
    let displaced = tree_hash(&backup).unwrap();

    let refused = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(refused.refused.len(), 1, "{:?}", refused.refused);
    assert_eq!(refused.refused[0].name, "crash");
    assert!(!entry.exists(), "restored unasked");
    assert_eq!(tree_hash(&backup).unwrap(), displaced);
    assert!(
        env.inbox_root().join("crash").is_dir(),
        "the inbox copy survives"
    );

    let report = reconcile::run(&env.paths, restore(&["crash"], false)).unwrap();
    assert_eq!(names(&report.imported), ["crash"]);
    assert!(!backup.exists());
    assert!(entry.join(".variants/claude/SKILL.md").is_file());
}

/// The scratch this tool recovers from must be its own real directories: a
/// symlink at `.npx/backup` would send every restore somewhere else.
#[test]
fn a_symlinked_scratch_dir_is_refused_before_anything_moves() {
    let env = Env::new();
    let elsewhere = env.home.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    piece(&elsewhere, "planted", "a directory reached through a link");
    fs::create_dir_all(env.paths.library.join(".npx")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, env.paths.library.join(".npx/backup")).unwrap();
    env.inbox("other", "v1");

    for options in [opts(&[], &[], false), restore(&["planted"], false)] {
        let error = reconcile::run(&env.paths, options).unwrap_err();
        assert_eq!(error.code(), "config_error", "{error}");
        assert!(error.to_string().contains(".npx/backup"), "{error}");
    }
    assert!(
        !env.paths.library.join("planted").exists(),
        "something was restored through the link"
    );
    assert!(elsewhere.join("planted/SKILL.md").is_file());
    assert!(
        !env.paths.library.join("other").exists(),
        "the run stopped before importing"
    );
    assert!(env.inbox_root().join("other").is_dir());
}

/// An import record fingerprints what the pool's git carries between machines.
/// Generated files excluded by a skill's or the pool's `.gitignore` do not
/// travel with a clone. Including them would misclassify the clone as a local edit.
#[test]
fn an_import_record_hashes_only_what_the_pool_commits() {
    fn plant(dir: &Path) {
        piece(dir.parent().unwrap(), "junky", "v1");
        fs::write(dir.join(".gitignore"), "transcript.json\n").unwrap();
        fs::create_dir_all(dir.join("scripts/__pycache__")).unwrap();
        fs::write(dir.join("scripts/__pycache__/x.pyc"), "bytecode").unwrap();
        fs::create_dir_all(dir.join("fixtures")).unwrap();
        fs::write(dir.join("fixtures/transcript.json"), "{}").unwrap();
    }

    let env = Env::new();
    let gitignore = env.paths.library.join(".gitignore");
    let mut text = fs::read_to_string(&gitignore).unwrap();
    text.push_str("__pycache__/\n");
    fs::write(&gitignore, text).unwrap();
    env.git(&["commit", "-q", "-am", "ignore caches"]);
    env.lock(&[("junky", "file:///nowhere.git")]);
    env.seed_inbox();
    plant(&env.inbox_root().join("junky"));
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(names(&report.imported), ["junky"]);
    assert!(
        env.paths
            .library
            .join("junky/scripts/__pycache__/x.pyc")
            .exists(),
        "the pool copy carries the junk (git leaves it uncommitted)"
    );

    // The other machine: a clone of the pool, which git delivers without the
    // ignored files, plus the same arrival in its inbox.
    let other = TempDir::new().unwrap();
    let mut theirs = Paths::fixture(other.path());
    theirs.npx = env.paths.npx.clone();
    fs::create_dir_all(theirs.library.parent().unwrap()).unwrap();
    let clone = Command::new("git")
        .args(["clone", "-q"])
        .arg(&env.paths.library)
        .arg(&theirs.library)
        .output()
        .unwrap();
    assert!(
        clone.status.success(),
        "{}",
        String::from_utf8_lossy(&clone.stderr)
    );
    for args in [
        ["config", "user.email", "fixture@example.invalid"],
        ["config", "user.name", "fixture"],
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&theirs.library)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    assert!(!theirs.library.join("junky/scripts/__pycache__").exists());
    assert!(
        !theirs
            .library
            .join("junky/fixtures/transcript.json")
            .exists()
    );
    fs::create_dir_all(theirs.lock.parent().unwrap()).unwrap();
    fs::copy(&env.paths.lock, &theirs.lock).unwrap();

    let doctor = skillherd::doctor::run(&theirs, false, false).unwrap();
    let codes: Vec<&str> = doctor
        .findings
        .iter()
        .map(|row| row.code.as_str())
        .collect();
    assert!(
        !codes.contains(&"local_edit"),
        "a fresh clone is not a local edit: {codes:?}"
    );

    let stage = skillherd::npx::prepare_stage(&theirs).unwrap();
    plant(&stage.inbox().join("junky"));
    let report = reconcile::run(&theirs, opts(&[], &[], false)).unwrap();
    assert!(
        report.refused.is_empty(),
        "the same arrival is already imported on the clone: {:?}",
        report.refused
    );
    assert_eq!(
        names(&report.imported),
        ["junky"],
        "the ordinary re-run: recorded, nothing replaced"
    );
    assert!(
        !report.plan.iter().any(|op| op.op == "copy"),
        "nothing was copied over the clone's copy: {:?}",
        report.plan
    );
    assert!(
        !theirs.library.join("junky/scripts/__pycache__").exists(),
        "the clone's copy is untouched"
    );
    assert!(
        !stage.inbox().join("junky").exists(),
        "an already-imported arrival is consumed"
    );
}

/// The keep step runs last, after every import is committed. When its staged
/// npx fails, the error still says what landed, so a partial import never
/// disappears behind the upstream command's error.
#[test]
fn a_failed_keep_still_reports_what_the_batch_imported() {
    let env = Env::new();
    env.lock(&[
        ("landed", "file:///nowhere.git"),
        ("kept", "file:///nowhere.git"),
    ]);
    env.inbox("landed", "v1");
    env.entry("kept", "house edit");
    env.inbox("kept", "upstream");
    env.conf(&["exit=1"]);

    let output = env.cli(&["reconcile", "--keep", "kept"], None);
    assert_eq!(
        output.status.code(),
        Some(75),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let envelope: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(envelope["error"]["code"], "npx_failed");
    let imported: Vec<String> = envelope["error"]["details"]["imported"]
        .as_array()
        .unwrap_or_else(|| panic!("no imported rows in {envelope}"))
        .iter()
        .map(|row| row["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(imported, ["landed"]);
    assert!(
        env.commits()
            .iter()
            .any(|line| line.contains("import landed")),
        "the import was committed before the keep failed: {:?}",
        env.commits()
    );
    assert!(
        env.paths.library.join("kept/SKILL.md").exists() && env.inbox_root().join("kept").exists(),
        "the kept name is untouched and its arrival still waits"
    );
}

#[test]
fn a_symlinked_upstream_harness_entry_is_resolved_not_refused() {
    let env = Env::new();
    let work = env.upstream_work_tree("linked", "claude variant");
    // An upstream can publish `.claude/skills/<n>` as a link to the
    // universal copy: that is no variant at all.
    let claude = work.join(".claude/skills/linked");
    fs::remove_dir_all(&claude).unwrap();
    std::os::unix::fs::symlink("../../.agents/skills/linked", &claude).unwrap();
    // A link that leaves the clone is not upstream content.
    let outside = env.home().join("outside/linked");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("SKILL.md"), "# not upstream\n").unwrap();
    fs::create_dir_all(work.join(".pi/skills")).unwrap();
    std::os::unix::fs::symlink(&outside, work.join(".pi/skills/linked")).unwrap();
    let bare = env.home().join("linked.git");
    env.publish_work(&bare);
    env.lock(&[("linked", &file_url(&bare))]);
    env.seed_inbox();
    let arrival = env.inbox_root().join("linked");
    fs::create_dir_all(&arrival).unwrap();
    fs::copy(
        work.join(".agents/skills/linked/SKILL.md"),
        arrival.join("SKILL.md"),
    )
    .unwrap();

    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(names(&report.imported), ["linked"]);
    assert!(report.refused.is_empty(), "{:?}", report.refused);
    let record = env.record("linked");
    assert!(
        record.variants.is_empty(),
        "a link to the universal copy is no variant, and an escaping link is ignored: {:?}",
        record.variants
    );
    assert!(!env.paths.library.join("linked/.variants").exists());
}

/// Replace the upstream work tree's `.claude/agents` with `files`.
fn write_upstream_agents(env: &Env, files: &[(&str, &str)]) {
    let dir = env.upstream_work().join(".claude/agents");
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    for (file, body) in files {
        fs::write(
            dir.join(file),
            format!(
                "---\nname: {}\n---\n\n{body}\n",
                file.trim_end_matches(".md")
            ),
        )
        .unwrap();
    }
}

/// The file names in a directory, sorted; empty when it does not exist.
fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|read| {
            read.map(|dirent| dirent.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Import `design` (a main-tier skill, so no render links it into the inbox)
/// from an upstream that ships Claude agents: its own, one that
/// only shares a word with it, and an unrelated one.
fn import_design_with_agents(env: &Env) -> PathBuf {
    let sets = env.paths.library.join(".globals/sets");
    let mut text = fs::read_to_string(&sets).unwrap();
    text.push_str("design  main\n");
    fs::write(&sets, text).unwrap();
    // Serving a skill's agents is opt-in (`.globals/agents`).
    fs::write(env.paths.library.join(".globals/agents"), "design\n").unwrap();
    env.git(&["add", ".globals/agents"]);
    env.git(&["commit", "-q", "-am", "serve design"]);
    let bare = env.home().join("design.git");
    env.upstream_work_tree("design", "claude variant");
    write_upstream_agents(
        env,
        &[
            ("design-helper.md", "helps"),
            ("design.md", "main agent"),
            ("designer-x.md", "a longer word, not ours"),
            ("other-agent.md", "another skill's agent"),
        ],
    );
    let cursor = env.upstream_work().join(".cursor/agents");
    fs::create_dir_all(&cursor).unwrap();
    fs::write(cursor.join("design-helper.md"), "cursor flavour\n").unwrap();
    env.publish_work(&bare);
    env.lock(&[("design", &file_url(&bare))]);
    env.inbox("design", "universal");
    bare
}

#[test]
fn upstream_agents_import_beside_the_skill_and_render_serves_them() {
    let env = Env::new();
    let bare = import_design_with_agents(&env);

    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(report.refused.is_empty(), "{:?}", report.refused);
    let stored = env.paths.library.join("design/.harness-agents/claude");
    assert_eq!(
        file_names(&stored),
        ["design-helper.md", "design.md"],
        "only the agents named for the skill are imported"
    );
    assert!(
        !env.paths
            .library
            .join("design/.harness-agents/cursor")
            .exists(),
        "v1 imports Claude agents only"
    );
    let record = env.record("design");
    assert_eq!(
        record.harness_agents.get("claude"),
        Some(&tree_hash(&stored).unwrap()),
        "the record carries the stored tree's hash"
    );
    assert_eq!(
        record.imported_hash,
        env.entry_hash("design"),
        "the agents stay out of the skill's own hash"
    );
    assert_eq!(
        env.git(&["ls-files", "design/.harness-agents"])
            .lines()
            .collect::<Vec<_>>(),
        [
            "design/.harness-agents/claude/design-helper.md",
            "design/.harness-agents/claude/design.md"
        ],
        "the agents are committed with the import"
    );

    let served = env.paths.claude_shared.join("agents");
    assert_eq!(
        fs::read_link(served.join("design-helper.md")).unwrap(),
        PathBuf::from("../../.agents/skill-library/design/.harness-agents/claude/design-helper.md")
    );
    assert!(
        fs::read_to_string(served.join("design.md"))
            .unwrap()
            .contains("main agent")
    );
    assert_eq!(file_names(&served), ["design-helper.md", "design.md"]);
    let render = report.render.expect("reconcile renders");
    assert_eq!(render.agents.len(), 1);
    assert_eq!(render.agents[0].linked, ["design-helper.md", "design.md"]);

    // Converged: nothing to import, nothing to serve again.
    let again = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(again.plan.is_empty(), "{:?}", again.plan);
    assert_eq!(
        again.render.unwrap().agents[0].unchanged,
        ["design-helper.md", "design.md"]
    );

    // Upstream edits one agent and drops the other.
    write_upstream_agents(
        &env,
        &[
            ("design-helper.md", "helps better"),
            ("other-agent.md", "x"),
        ],
    );
    env.publish_work(&bare);
    env.inbox("design", "universal v2");
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(report.refused.is_empty(), "{:?}", report.refused);
    assert_eq!(file_names(&stored), ["design-helper.md"]);
    assert!(
        fs::read_to_string(served.join("design-helper.md"))
            .unwrap()
            .contains("helps better")
    );
    assert!(
        fs::symlink_metadata(served.join("design.md")).is_err(),
        "render removed the link to the dropped agent"
    );
    assert_eq!(report.render.unwrap().agents[0].removed, ["design.md"]);
    assert_eq!(
        env.record("design").harness_agents.get("claude"),
        Some(&tree_hash(&stored).unwrap())
    );
}

#[test]
fn a_locally_edited_agent_survives_an_upstream_move_and_is_refused() {
    let env = Env::new();
    let bare = import_design_with_agents(&env);
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    let stored = env.paths.library.join("design/.harness-agents/claude");
    fs::write(stored.join("design-helper.md"), "hand edit\n").unwrap();
    let recorded = env.record("design").harness_agents["claude"].clone();

    // Upstream moves the skill and the agent; the arrival is upstream-newer.
    env.upstream_work_tree("design", "claude variant v2");
    write_upstream_agents(&env, &[("design-helper.md", "upstream v2")]);
    env.publish_work(&bare);
    env.inbox("design", "universal v2");
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();

    assert_eq!(report.refused.len(), 1, "{:?}", report.refused);
    assert_eq!(report.refused[0].code, "local_edit_refused");
    assert!(
        report.refused[0].message.contains(".harness-agents/claude"),
        "{:?}",
        report.refused
    );
    assert_eq!(
        fs::read_to_string(stored.join("design-helper.md")).unwrap(),
        "hand edit\n",
        "the edited agent was carried across the replacement and kept"
    );
    assert_eq!(
        file_names(&stored),
        ["design-helper.md", "design.md"],
        "nothing in the edited tree was removed"
    );
    assert_eq!(env.record("design").harness_agents["claude"], recorded);
    assert!(
        fs::read_to_string(env.paths.library.join("design/SKILL.md"))
            .unwrap()
            .contains("universal v2"),
        "the skill itself still updated"
    );
}

#[test]
fn remove_refuses_a_locally_edited_agent_until_forced() {
    let env = Env::new();
    import_design_with_agents(&env);
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    let stored = env.paths.library.join("design/.harness-agents/claude");
    fs::write(stored.join("design.md"), "hand edit\n").unwrap();
    let served = env.paths.claude_shared.join("agents");
    fs::write(served.join("worker.md"), "a house agent\n").unwrap();

    let error = remove::run(
        &RemoveArgs {
            names: vec!["design".to_string()],
            force: false,
        },
        &env.paths,
        false,
    )
    .unwrap_err();
    assert_eq!(error.code(), "local_edit_refused");
    assert!(
        error.to_string().contains(".harness-agents/claude"),
        "{error}"
    );
    assert!(stored.join("design.md").is_file());

    remove::run(
        &RemoveArgs {
            names: vec!["design".to_string()],
            force: true,
        },
        &env.paths,
        false,
    )
    .unwrap();
    assert!(!env.paths.library.join("design").exists());
    assert_eq!(
        file_names(&served),
        ["worker.md"],
        "the removal's render dropped our links and left the house agent"
    );
}

// ---- GitHub release assets ---------------------------------------------

const REPO: &str = "ComposioHQ/composio";
const ASSET: &str = "composio-skill.zip";
const PREFIX: &str = "@composio/cli@";
const CLI: &str = "composio-cli";

/// A stub `gh` beside the npx stub, serving a release listing and assets.
struct Releases {
    dir: PathBuf,
    log: PathBuf,
}

impl Releases {
    fn new(env: &mut Env) -> Self {
        let bin = env.home().join("bin");
        let stub = bin.join("gh");
        fs::copy(format!("{FIXTURES}/gh-stub.sh"), &stub).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        }
        env.paths.gh = Some(stub);
        let releases = Self {
            dir: env.home().join("gh"),
            log: bin.join("gh.log"),
        };
        fs::create_dir_all(releases.dir.join("assets")).unwrap();
        releases.conf(&[]);
        releases
    }

    fn conf(&self, lines: &[&str]) {
        let mut text = format!(
            "log={}\nlisting_dir={}\nasset_dir={}\n",
            self.log.display(),
            self.dir.display(),
            self.dir.join("assets").display()
        );
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        fs::write(self.log.parent().unwrap().join("gh.conf"), text).unwrap();
    }

    /// Page 1 of the listing: `(tag, prerelease, asset id)`, newest first.
    fn publish(&self, releases: &[(&str, bool, u64)]) {
        let rows: Vec<serde_json::Value> = releases
            .iter()
            .map(|(tag, prerelease, id)| {
                serde_json::json!({
                    "tag_name": tag,
                    "draft": false,
                    "prerelease": prerelease,
                    "published_at": "2026-09-04T18:38:20Z",
                    "assets": [
                        {"id": id + 1000, "name": "checksums.txt"},
                        {"id": id, "name": ASSET},
                    ],
                })
            })
            .collect();
        fs::write(
            self.dir.join("page-1.json"),
            serde_json::to_string(&rows).unwrap(),
        )
        .unwrap();
    }

    /// The zip for asset `id`: `composio-cli/` with this SKILL.md body.
    fn asset(&self, id: u64, body: &str) {
        self.asset_with(id, |writer, opts| {
            writer.add_directory(format!("{CLI}/"), opts).unwrap();
            writer.start_file(format!("{CLI}/SKILL.md"), opts).unwrap();
            std::io::Write::write_all(writer, piece_body(CLI, body).as_bytes()).unwrap();
            writer
                .start_file(format!("{CLI}/references/troubleshooting.md"), opts)
                .unwrap();
            std::io::Write::write_all(writer, b"reference\n").unwrap();
        });
    }

    fn asset_with(
        &self,
        id: u64,
        build: impl FnOnce(&mut zip::ZipWriter<fs::File>, zip::write::SimpleFileOptions),
    ) {
        let path = self.dir.join("assets").join(format!("{id}.zip"));
        let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        build(&mut writer, opts);
        writer.finish().unwrap();
    }

    /// Every `gh` argv, in order.
    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.strip_prefix("ARGV=").map(str::to_string))
            .collect()
    }

    fn downloads(&self) -> usize {
        self.calls()
            .iter()
            .filter(|argv| argv.contains("/releases/assets/"))
            .count()
    }

    /// Every working directory `gh` ran in.
    fn cwds(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.strip_prefix("PWD=").map(str::to_string))
            .collect()
    }
}

fn register_args(adopt: &[&str]) -> InstallArgs {
    InstallArgs {
        source: REPO.to_string(),
        skill: strings(&[CLI]),
        release_asset: Some(ASSET.to_string()),
        tag_prefix: Some(PREFIX.to_string()),
        release_dir: None,
        adopt: strings(adopt),
        keep: Vec::new(),
    }
}

fn update_args(names: &[&str], adopt: &[&str]) -> UpdateArgs {
    UpdateArgs {
        names: strings(names),
        adopt: strings(adopt),
        keep: Vec::new(),
    }
}

fn pool_skill_md(env: &Env, name: &str) -> String {
    fs::read_to_string(env.paths.library.join(name).join("SKILL.md")).unwrap()
}

/// The registration imports the newest stable asset through reconcile: a
/// `github-release` record naming the tag, the ordinary import commit, npx
/// never asked, and every fetch made inside the run's stage.
#[test]
fn a_release_install_registers_the_newest_stable_asset_through_reconcile() {
    let mut env = Env::new();
    let gh = Releases::new(&mut env);
    gh.publish(&[
        ("@composio/core@0.21.0", false, 9),
        ("@composio/cli@0.4.2-beta.402", true, 3),
        ("@composio/cli@0.4.1", false, 2),
        ("@composio/cli@0.4.0", false, 1),
    ]);
    gh.asset(3, "beta body");
    gh.asset(2, "stable 0.4.1");
    gh.asset(1, "stable 0.4.0");
    env.conf(&[]);

    let data = install::run(&register_args(&[]), &env.paths, false).unwrap();

    assert!(pool_skill_md(&env, CLI).contains("stable 0.4.1"), "{data}");
    assert!(
        env.paths
            .library
            .join(CLI)
            .join("references/troubleshooting.md")
            .is_file()
    );
    let record = env.record(CLI);
    assert_eq!(record.source_type, "github-release");
    assert_eq!(record.source, REPO);
    assert_eq!(record.revision, "@composio/cli@0.4.1");
    let release = record.release.clone().unwrap();
    assert_eq!(
        (
            release.repo.as_str(),
            release.asset.as_str(),
            release.tag_prefix.as_str(),
            release.dir.as_str()
        ),
        (REPO, ASSET, PREFIX, CLI)
    );
    assert_eq!(record.imported_hash, env.entry_hash(CLI));
    assert_eq!(
        env.commits_starting_with(&format!("npx: import {CLI} (")),
        1,
        "{:?}",
        env.commits()
    );
    assert_eq!(data["releases"][0]["tag"], "@composio/cli@0.4.1");
    assert_eq!(data["releases"][0]["fetched"], true);
    assert!(data.get("npx").is_none(), "{data}");
    assert!(
        env.npx_runs().is_empty(),
        "npx is not asked about a release"
    );
    assert_eq!(gh.downloads(), 1);
    assert!(
        gh.calls()
            .iter()
            .any(|argv| argv.ends_with("/releases/assets/2"))
    );
    // `gh` reports its physical working directory (`/private/var/…` on macOS).
    let stage_root = fs::canonicalize(env.home())
        .unwrap()
        .join(env.paths.stage.strip_prefix(env.home()).unwrap());
    let cwds = gh.cwds();
    assert!(!cwds.is_empty());
    for cwd in cwds {
        assert!(
            cwd.starts_with(&format!("{}/run-", stage_root.display())),
            "gh ran in {cwd}, outside the run's stage"
        );
    }
    assert!(!env.paths.stage.exists(), "the finished stage is cleared");
}

#[test]
fn a_release_update_is_a_noop_when_the_record_names_the_newest_tag() {
    let mut env = Env::new();
    let gh = Releases::new(&mut env);
    gh.publish(&[("@composio/cli@0.4.1", false, 2)]);
    gh.asset(2, "stable 0.4.1");
    env.conf(&[]);
    install::run(&register_args(&[]), &env.paths, false).unwrap();
    let head = env.git(&["rev-parse", "HEAD"]);
    let record = fs::read_to_string(env.record_path(CLI)).unwrap();

    // A newer beta is not a newer stable release.
    gh.publish(&[
        ("@composio/cli@0.4.2-beta.403", true, 4),
        ("@composio/cli@0.4.1", false, 2),
    ]);
    gh.asset(4, "beta body");
    let data = update::run(&update_args(&[CLI], &[]), &env.paths, false).unwrap();

    assert_eq!(data["releases"][0]["tag"], "@composio/cli@0.4.1");
    assert_eq!(data["releases"][0]["previous"], "@composio/cli@0.4.1");
    assert_eq!(data["releases"][0]["fetched"], false);
    assert_eq!(gh.downloads(), 1, "only the registration downloaded");
    assert!(
        env.npx_runs().is_empty(),
        "a named release update never runs npx"
    );
    assert_eq!(env.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(fs::read_to_string(env.record_path(CLI)).unwrap(), record);
    assert!(pool_skill_md(&env, CLI).contains("stable 0.4.1"));
}

/// `skillherd update` with no names checks release records beside the npx run,
/// and a newer stable tag replaces the pool copy through reconcile.
#[test]
fn a_release_update_imports_a_newer_stable_asset() {
    let mut env = Env::new();
    let gh = Releases::new(&mut env);
    gh.publish(&[("@composio/cli@0.4.1", false, 2)]);
    gh.asset(2, "stable 0.4.1");
    env.conf(&[]);
    install::run(&register_args(&[]), &env.paths, false).unwrap();

    gh.publish(&[
        ("@composio/cli@0.4.3-beta.1", true, 5),
        ("@composio/cli@0.4.2", false, 4),
        ("@composio/cli@0.4.1", false, 2),
    ]);
    gh.asset(4, "stable 0.4.2");
    gh.asset(5, "beta body");
    let data = update::run(&update_args(&[], &[]), &env.paths, false).unwrap();

    assert!(pool_skill_md(&env, CLI).contains("stable 0.4.2"), "{data}");
    let record = env.record(CLI);
    assert_eq!(record.revision, "@composio/cli@0.4.2");
    assert_eq!(record.imported_hash, env.entry_hash(CLI));
    assert_eq!(
        env.commits_starting_with(&format!("npx: import {CLI} (")),
        2
    );
    assert_eq!(data["releases"][0]["previous"], "@composio/cli@0.4.1");
    assert_eq!(data["releases"][0]["fetched"], true);
    let runs = env.npx_runs();
    assert_eq!(runs.len(), 1, "the unnamed update still runs npx: {runs:?}");
    assert!(runs[0].1.contains(" update -g -y"), "{runs:?}");
    assert!(!env.paths.stage.exists());
}

/// A pool copy edited since the last import is refused like any npx arrival;
/// the hinted `--adopt` takes the release.
#[test]
fn a_release_update_refuses_a_local_edit_until_adopted() {
    let mut env = Env::new();
    let gh = Releases::new(&mut env);
    gh.publish(&[("@composio/cli@0.4.1", false, 2)]);
    gh.asset(2, "stable 0.4.1");
    env.conf(&[]);
    install::run(&register_args(&[]), &env.paths, false).unwrap();
    fs::write(
        env.paths.library.join(CLI).join("SKILL.md"),
        piece_body(CLI, "house edit"),
    )
    .unwrap();
    env.git(&["commit", "-q", "-am", "house edit"]);

    gh.publish(&[("@composio/cli@0.4.2", false, 4)]);
    gh.asset(4, "stable 0.4.2");
    let error = update::run(&update_args(&[CLI], &[]), &env.paths, false).unwrap_err();
    assert_eq!(error.code(), "local_edit_refused");
    assert_eq!(error.exit_code(), 65);
    assert!(
        error
            .suggested_fix()
            .unwrap()
            .contains("`skillherd update n --adopt n`"),
        "{error:?}"
    );
    assert!(pool_skill_md(&env, CLI).contains("house edit"));
    assert_eq!(env.record(CLI).revision, "@composio/cli@0.4.1");

    update::run(&update_args(&[CLI], &[CLI]), &env.paths, false).unwrap();
    assert!(pool_skill_md(&env, CLI).contains("stable 0.4.2"));
    assert_eq!(env.record(CLI).revision, "@composio/cli@0.4.2");
    assert_eq!(env.record(CLI).imported_hash, env.entry_hash(CLI));
}

/// Registering over a pool copy that has no import record is a conflict, and
/// the refusal's retry carries the release flags.
#[test]
fn a_release_install_over_an_untracked_copy_needs_adopt() {
    let mut env = Env::new();
    let gh = Releases::new(&mut env);
    gh.publish(&[("@composio/cli@0.4.1", false, 2)]);
    gh.asset(2, "stable 0.4.1");
    env.conf(&[]);
    env.entry(CLI, "copied in by hand");
    env.git(&["add", CLI]);
    env.git(&["commit", "-q", "-m", "hand copy"]);

    let error = install::run(&register_args(&[]), &env.paths, false).unwrap_err();
    assert_eq!(error.code(), "local_edit_refused");
    let fix = error.suggested_fix().unwrap();
    assert!(
        fix.contains(&format!(
            "`skillherd install {REPO} --release-asset {ASSET} --tag-prefix {PREFIX} --skill n --adopt n`"
        )),
        "{fix}"
    );
    assert!(pool_skill_md(&env, CLI).contains("copied in by hand"));
    assert!(!env.record_path(CLI).exists());

    install::run(&register_args(&[CLI]), &env.paths, false).unwrap();
    assert!(pool_skill_md(&env, CLI).contains("stable 0.4.1"));
    assert_eq!(env.record(CLI).source_type, "github-release");
}

/// GitHub refusing the request (the anonymous rate limit that broke
/// `composio --install-skill`) is a typed error, and the run stops before npx
/// or the pool is touched.
#[test]
fn a_github_auth_failure_is_a_typed_error_that_touches_nothing() {
    let mut env = Env::new();
    let gh = Releases::new(&mut env);
    gh.publish(&[("@composio/cli@0.4.1", false, 2)]);
    gh.asset(2, "stable 0.4.1");
    env.conf(&[]);
    install::run(&register_args(&[]), &env.paths, false).unwrap();
    let head = env.git(&["rev-parse", "HEAD"]);

    gh.conf(&[
        "exit=1",
        "stderr=gh: API rate limit exceeded for 203.0.113.9. (HTTP 403)",
    ]);
    let error = update::run(&update_args(&[], &[]), &env.paths, false).unwrap_err();
    assert_eq!(error.code(), "github_auth_failed", "{error:?}");
    assert_eq!(error.exit_code(), 77);
    assert!(error.to_string().contains("HTTP 403"), "{error}");
    assert!(error.suggested_fix().unwrap().contains("gh auth login"));
    assert!(
        env.npx_runs().is_empty(),
        "npx never ran, so the live lock is untouched"
    );
    assert_eq!(env.git(&["rev-parse", "HEAD"]), head);
}

#[test]
fn a_hostile_release_asset_is_rejected_before_the_pool_is_touched() {
    let mut env = Env::new();
    let gh = Releases::new(&mut env);
    gh.publish(&[("@composio/cli@0.4.1", false, 2)]);
    gh.asset_with(2, |writer, opts| {
        writer.start_file(format!("{CLI}/SKILL.md"), opts).unwrap();
        std::io::Write::write_all(writer, piece_body(CLI, "evil").as_bytes()).unwrap();
        writer
            .start_file(format!("{CLI}/../../../escaped"), opts)
            .unwrap();
    });
    env.conf(&[]);

    let error = install::run(&register_args(&[]), &env.paths, false).unwrap_err();
    assert_eq!(error.code(), "release_archive_rejected", "{error:?}");
    assert!(!env.paths.library.join(CLI).exists());
    assert!(!env.record_path(CLI).exists());
    assert!(!env.home().join(".agents/escaped").exists());
    assert!(!env.paths.stage.parent().unwrap().join("escaped").exists());
}

#[test]
fn a_dry_run_release_update_plans_the_fetch_without_making_it() {
    let mut env = Env::new();
    let gh = Releases::new(&mut env);
    gh.publish(&[("@composio/cli@0.4.1", false, 2)]);
    gh.asset(2, "stable 0.4.1");
    env.conf(&[]);
    install::run(&register_args(&[]), &env.paths, false).unwrap();
    let calls = gh.calls().len();

    let data = update::run(&update_args(&[CLI], &[]), &env.paths, true).unwrap();
    let ops: Vec<&serde_json::Value> = data["plan"].as_array().unwrap().iter().collect();
    let fetch = ops
        .iter()
        .find(|op| op["op"] == "github-release")
        .unwrap_or_else(|| panic!("no fetch op: {data}"));
    assert!(fetch["target"].as_str().unwrap().contains(REPO), "{fetch}");
    assert!(!ops.iter().any(|op| op["op"] == "npx"), "{data}");
    assert_eq!(gh.calls().len(), calls, "a dry run never calls GitHub");
}

/// A raw arrival for a release-tracked name (`composio --install-skill` writes
/// into `~/.agents/skills`) is imported by a plain reconcile without treating
/// the record as an npx source: no clone, no pending variants, the release
/// block kept.
#[test]
fn a_raw_arrival_for_a_release_record_is_not_cloned_for_variants() {
    let mut env = Env::new();
    let gh = Releases::new(&mut env);
    gh.publish(&[("@composio/cli@0.4.1", false, 2)]);
    gh.asset(2, "stable 0.4.1");
    env.conf(&[]);
    install::run(&register_args(&[]), &env.paths, false).unwrap();
    // Point the informational URL somewhere a clone fails fast and offline.
    let mut record = env.record(CLI);
    record.source_url = file_url(&env.home().join("no-such-upstream.git"));
    fs::write(
        env.record_path(CLI),
        serde_json::to_string_pretty(&record).unwrap() + "\n",
    )
    .unwrap();
    env.git(&["commit", "-q", "-am", "record url"]);

    env.inbox(CLI, "raw install");
    let report = reconcile::run(&env.paths, opts(&[CLI], &[], false)).unwrap();

    assert_eq!(names(&report.imported), [CLI]);
    assert!(report.variants_pending.is_empty(), "{report:?}");
    assert!(!warning_codes().contains(&"variants_unavailable".to_string()));
    let after = env.record(CLI);
    assert_eq!(after.source_type, "github-release");
    assert!(after.release.is_some());
    assert!(!after.variants_pending);
    assert_eq!(after.imported_hash, env.entry_hash(CLI));
    assert!(pool_skill_md(&env, CLI).contains("raw install"));
}

#[test]
fn imported_subagent_definitions_are_visible_in_the_command_output() {
    let env = Env::new();
    import_design_with_agents(&env);
    let output = env.cli(&["reconcile"], None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let warnings = value["meta"]["warnings"].as_array().unwrap();
    let notice = warnings
        .iter()
        .find(|warning| warning["code"] == "imported_subagents")
        .expect("the import must disclose its subagent definitions");
    let message = notice["message"].as_str().unwrap();
    for text in [
        "design",
        "claude",
        "design-helper.md",
        "design.md",
        "instructions",
    ] {
        assert!(message.contains(text), "{message}");
    }
}

#[test]
fn update_reports_local_path_imports_instead_of_silently_skipping_them() {
    let env = Env::new();
    env.inbox("local-copy", "imported without a remote");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(env.record("local-copy").source_type, "local");
    for args in [
        vec!["--dry-run", "update"],
        vec!["--dry-run", "update", "local-copy"],
    ] {
        let output = env.cli(&args, None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let warnings = value["meta"]["warnings"].as_array().unwrap();
        let notice = warnings
            .iter()
            .find(|warning| warning["code"] == "local_update_skipped")
            .expect("local imports need an explicit update warning");
        let message = notice["message"].as_str().unwrap();
        assert!(
            message.contains("local-copy") && message.contains("skillherd install"),
            "{message}"
        );
    }
}

#[test]
fn lock_snapshots_are_opt_in_and_refuse_symlinks() {
    let env = Env::new();
    env.lock(&[]);
    // `cli` runs the binary as SKILLHERD_PLATFORM=mac, whatever the host.
    let mut binary_paths = env.paths.clone();
    binary_paths.platform = skillherd::model::Platform::Mac;
    let snapshot = env
        .paths
        .library
        .join(skillherd::npx::lock_snapshot(&binary_paths));
    let output = env.cli(&["reconcile"], None);
    assert!(output.status.success());
    assert!(
        !snapshot.exists(),
        "a default run must not copy the lock into the library"
    );
    let opt_in = [("SKILLHERD_SNAPSHOT_LOCK".into(), "1".into())];
    let output = env.cli_with_env(&["reconcile"], None, &opt_in);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(&snapshot).unwrap(),
        fs::read(&env.paths.lock).unwrap()
    );
    fs::remove_file(&snapshot).unwrap();
    let victim = env.home().join("outside-lock");
    fs::write(&victim, "operator data\n").unwrap();
    std::os::unix::fs::symlink(&victim, &snapshot).unwrap();
    let output = env.cli_with_env(&["reconcile"], None, &opt_in);
    assert!(
        !output.status.success(),
        "a symlinked snapshot must be refused"
    );
    assert_eq!(fs::read_to_string(victim).unwrap(), "operator data\n");
}

#[test]
fn foreign_shared_skills_are_not_an_inbox() {
    let env = Env::new();
    let foreign = piece(&env.paths.agents_dir, "foreign", "placed by another tool");
    let before = tree_hash(&foreign).unwrap();
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(report.imported.is_empty(), "foreign skills were imported");
    assert_eq!(tree_hash(&foreign).unwrap(), before);
    assert!(!env.paths.library.join("foreign").exists());
    env.conf(&[]);
    let output = env.cli(&["install", "owner/repo"], None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(tree_hash(&foreign).unwrap(), before);
    assert!(!env.paths.library.join("foreign").exists());
}

#[test]
fn shared_recovery_markers_cannot_delete_unrelated_directories() {
    let env = Env::new();
    let victim = piece(&env.home().join("Documents"), "victim", "operator data");
    let before = tree_hash(&victim).unwrap();
    fs::create_dir_all(&env.paths.agents_dir).unwrap();
    let mark = env.paths.library.join(".npx/tmp/keep/forged");
    fs::create_dir_all(mark.parent().unwrap()).unwrap();
    fs::write(
        &mark,
        format!(
            "{}/../../Documents/victim\n",
            env.paths.agents_dir.display()
        ),
    )
    .unwrap();
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(tree_hash(&victim).unwrap(), before);
    assert!(
        !skillherd::provenance::load_house(&env.paths)
            .unwrap()
            .contains("forged")
    );

    // Even a marker forged in tool-local state is not a deletion capability.
    let key = skillherd::hash::sha256_hex(env.paths.library.as_os_str().as_encoded_bytes());
    let private_mark = env
        .home()
        .join(".local/state/skillherd/reconcile")
        .join(key)
        .join("keep/forged");
    fs::create_dir_all(private_mark.parent().unwrap()).unwrap();
    fs::write(
        &private_mark,
        format!(
            "{}/../../Documents/victim\n",
            env.paths.agents_dir.display()
        ),
    )
    .unwrap();
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(
        !private_mark.exists(),
        "stale recovery state must not wedge subsequent runs"
    );
    assert!(warning_codes().contains(&"stale_keep_marker".to_string()));
    assert_eq!(tree_hash(&victim).unwrap(), before);
}

#[test]
fn recovered_arrivals_never_silently_downgrade_a_clean_import() {
    let env = Env::new();
    env.inbox("versioned", "v1");
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    let clean_v1 = fs::read(env.paths.library.join("versioned/SKILL.md")).unwrap();
    env.entry("versioned", "local edit");
    env.inbox("versioned", "v2");
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert_eq!(report.refused.len(), 1);
    fs::write(env.paths.library.join("versioned/SKILL.md"), clean_v1).unwrap();
    let report = reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(
        report.imported.is_empty(),
        "a recovered arrival must require adoption"
    );
    assert_eq!(report.refused[0].code, "stale_arrival");
    assert!(pool_skill_md(&env, "versioned").contains("v1"));
    assert!(env.inbox_root().join("versioned").is_dir());
    let report = reconcile::run(&env.paths, opts(&["versioned"], &[], false)).unwrap();
    assert!(report.refused.is_empty());
    assert!(pool_skill_md(&env, "versioned").contains("v2"));
}

#[test]
fn explicit_discard_removes_only_owned_arrivals_and_dry_run_keeps_them() {
    let env = Env::new();
    env.entry("obsolete", "installed copy");
    let installed = env.entry_hash("obsolete");
    env.git(&["add", "obsolete"]);
    env.git(&["commit", "-q", "-m", "local skill"]);
    env.inbox("obsolete", "retained arrival");
    let arrival = env.inbox_root().join("obsolete");
    let foreign = piece(&env.paths.agents_dir, "obsolete", "foreign copy");
    let foreign_hash = tree_hash(&foreign).unwrap();
    let planned = env.cli(&["--dry-run", "reconcile", "--discard", "obsolete"], None);
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    assert!(arrival.is_dir());
    let discarded = env.cli(&["reconcile", "--discard", "obsolete"], None);
    assert!(
        discarded.status.success(),
        "{}",
        String::from_utf8_lossy(&discarded.stderr)
    );
    assert!(!arrival.exists());
    assert_eq!(env.entry_hash("obsolete"), installed);
    assert_eq!(tree_hash(&foreign).unwrap(), foreign_hash);
}

#[test]
fn a_current_import_discards_older_arrivals_of_that_name() {
    let env = Env::new();
    env.inbox("versioned", "stale v2");
    let old = env.inbox_root().join("versioned");
    let current = skillherd::npx::prepare_stage(&env.paths).unwrap();
    piece(&current.inbox(), "versioned", "current v3");
    let mut options = opts(&[], &[], false);
    options.inbox = vec![current.inbox()];
    reconcile::run(&env.paths, options).unwrap();
    assert!(
        !old.exists(),
        "a successful current import invalidates its older arrivals"
    );
    reconcile::run(&env.paths, opts(&[], &[], false)).unwrap();
    assert!(pool_skill_md(&env, "versioned").contains("current v3"));
}

#[test]
fn a_symlinked_home_ancestor_does_not_block_install() {
    let env = Env::new();
    env.conf(&[]);
    let alias_parent = TempDir::new().unwrap();
    let alias = alias_parent.path().join("home");
    std::os::unix::fs::symlink(env.home(), &alias).unwrap();
    let output = env.cli_with_env(
        &["install", "owner/repo"],
        None,
        &[("HOME".into(), alias.to_string_lossy().into_owned())],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn metadata_writers_refuse_symlink_leaves_and_parents() {
    for parent_link in [false, true] {
        let env = Env::new();
        let outside = env.home().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("house");
        fs::write(&victim, "operator data\n").unwrap();
        let globals = env.paths.library.join(".globals");
        if parent_link {
            fs::remove_file(globals.join("sets")).unwrap();
            fs::remove_dir(&globals).unwrap();
            std::os::unix::fs::symlink(&outside, &globals).unwrap();
        } else {
            std::os::unix::fs::symlink(&victim, globals.join("house")).unwrap();
        }
        assert!(skillherd::provenance::load_house(&env.paths).is_err());
        assert!(skillherd::provenance::declare_house(&env.paths, "new", None).is_err());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "operator data\n");
    }
}

#[test]
fn an_import_record_temp_symlink_never_overwrites_its_target() {
    let env = Env::new();
    env.inbox("arrival", "new upstream");
    let victim = env.home().join("outside");
    fs::write(&victim, "operator data\n").unwrap();
    let tmp = env.paths.library.join(".npx/tmp");
    fs::create_dir_all(&tmp).unwrap();
    std::os::unix::fs::symlink(&victim, tmp.join("arrival.json")).unwrap();
    let _ = reconcile::run(&env.paths, opts(&[], &[], false));
    assert_eq!(fs::read_to_string(&victim).unwrap(), "operator data\n");
}

#[test]
fn npx_credentials_are_explicitly_opt_in_and_redirected_not_sandboxed() {
    for opt_in in [false, true] {
        let env = Env::new();
        fs::write(
            &env.paths.npx,
            "#!/bin/sh\nenv > \"$HOME/child-env\"\nexit 19\n",
        )
        .unwrap();
        fs::write(
            env.home().join(".npmrc"),
            "//example.invalid/:_authToken=fixture-token\n",
        )
        .unwrap();
        fs::write(env.home().join(".gitconfig"), "[user]\nname = fixture\n").unwrap();
        let mut extra = vec![(
            "SKILLHERD_NPX_CREDENTIALS".into(),
            if opt_in { "1" } else { "0" }.into(),
        )];
        for key in [
            "SSH_AUTH_SOCK",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "NPM_TOKEN",
            "NODE_AUTH_TOKEN",
        ] {
            extra.push((key.into(), "fixture-token".into()));
        }
        let output = env.cli_with_env(&["install", "owner/repo"], None, &extra);
        assert_eq!(output.status.code(), Some(75));
        let run = skillherd::npx::recoverable_inboxes(&env.paths).unwrap()[0]
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let observed = fs::read_to_string(run.join("child-env")).unwrap();
        let child: BTreeMap<_, _> = observed
            .lines()
            .filter_map(|line| line.split_once('='))
            .collect();
        assert_eq!(child.get("GIT_TERMINAL_PROMPT"), Some(&"0"));
        for key in [
            "SSH_AUTH_SOCK",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "NPM_TOKEN",
            "NODE_AUTH_TOKEN",
        ] {
            assert_eq!(
                child.get(key).copied(),
                if opt_in { Some("fixture-token") } else { None },
                "{key}"
            );
        }
        if opt_in {
            assert_eq!(
                Path::new(child["GIT_CONFIG_GLOBAL"]),
                env.home().join(".gitconfig")
            );
            assert_eq!(
                Path::new(child["npm_config_userconfig"]),
                env.home().join(".npmrc")
            );
        } else {
            assert!(!child.contains_key("GIT_CONFIG_GLOBAL"));
            assert_eq!(Path::new(child["npm_config_userconfig"]), run.join("npmrc"));
        }
        assert!(!run.join(".ssh").exists());
        assert!(!run.join(".gitconfig").exists());
    }
}

#[test]
fn npx_network_settings_and_a_persistent_cache_follow_operator_configuration() {
    let env = Env::new();
    fs::write(
        &env.paths.npx,
        "#!/bin/sh\nenv > \"$HOME/child-env\"\nexit 19\n",
    )
    .unwrap();
    let keys = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "all_proxy",
        "NODE_EXTRA_CA_CERTS",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "GIT_SSL_CAINFO",
        "npm_config_registry",
        "npm_config_proxy",
        "npm_config_https_proxy",
        "npm_config_cafile",
        "npm_config_strict_ssl",
        "NPM_CONFIG_REGISTRY",
        "NPM_CONFIG_PROXY",
        "NPM_CONFIG_HTTPS_PROXY",
        "NPM_CONFIG_CAFILE",
        "NPM_CONFIG_STRICT_SSL",
    ];
    let cache = env.home().join("operator-cache");
    let mut extra: Vec<(String, String)> = keys
        .iter()
        .map(|key| ((*key).into(), "operator-value".into()))
        .collect();
    extra.push((
        "XDG_CACHE_HOME".into(),
        cache.to_string_lossy().into_owned(),
    ));
    for _ in 0..2 {
        let output = env.cli_with_env(&["install", "owner/repo"], None, &extra);
        assert_eq!(output.status.code(), Some(75));
        for inbox in skillherd::npx::recoverable_inboxes(&env.paths).unwrap() {
            let run = inbox.parent().unwrap().parent().unwrap();
            let observed = fs::read_to_string(run.join("child-env")).unwrap();
            let child: BTreeMap<_, _> = observed
                .lines()
                .filter_map(|line| line.split_once('='))
                .collect();
            for key in keys {
                assert_eq!(child.get(key), Some(&"operator-value"), "{key}");
            }
            assert_eq!(
                Path::new(child["npm_config_cache"]),
                cache.join("skillherd/npm")
            );
            assert!(cache.join("skillherd/npm").is_dir());
        }
    }
}

#[test]
fn the_npx_child_has_an_isolated_cwd_config_and_environment() {
    let env = Env::new();
    fs::write(&env.paths.npx, r#"#!/bin/sh
printf '%s\n' "$PWD" "$HOME" "$npm_config_userconfig" "$npm_config_globalconfig" "$DISABLE_TELEMETRY" "$DO_NOT_TRACK" "${GH_TOKEN-unset}" "${GITHUB_TOKEN-unset}" "${NPM_TOKEN-unset}" "${SSH_AUTH_SOCK-unset}" "${NODE_OPTIONS-unset}" "${npm_config_registry-unset}" > "$HOME/child-observed"
printf 'synthetic-sensitive-value\n' >&2
exit 19
"#).unwrap();
    let output = env.cli_with_env(
        &["install", "owner/repo"],
        None,
        &[
            ("GH_TOKEN".into(), "synthetic-sensitive-value".into()),
            ("GITHUB_TOKEN".into(), "synthetic-sensitive-value".into()),
            ("NPM_TOKEN".into(), "synthetic-sensitive-value".into()),
            ("SSH_AUTH_SOCK".into(), "/synthetic/socket".into()),
            ("NODE_OPTIONS".into(), "--require=/synthetic/evil.js".into()),
            (
                "npm_config_registry".into(),
                "https://example.invalid".into(),
            ),
        ],
    );
    assert_eq!(output.status.code(), Some(75));
    let run = fs::read_dir(&env.paths.stage)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| entry.file_name().to_string_lossy().starts_with("run-"))
        .unwrap()
        .path();
    let observed = fs::read_to_string(run.join("child-observed")).unwrap();
    let lines: Vec<_> = observed.lines().collect();
    assert_eq!(
        fs::canonicalize(lines[0]).unwrap(),
        fs::canonicalize(run.join("work")).unwrap(),
        "the caller cwd reached npx"
    );
    assert_eq!(Path::new(lines[1]), run);
    assert_eq!(Path::new(lines[2]), run.join("npmrc"));
    assert_eq!(Path::new(lines[3]), run.join("npmrc"));
    assert_eq!(&lines[4..6], &["1", "1"]);
    assert!(
        lines[6..11].iter().all(|line| *line == "unset"),
        "{observed}"
    );
    assert_eq!(
        lines[11], "https://example.invalid",
        "the operator's registry must reach npx"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-sensitive-value"));
}
