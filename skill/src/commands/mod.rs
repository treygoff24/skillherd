//! Command dispatch: one `run` per command, one envelope out.

pub mod add;
pub mod doctor;
pub mod drop;
pub mod info;
pub mod install;
pub mod list;
pub mod npx;
pub mod off;
pub mod on;
pub mod reconcile;
pub mod remove;
pub mod render;
pub mod schema;
pub mod search;
pub mod strip;
pub mod sync;
pub mod update;

use serde_json::Value;

use crate::cli::{Cli, Command};
use crate::error::Result;
use crate::model::Envelope;
use crate::output;
use crate::paths::Paths;

/// Run the parsed command and wrap its payload in the success envelope.
pub fn dispatch(cli: Cli, paths: &Paths) -> Result<Envelope> {
    let dry_run = cli.global.dry_run;
    let data: Value = match cli.command {
        Command::Npx(_) => unreachable!("npx is dispatched before path resolution"),
        Command::Schema(args) => schema::run(&args, paths, dry_run)?,
        Command::Doctor(args) => doctor::run(&args, paths, dry_run)?,
        Command::List(args) => list::run(&args, paths, dry_run)?,
        Command::Search(args) => search::run(&args, paths, dry_run)?,
        Command::Info(args) => info::run(&args, paths, dry_run)?,
        Command::On(args) => on::run(&args, paths, dry_run)?,
        Command::Off(args) => off::run(&args, paths, dry_run)?,
        Command::Add(args) => add::run(&args, paths, dry_run)?,
        Command::Drop(args) => drop::run(&args, paths, dry_run)?,
        Command::Render(args) => render::run(&args, paths, dry_run)?,
        Command::Reconcile(args) => reconcile::run(&args, paths, dry_run)?,
        Command::Install(args) => install::run(&args, paths, dry_run)?,
        Command::Update(args) => update::run(&args, paths, dry_run)?,
        Command::Remove(args) => remove::run(&args, paths, dry_run)?,
        Command::Sync(args) => sync::run(&args, paths, dry_run)?,
        Command::Strip(args) => strip::run(&args, dry_run)?,
    };
    Ok(output::success_envelope(data, Some(paths)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use tempfile::TempDir;

    /// Every command is wired: dispatch either succeeds or fails with a
    /// reported error, never panics, and `schema` already answers.
    #[test]
    fn every_command_is_wired() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        let invocations: Vec<Vec<&str>> = vec![
            vec!["skillherd", "schema"],
            vec!["skillherd", "doctor"],
            vec!["skillherd", "list"],
            vec!["skillherd", "search", "foo"],
            vec!["skillherd", "info", "foo"],
            vec!["skillherd", "on", "foo"],
            vec!["skillherd", "off", "foo"],
            vec!["skillherd", "add", "foo"],
            vec!["skillherd", "drop", "foo"],
            vec!["skillherd", "render"],
            vec!["skillherd", "reconcile"],
            vec!["skillherd", "install", "owner/repo"],
            vec!["skillherd", "update"],
            vec!["skillherd", "remove", "foo"],
            vec!["skillherd", "sync"],
            vec!["skillherd", "strip", "--keep", "mac", "missing.md"],
        ];
        for argv in invocations {
            let cli = Cli::try_parse_from(&argv).unwrap();
            match dispatch(cli, &paths) {
                Ok(envelope) => assert!(envelope.ok, "{argv:?}"),
                Err(error) => assert!(!error.code().is_empty(), "{argv:?}"),
            }
        }
    }

    #[test]
    fn sync_refuses_a_dry_run() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        let cli = Cli::try_parse_from(["skillherd", "sync", "--dry-run"]).unwrap();
        let error = dispatch(cli, &paths).unwrap_err();
        assert_eq!(error.code(), "usage");
        assert_eq!(error.exit_code(), 2);
    }
}
