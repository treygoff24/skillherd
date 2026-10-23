//! `skillherd` — one pool, one policy file, one renderer.
//!
//! Every skill lives in a configured folder or Git pool; one policy file
//! picks the always-on set per tier; every harness read surface is rendered
//! from pool plus policy, and `npx skills` runs in a staging home so an install
//! can never touch a live surface.
//!
//! Layering: `main` parses and reports, `cli` declares the surface, `commands`
//! implements it, and `paths`, `names`, `managed`, `lockfile`, `hash`, `policy`
//! and `model` are the shared ABI every lane builds on.

pub mod agents;
pub mod cli;
pub mod commands;
pub mod config;
pub mod doctor;
pub mod error;
pub mod fsx;
pub mod hash;
pub mod human;
pub mod library;
pub mod lockfile;
pub mod managed;
pub mod model;
pub mod names;
pub mod npx;
pub mod output;
pub mod paths;
pub mod policy;
pub mod project;
pub mod provenance;
pub mod reconcile;
pub mod release;
pub mod render;
pub mod strip;
pub mod surfaces;
pub mod variants;

pub use error::{Error, Result};
pub use paths::Paths;

/// Library and scratch Git operations choose their own repository. A caller
/// inside a Git hook must not redirect them into the hook's worktree or index.
pub(crate) fn git_command() -> std::process::Command {
    let mut command = std::process::Command::new("git");
    for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        command.env_remove(key);
    }
    command
}
