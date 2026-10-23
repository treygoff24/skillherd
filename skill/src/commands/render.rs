//! `skillherd render` — Rebuild the owned surfaces from pool plus policy.

use serde_json::Value;

use crate::cli::RenderArgs;
use crate::error::{Error, Result};
use crate::paths::Paths;
use crate::render::{self, RenderOpts};

pub fn run(args: &RenderArgs, paths: &Paths, dry_run: bool) -> Result<Value> {
    let report = render::run(
        paths,
        RenderOpts {
            surfaces: args.surface.clone(),
            dry_run,
            only: None,
        },
    )?;
    serde_json::to_value(&report)
        .map_err(|err| Error::internal(format!("serializing the render report: {err}")))
}
