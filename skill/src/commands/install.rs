//! `skillherd install` — Install an upstream skill through a staged npx.
//!
//! The whole point is that npx never sees the live HOME: it runs in a fresh
//! per-run stage under `Paths.stage`, writes `<stage>/.agents/`, and what it
//! leaves there is reconciled into the pool. The stage lock becomes the live
//! lock first, because reconcile reads the live lock for the source fields of
//! the import record. `update` runs the same staged flow with a different verb.
//!
//! `--release-asset` tracks a zip on the source's GitHub releases instead
//! (`release.rs`): the asset is fetched into the same stage, and reconcile
//! imports it with the record the fetch built in place of the lock's fields.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::cli::InstallArgs;
use crate::commands::reconcile::{self as reconcile_cmd, Data, Via};
use crate::error::{Error, Result};
use crate::lockfile::lock_library;
use crate::model::{NpxMeta, ReleaseSource};
use crate::names::validate_name;
use crate::npx::{self, NpxRun, Verb};
use crate::paths::Paths;
use crate::reconcile::{self, ReconcileOpts, ReconcileReport};
use crate::release::{self, ReleaseCheck, Target};

/// The reconcile report, plus what npx was actually asked to do and what the
/// release checks found.
#[derive(Debug, Serialize)]
struct NpxData {
    #[serde(flatten)]
    report: Data,
    #[serde(skip_serializing_if = "Option::is_none")]
    npx: Option<NpxRun>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    releases: Vec<ReleaseCheck>,
}

/// What the staged flow produced: the report, the npx run, the release
/// checks, and the refusal the command must report as an error once the
/// report is in hand.
pub struct Staged {
    pub report: ReconcileReport,
    pub npx: Option<NpxRun>,
    pub releases: Vec<ReleaseCheck>,
    pub refused: Option<Error>,
}

impl Staged {
    /// The command payload, or the refusal that outranks it.
    pub fn into_value(self) -> Result<Value> {
        let data = NpxData {
            report: Data {
                report: self.report,
            },
            npx: self.npx,
            releases: self.releases,
        };
        let value = serde_json::to_value(&data)
            .map_err(|err| Error::internal(format!("serializing the npx report: {err}")))?;
        match self.refused {
            Some(error) => Err(error),
            None => Ok(value),
        }
    }
}

/// How a staged run resolves arrivals that differ from the pool copy.
pub struct Resolve<'a> {
    pub adopt: &'a [String],
    pub keep: &'a [String],
    pub via: Via<'a>,
}

/// What a staged run fetches: an npx verb, GitHub release assets, or both.
pub struct Upstream {
    pub npx: Option<Verb>,
    pub releases: Vec<Target>,
}

/// Build the stage, fetch `upstream` into it, reconcile only its inbox,
/// and clear the stage unless reconcile refused something.
///
/// `upstream` is decided under the library lock, because it reads records.
pub fn staged(
    paths: &Paths,
    dry_run: bool,
    upstream: impl FnOnce() -> Result<Upstream>,
    resolve: Resolve<'_>,
) -> Result<Staged> {
    reconcile_cmd::check_resolutions(resolve.adopt, resolve.keep)?;
    // The shim check comes first: it is a configuration error, and nothing
    // should be built or run before it is reported.
    npx::assert_not_shim(paths)?;
    let guard = if dry_run {
        None
    } else {
        Some(lock_library(paths)?)
    };
    let upstream = upstream()?;

    let mut prefix = Vec::new();
    let mut stage = None;
    let mut checks = Vec::new();
    let mut fetched = BTreeMap::new();
    let npx_run = if dry_run {
        prefix.extend(
            upstream
                .releases
                .iter()
                .map(|target| release::plan_op(paths, target)),
        );
        prefix.extend(upstream.npx.iter().map(|verb| verb.plan_op(paths)));
        None
    } else {
        let built = npx::prepare_stage(paths)?;
        // Releases first: a GitHub failure stops the run before npx has
        // rewritten the live lock.
        (checks, fetched) = release::fetch_all(paths, &built, &upstream.releases, |key| {
            std::env::var(key).ok()
        })?;
        let run = match &upstream.npx {
            Some(verb) => {
                let run = npx::run(paths, verb)?;
                // npx's lock is now the live lock; reconcile reads it for
                // source fields.
                npx::copy_file_if_exists(&built.lock(), &paths.lock)?;
                Some(run)
            }
            None => None,
        };
        stage = Some(built);
        run
    };

    let opts = ReconcileOpts {
        dry_run,
        adopt: resolve.adopt.to_vec(),
        keep: resolve.keep.to_vec(),
        restore: Vec::new(),
        inbox: stage.iter().map(|stage| stage.inbox()).collect(),
        fetched,
    };
    let mut report = match guard {
        Some(guard) => reconcile::run_locked(paths, opts, guard)?,
        None => reconcile::run(paths, opts)?,
    };
    report.plan.splice(0..0, prefix);
    let refused = reconcile_cmd::refusal_error(&report, resolve.via);

    // The stage is scratch, but it is also the only copy of an arrival that
    // reconcile refused: leave it in place so the refusal can be resolved.
    if let Some(stage) = stage
        && refused.is_none()
    {
        stage.clear()?;
    }

    Ok(Staged {
        report,
        npx: npx_run,
        releases: checks,
        refused,
    })
}

