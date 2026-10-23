//! `skillherd strip` — Strip fenced blocks from Markdown files.
//!
//! The standalone face of `strip.rs`, for text that is not a skill (rule
//! files, `CLAUDE.md`, `AGENTS.md`): the same grammar `render` applies to
//! served copies. Every file is read and stripped before anything is
//! written, so one malformed file fails the run with no file rewritten.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::StripArgs;
use crate::error::{Error, Result};
use crate::model::Platform;
use crate::strip::strip_bytes;

/// What `skillherd strip` did.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StripReport {
    pub keep: Platform,
    pub harness: Option<String>,
    pub in_place: bool,
    pub files: Vec<StrippedFile>,
}

/// One file of a strip run.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StrippedFile {
    pub path: PathBuf,
    /// The file carried at least one marker line.
    pub had_markers: bool,
    /// With `--in-place`: the file was rewritten (or would be, on a dry run).
    pub rewritten: bool,
    /// Without `--in-place`: the stripped text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

struct Planned {
    path: PathBuf,
    had_markers: bool,
    original: Vec<u8>,
    stripped: Vec<u8>,
    meta: Option<fs::Metadata>,
}

pub fn run(args: &StripArgs, dry_run: bool) -> Result<Value> {
    let keep = match args.keep.as_str() {
        "mac" => Platform::Mac,
        "linux" | "cell" => Platform::Linux,
        other => return Err(Error::usage(format!("unknown --keep {other:?}"))),
    };
    let harness = args.harness.as_deref();

    let mut planned = Vec::with_capacity(args.files.len());
    let mut errors: Vec<Value> = Vec::new();
    for path in &args.files {
        let meta = if args.in_place {
            let meta = fs::symlink_metadata(path).map_err(|_| Error::input_not_found(path))?;
            if meta.file_type().is_symlink() {
                return Err(Error::usage(format!(
                    "refusing to rewrite {} in place: it is a symlink; strip the file it points at instead",
                    path.display()
                )));
            }
            if !meta.is_file() {
                return Err(Error::input_not_found_reason(path, "is not a regular file"));
            }
            Some(meta)
        } else {
            None
        };
        let original = fs::read(path).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => Error::input_not_found(path),
            _ => Error::io(&format!("reading {}", path.display()), &err),
        })?;
        match strip_bytes(&original, keep, harness) {
            Ok(result) => planned.push(Planned {
                path: path.clone(),
                had_markers: result.had_markers,
                stripped: result.bytes,
                original,
                meta,
            }),
            Err(error) => errors.push(json!({
                "path": path.display().to_string(),
                "message": format!("{}: {error}", path.display()),
            })),
        }
    }
    if !errors.is_empty() {
        return Err(Error::RenderFailed {
            message: format!(
                "{} file{} could not be stripped; nothing was written",
                errors.len(),
                if errors.len() == 1 { "" } else { "s" }
            ),
            details: json!({ "errors": errors }),
            suggested_fix: Some(
                "fix the reported marker lines, then rerun `skillherd strip`".into(),
            ),
        });
    }

    let mut files = Vec::with_capacity(planned.len());
    for file in planned {
        let changed = file.stripped != file.original;
        let content = if args.in_place {
            None
        } else {
            Some(String::from_utf8(file.stripped.clone()).map_err(|_| {
                Error::usage(format!(
                    "{} is not UTF-8 text; use --in-place to rewrite it",
                    file.path.display()
                ))
            })?)
        };
        if args.in_place && changed && !dry_run {
            let meta = file.meta.as_ref().ok_or_else(|| {
                Error::internal(format!("no metadata for {}", file.path.display()))
            })?;
            rewrite(&file.path, meta, &file.stripped)?;
        }
        files.push(StrippedFile {
            path: file.path,
            had_markers: file.had_markers,
            rewritten: args.in_place && changed,
            content,
        });
    }

    serde_json::to_value(StripReport {
        keep,
        harness: harness.map(str::to_string),
        in_place: args.in_place,
        files,
    })
    .map_err(|err| Error::internal(format!("serializing the strip report: {err}")))
}

/// Replace `path` with `bytes`, keeping its mode: write a fresh temporary file
/// beside it (`O_EXCL | O_NOFOLLOW`), re-check that `path` is still the regular
/// file that was read, then rename over it. A rename replaces the directory
/// entry and never writes through a link, so a path swapped for a symlink in
/// between is not followed; the re-check refuses that case outright.
fn rewrite(path: &Path, read: &fs::Metadata, bytes: &[u8]) -> Result<()> {
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut temp = None;
    for attempt in 0..1000u32 {
        let candidate = dir.join(format!(
            ".{name}.skill-strip-tmp-{}-{attempt}",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => {
                temp = Some((candidate, file));
                break;
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(Error::io(
                    &format!("creating {}", candidate.display()),
                    &err,
                ));
            }
        }
    }
    let (temp, mut file) = temp.ok_or_else(|| {
        Error::internal(format!("no free temporary name beside {}", path.display()))
    })?;
    let written = file
        .write_all(bytes)
        .and_then(|()| {
            file.set_permissions(fs::Permissions::from_mode(
                read.permissions().mode() & 0o7777,
            ))
        })
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(err) = written {
        let _ = fs::remove_file(&temp);
        return Err(Error::io(&format!("writing {}", temp.display()), &err));
    }
    let still = fs::symlink_metadata(path).ok();
    let same = still
        .as_ref()
        .is_some_and(|now| now.is_file() && now.dev() == read.dev() && now.ino() == read.ino());
    if !same {
        let _ = fs::remove_file(&temp);
        return Err(Error::usage(format!(
            "{} changed while it was being stripped; nothing was written to it",
            path.display()
        )));
    }
    fs::rename(&temp, path).map_err(|err| {
        let _ = fs::remove_file(&temp);
        Error::io(&format!("replacing {}", path.display()), &err)
    })
}
