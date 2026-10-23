//! The command surface: Clap derive, kebab-case, agent-facing help.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// One pool, one policy file, one renderer.
#[derive(Debug, Parser)]
#[command(
    name = "skillherd",
    version,
    about = "Manage cross-harness skills from one pool, one policy file and one renderer",
    long_about = None,
    arg_required_else_help = true,
    subcommand_required = true,
    color = clap::ColorChoice::Never,
    disable_help_subcommand = true,
    rename_all = "kebab-case"
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

/// Flags valid on every subcommand.
#[derive(Debug, Clone, Args)]
pub struct GlobalArgs {
    /// Emit pretty-printed JSON.
    #[arg(long, global = true, default_value_t = false)]
    pub pretty: bool,

    /// Emit the JSON envelope even on a terminal (`on`, `off`, `render` and
    /// `doctor` print a short summary there otherwise; piped output is JSON).
    #[arg(long, global = true, default_value_t = false)]
    pub json: bool,

    /// Report the operations without changing anything.
    #[arg(long, global = true, default_value_t = false)]
    pub dry_run: bool,

    /// Correlation id echoed in the envelope; generated when omitted.
    #[arg(long, global = true, value_name = "ID")]
    pub correlation_id: Option<String>,

    /// Pool folder (overrides SKILLHERD_LIBRARY and config.json).
    #[arg(long, global = true, value_name = "DIR")]
    pub pool: Option<PathBuf>,

    /// Git remote to clone/pull with `sync` (overrides config and environment).
    #[arg(long, global = true, value_name = "URL")]
    pub pool_remote: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run npx; all following arguments belong to npx. Put skillherd flags before npx.
    #[command(disable_help_flag = true)]
    Npx(crate::commands::npx::NpxArgs),

    /// Print the machine contract: input surface, output schemas, error codes.
    Schema(SchemaArgs),

    /// Diagnose the pool, the policy and the surfaces.
    Doctor(DoctorArgs),

    /// List skills, by harness, by tier, or the whole catalog.
    List(ListArgs),

    /// Search names and descriptions.
    Search(SearchArgs),

    /// Describe one skill: tiers, harnesses that see it, variants, npx record.
    Info(InfoArgs),

    /// Activate skills for a tier and render.
    On(OnArgs),

    /// Deactivate skills and render.
    Off(OffArgs),

    /// Activate skills in a project so its harnesses read them.
    Add(AddArgs),

    /// Deactivate skills from a project.
    Drop(DropArgs),

    /// Rebuild every owned surface from pool plus policy; idempotent.
    Render(RenderArgs),

    /// Import the npx inbox into the pool, then render.
    Reconcile(ReconcileArgs),

    /// Install an upstream skill through a staged npx, then reconcile.
    Install(InstallArgs),

    /// Update installed skills through a staged npx, then reconcile.
    Update(UpdateArgs),

    /// Remove upstream skills: policy rows, npx record, pool entry.
    Remove(RemoveArgs),

    /// Clone/pull a configured pool remote and render, or render a plain folder.
    Sync(SyncArgs),

    /// Strip fenced blocks from Markdown files, to stdout or in place.
    Strip(StripArgs),
}

/// Which contract to print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SchemaTarget {
    All,
    Input,
    Output,
    Error,
}

#[derive(Debug, Clone, Args)]
pub struct SchemaArgs {
    /// Contract section to print.
    #[arg(value_enum, default_value_t = SchemaTarget::All)]
    pub target: SchemaTarget,
}

#[derive(Debug, Clone, Args)]
pub struct DoctorArgs {
    /// Apply the fixes that are safe and reversible.
    #[arg(long, default_value_t = false)]
    pub fix: bool,

    /// Print the finding codes, their fixability and the exit dictionary.
    #[arg(long, default_value_t = false)]
    pub capabilities: bool,
}

/// The tier filter for `list` and `on`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TierArg {
    Main,
    Lane,
}