/// Install `source`, reconcile the stage, and report both.
pub fn run(args: &InstallArgs, paths: &Paths, dry_run: bool) -> Result<Value> {
    for skill in &args.skill {
        validate_name(skill)?;
    }
    if args.source.trim().is_empty() {
        return Err(Error::usage(
            "install needs a source: `skillherd install owner/repo` or `skillherd install ~/path/to/skill`",
        ));
    }

    if let Some(asset) = &args.release_asset {
        return install_release(args, asset, paths, dry_run);
    }

    let staged = staged(
        paths,
        dry_run,
        || {
            Ok(Upstream {
                npx: Some(Verb::Add {
                    source: args.source.clone(),
                    skills: args.skill.clone(),
                }),
                releases: Vec::new(),
            })
        },
        Resolve {
            adopt: &args.adopt,
            keep: &args.keep,
            via: Via::Install {
                source: &args.source,
            },
        },
    )?;
    staged.into_value()
}

/// Register (or re-fetch) one skill tracked as a GitHub release asset.
fn install_release(args: &InstallArgs, asset: &str, paths: &Paths, dry_run: bool) -> Result<Value> {
    let [name] = args.skill.as_slice() else {
        return Err(Error::usage(
            "--release-asset installs exactly one skill: pass one `--skill NAME`",
        ));
    };
    let source = ReleaseSource {
        repo: args.source.trim().to_string(),
        asset: asset.to_string(),
        tag_prefix: args.tag_prefix.clone().unwrap_or_default(),
        dir: args.release_dir.clone().unwrap_or_else(|| name.clone()),
    };
    release::validate_source(&source)?;
    // A refusal's retry has to carry the release flags, or it would reach
    // for an npx source.
    let mut retry = format!(
        "{} --release-asset {} --tag-prefix {}",
        source.repo, source.asset, source.tag_prefix
    );
    if source.dir != *name {
        retry.push_str(&format!(" --release-dir {}", source.dir));
    }

    staged(
        paths,
        dry_run,
        || {
            Ok(Upstream {
                npx: None,
                releases: vec![Target {
                    name: name.clone(),
                    source: source.clone(),
                    previous: read_record(paths, name)?,
                    force: true,
                }],
            })
        },
        Resolve {
            adopt: &args.adopt,
            keep: &args.keep,
            via: Via::Install { source: &retry },
        },
    )?
    .into_value()
}

/// `.npx/<name>.json` as it stands, whatever its source type.
fn read_record(paths: &Paths, name: &str) -> Result<Option<NpxMeta>> {
    let path = paths.library.join(".npx").join(format!("{name}.json"));
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(serde_json::from_str(&text).ok()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::io(&format!("reading {}", path.display()), &err)),
    }
}