#[derive(Debug, Clone, Args)]
pub struct ListArgs {
    /// Print the set this harness sees (its read roots unioned).
    #[arg(long, value_name = "HARNESS", conflicts_with = "tier")]
    pub harness: Option<String>,

    /// Print the set for one tier.
    #[arg(long, value_enum, value_name = "TIER")]
    pub tier: Option<TierArg>,

    /// List the whole catalog with the harnesses that see each skill.
    #[arg(long, default_value_t = false)]
    pub all: bool,
}

#[derive(Debug, Clone, Args)]
pub struct SearchArgs {
    /// Case-insensitive substring over name and description.
    #[arg(value_name = "QUERY")]
    pub query: String,

    /// Return names only.
    #[arg(long, default_value_t = false)]
    pub names: bool,
}

#[derive(Debug, Clone, Args)]
pub struct InfoArgs {
    #[arg(value_name = "NAME", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub name: String,
}

#[derive(Debug, Clone, Args)]
pub struct OnArgs {
    /// Skill names to activate.
    #[arg(value_name = "NAME", required = true)]
    pub names: Vec<String>,

    /// Tier to activate for; `lane` also writes `main`.
    #[arg(long, value_enum, value_name = "TIER", default_value_t = TierArg::Main)]
    pub tier: TierArg,

    /// Keep the row off every surface except this harness's own.
    #[arg(long = "only", value_name = "HARNESS")]
    pub only: Vec<String>,

    /// Exclude this harness from the row.
    #[arg(long = "not", value_name = "HARNESS")]
    pub not: Vec<String>,

    /// Mark the row `!mac`.
    #[arg(long, default_value_t = false)]
    pub no_mac: bool,

    /// Mark the row `!linux` (`--no-cell` is a legacy alias).
    #[arg(long = "no-linux", alias = "no-cell", default_value_t = false)]
    pub no_linux: bool,
}

#[derive(Debug, Clone, Args)]
pub struct OffArgs {
    /// Skill names to deactivate.
    #[arg(value_name = "NAME", required = true)]
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct AddArgs {
    /// Skill names or `@preset` names.
    #[arg(value_name = "NAME", required = true)]
    pub names: Vec<String>,

    /// Project root; defaults to the nearest git root above the cwd.
    #[arg(long, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct DropArgs {
    /// Skill names to deactivate in the project.
    #[arg(value_name = "NAME", required = true)]
    pub names: Vec<String>,

    /// Project root; defaults to the nearest git root above the cwd.
    #[arg(long, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct RenderArgs {
    /// Restrict the render to these harness surfaces; default is every row.
    #[arg(long = "surface", value_name = "HARNESS")]
    pub surface: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct StripArgs {
    /// Machine whose `<!-- NAME-only -->` blocks are kept.
    #[arg(long, value_name = "MACHINE", value_parser = clap::builder::PossibleValuesParser::new(crate::strip::MACHINE_NAMES))]
    pub keep: String,

    /// Harness whose blocks are kept; without it every harness block is dropped.
    #[arg(long, value_name = "HARNESS", value_parser = clap::builder::PossibleValuesParser::new(crate::strip::HARNESS_NAMES))]
    pub harness: Option<String>,

    /// Rewrite each file in place (a symlink is refused) instead of returning its text.
    #[arg(short = 'i', long = "in-place")]
    pub in_place: bool,

    /// The Markdown files to strip.
    #[arg(value_name = "FILE", required = true)]
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct ReconcileArgs {
    /// Discard retained stage arrivals without changing installed copies.
    #[arg(long = "discard", value_name = "NAME", conflicts_with_all = ["adopt", "keep", "restore"])]
    pub discard: Vec<String>,

    /// Take upstream for a locally edited entry.
    #[arg(long = "adopt", value_name = "NAME")]
    pub adopt: Vec<String>,

    /// Keep the house copy of a locally edited entry and stop tracking it.
    #[arg(long = "keep", value_name = "NAME")]
    pub keep: Vec<String>,

    /// Put back the copy an interrupted run displaced to `.npx/backup/<NAME>`
    /// (only when no live entry exists); nothing is restored without this.
    #[arg(long = "restore", value_name = "NAME")]
    pub restore: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct InstallArgs {
    /// Upstream source, as `npx skills add` takes it.
    #[arg(value_name = "SOURCE")]
    pub source: String,

    /// Install only this skill from the source.
    #[arg(long = "skill", value_name = "NAME")]
    pub skill: Vec<String>,

    /// Track a zip asset on SOURCE's GitHub releases instead of an npx
    /// source: SOURCE is `owner/repo`, this is the asset's file name, and one
    /// `--skill NAME` names the skill. `skillherd update` refreshes it from the
    /// newest stable release whose tag starts with `--tag-prefix`.
    #[arg(long = "release-asset", value_name = "ZIP", requires = "tag_prefix")]
    pub release_asset: Option<String>,

    /// With `--release-asset`: only releases whose tag starts with this prefix
    /// count (drafts and prereleases never do).
    #[arg(long = "tag-prefix", value_name = "PREFIX", requires = "release_asset")]
    pub tag_prefix: Option<String>,

    /// With `--release-asset`: the directory inside the zip that holds
    /// SKILL.md; defaults to the `--skill` name.
    #[arg(long = "release-dir", value_name = "DIR", requires = "release_asset")]
    pub release_dir: Option<String>,

    /// Take upstream for this name even though the pool copy differs (a
    /// local edit, or a copy with no import record).
    #[arg(long = "adopt", value_name = "NAME")]
    pub adopt: Vec<String>,

    /// Keep the pool copy of this name, declare it house, and stop tracking it.
    #[arg(long = "keep", value_name = "NAME")]
    pub keep: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct UpdateArgs {
    /// Skills to update; default is every tracked skill.
    #[arg(value_name = "NAME")]
    pub names: Vec<String>,
    /// Take upstream for this name even though the pool copy differs (a
    /// local edit, or a copy with no import record).
    #[arg(long = "adopt", value_name = "NAME")]
    pub adopt: Vec<String>,

    /// Keep the pool copy of this name, declare it house, and stop tracking it.
    #[arg(long = "keep", value_name = "NAME")]
    pub keep: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct RemoveArgs {
    /// Skills to remove; must be tracked in `.npx/<name>.json`.
    #[arg(value_name = "NAME", required = true)]
    pub names: Vec<String>,

    /// Remove even a locally edited entry or variant.
    #[arg(long, default_value_t = false)]
    pub force: bool,
}

#[derive(Debug, Clone, Args)]
pub struct SyncArgs {}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(argv: &[&str]) -> Cli {
        Cli::try_parse_from(argv).unwrap_or_else(|err| panic!("{argv:?}: {err}"))
    }

    #[test]
    fn the_command_surface_parses() {
        assert!(matches!(
            parse(&["skillherd", "schema"]).command,
            Command::Schema(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "doctor"]).command,
            Command::Doctor(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "list"]).command,
            Command::List(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "search", "foo"]).command,
            Command::Search(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "info", "foo"]).command,
            Command::Info(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "on", "foo"]).command,
            Command::On(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "off", "foo"]).command,
            Command::Off(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "add", "foo"]).command,
            Command::Add(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "drop", "foo"]).command,
            Command::Drop(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "render"]).command,
            Command::Render(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "reconcile"]).command,
            Command::Reconcile(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "install", "owner/repo"]).command,
            Command::Install(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "update"]).command,
            Command::Update(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "remove", "foo"]).command,
            Command::Remove(_)
        ));
        assert!(matches!(
            parse(&["skillherd", "sync"]).command,
            Command::Sync(_)
        ));
    }

    #[test]
    fn the_plan_flags_all_exist() {
        let cli = parse(&[
            "skillherd",
            "on",
            "foo",
            "--tier",
            "lane",
            "--only",
            "claude",
            "--not",
            "grok",
            "--no-mac",
            "--no-cell",
            "--dry-run",
            "--pretty",
        ]);
        match cli.command {
            Command::On(args) => {
                assert_eq!(args.tier, TierArg::Lane);
                assert_eq!(args.only, ["claude"]);
                assert_eq!(args.not, ["grok"]);
                assert!(args.no_mac && args.no_linux);
            }
            other => panic!("expected on, got {other:?}"),
        }
        assert!(cli.global.dry_run && cli.global.pretty);

        let cli = parse(&["skillherd", "reconcile", "--adopt", "a", "--keep", "b"]);
        match cli.command {
            Command::Reconcile(args) => {
                assert_eq!(args.adopt, ["a"]);
                assert_eq!(args.keep, ["b"]);
            }
            other => panic!("expected reconcile, got {other:?}"),
        }

        let cli = parse(&["skillherd", "doctor", "--fix", "--capabilities"]);
        match cli.command {
            Command::Doctor(args) => assert!(args.fix && args.capabilities),
            other => panic!("expected doctor, got {other:?}"),
        }

        let cli = parse(&["skillherd", "list", "--harness", "grok", "--all"]);
        match cli.command {
            Command::List(args) => {
                assert_eq!(args.harness.as_deref(), Some("grok"));
                assert!(args.all);
            }
            other => panic!("expected list, got {other:?}"),
        }

        let cli = parse(&["skillherd", "add", "foo", "--project", "/tmp/p"]);
        match cli.command {
            Command::Add(args) => assert_eq!(args.project.unwrap().to_str().unwrap(), "/tmp/p"),
            other => panic!("expected add, got {other:?}"),
        }

        let cli = parse(&[
            "skillherd",
            "render",
            "--surface",
            "claude",
            "--surface",
            "codex",
        ]);
        match cli.command {
            Command::Render(args) => assert_eq!(args.surface, ["claude", "codex"]),
            other => panic!("expected render, got {other:?}"),
        }

        let cli = parse(&["skillherd", "remove", "foo", "--force"]);
        match cli.command {
            Command::Remove(args) => assert!(args.force),
            other => panic!("expected remove, got {other:?}"),
        }

        let cli = parse(&[
            "skillherd",
            "install",
            "owner/repo",
            "--skill",
            "one",
            "--skill",
            "two",
        ]);
        match cli.command {
            Command::Install(args) => assert_eq!(args.skill, ["one", "two"]),
            other => panic!("expected install, got {other:?}"),
        }
    }

    #[test]
    fn tiers_and_schema_targets_reject_unknown_values() {
        let err = Cli::try_parse_from(["skillherd", "list", "--tier", "bogus"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
        let err = Cli::try_parse_from(["skillherd", "schema", "bogus"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
        let err = Cli::try_parse_from(["skillherd", "list", "--harness", "grok", "--tier", "main"])
            .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn required_positionals_fail_at_the_boundary() {
        for argv in [
            vec!["skillherd", "on"],
            vec!["skillherd", "off"],
            vec!["skillherd", "remove"],
            vec!["skillherd", "search"],
            vec!["skillherd", "info"],
            vec!["skillherd", "install"],
            vec!["skillherd"],
        ] {
            let err = Cli::try_parse_from(&argv).unwrap_err();
            assert!(
                matches!(
                    err.kind(),
                    clap::error::ErrorKind::MissingRequiredArgument
                        | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                ),
                "{argv:?}: {:?}",
                err.kind()
            );
        }
    }

    #[test]
    fn every_subcommand_documents_itself() {
        let mut command = Cli::command();
        command.build();
        for sub in command.get_subcommands() {
            assert!(
                !sub.get_about()
                    .map(|about| about.to_string())
                    .unwrap_or_default()
                    .is_empty(),
                "{}",
                sub.get_name()
            );
            assert!(
                !sub.get_arguments()
                    .filter(|arg| arg.get_long().is_some())
                    .any(|arg| arg.get_help().is_none()),
                "{} has an undocumented flag",
                sub.get_name()
            );
        }
    }
}
