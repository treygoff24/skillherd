//! GitHub release assets as an upstream.
//!
//! Some upstreams ship their skill as a zip attached to each release rather
//! than as an `npx skills` repository (`composio-cli`: `composio-skill.zip` on
//! every stable `@composio/cli@<version>` release of `ComposioHQ/composio`).
//! A `github-release` import record ([`ReleaseSource`]) names the repo, the
//! asset, the tag prefix and the directory inside the zip; its `revision` is
//! the tag last imported.
//!
//! This module only fetches. The newest stable release whose tag starts with
//! the prefix is found, its asset is downloaded into the run's stage, unzipped
//! there, and the skill directory is placed in the stage inbox. `reconcile`
//! imports it like any other arrival — local-edit refusal, `--adopt`/`--keep`,
//! the `npx: import <name> (<hash8>)` commit, render — with the record this
//! module built ([`record_for`]) standing in for the npx lock's source fields.
//!
//! GitHub access is always authenticated to avoid the anonymous API rate limit.
//! The fetch goes
//! through `gh` (`Paths.gh`) when present, otherwise `curl` with a token from
//! `GH_TOKEN`/`GITHUB_TOKEN` passed on stdin, never in argv; with neither it is
//! `github_auth_failed`. Both write only into the stage.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::fsx::rename_noreplace;
use crate::model::{NpxMeta, PlanOp, ReleaseSource, SOURCE_GITHUB_RELEASE};
use crate::names::validate_name;
use crate::npx::Stage;
use crate::paths::Paths;

/// Releases per listing page (GitHub's maximum).
const PER_PAGE: usize = 100;

/// Listing pages read before giving up on a prefix. Multi-package repositories
/// can publish hundreds of releases, with the desired package beyond page one.
const MAX_PAGES: usize = 30;

/// Unzip limits: a skill is a handful of Markdown files.
const MAX_ENTRIES: usize = 10_000;
const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;

/// The work directory inside a stage, beside its `.agents/`.
const WORK_DIR: &str = "release";

/// One release as the GitHub API lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// One release asset.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub id: u64,
    pub name: String,
}

/// What one tracked release came to in this run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseCheck {
    pub name: String,
    pub repo: String,
    /// The newest stable tag with the prefix.
    pub tag: String,
    /// The tag the record named before this run; empty for a new record.
    pub previous: String,
    /// Whether the asset was fetched into the stage inbox. `false` means the
    /// record already names the newest tag.
    pub fetched: bool,
}

/// One release-tracked skill a staged run looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub source: ReleaseSource,
    /// The record as it stands, if any: its `revision` is the tag last imported.
    pub previous: Option<NpxMeta>,
    /// Fetch even when the record already names the newest tag (`install`).
    pub force: bool,
}

// ---- validation ------------------------------------------------------

/// One `owner` or `repo` component as GitHub allows it.
fn is_repo_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// A relative path of plain components, as the zip's skill directory.
fn is_plain_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.contains('\0')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Refuse a source whose fields could steer a request or a path elsewhere.
pub fn validate_source(source: &ReleaseSource) -> Result<()> {
    let mut parts = source.repo.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(Error::usage(format!(
            "release repo {:?} is not `owner/repo`",
            source.repo
        )));
    };
    if !is_repo_part(owner) || !is_repo_part(repo) {
        return Err(Error::usage(format!(
            "release repo {:?} is not `owner/repo`",
            source.repo
        )));
    }
    if source.asset.is_empty()
        || source.asset.contains('/')
        || source.asset.contains('\\')
        || source.asset.starts_with('.')
    {
        return Err(Error::usage(format!(
            "release asset {:?} must be a plain file name",
            source.asset
        )));
    }
    if source.tag_prefix.trim().is_empty() {
        return Err(Error::usage(
            "--tag-prefix must not be empty: it is what keeps other packages' releases out",
        ));
    }
    if !is_plain_relative(&source.dir) {
        return Err(Error::usage(format!(
            "release dir {:?} must be a relative path inside the zip",
            source.dir
        )));
    }
    Ok(())
}

// ---- records ---------------------------------------------------------

/// The pool's `github-release` records, by name. Read under the library lock.
pub fn tracked(paths: &Paths) -> Result<BTreeMap<String, NpxMeta>> {
    let dir = paths.library.join(".npx");
    let read = match fs::read_dir(&dir) {
        Ok(read) => read,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(err) => return Err(Error::io(&format!("reading {}", dir.display()), &err)),
    };
    let mut records = BTreeMap::new();
    for dirent in read {
        let dirent = dirent.map_err(|err| Error::io("reading an import record", &err))?;
        let file = dirent.file_name().to_string_lossy().into_owned();
        let Some(stem) = file.strip_suffix(".json") else {
            continue;
        };
        if stem.starts_with("skill-lock") || validate_name(stem).is_err() {
            continue;
        }
        // An unreadable record is reconcile's to report; it is not a release.
        let Ok(text) = fs::read_to_string(dirent.path()) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<NpxMeta>(&text) else {
            continue;
        };
        if record.source_type != SOURCE_GITHUB_RELEASE {
            continue;
        }
        match &record.release {
            Some(source) => validate_source(source).map_err(|err| {
                Error::config_error(format!(".npx/{file} has an invalid release block: {err}"))
            })?,
            None => {
                return Err(Error::config_error(format!(
                    ".npx/{file} says sourceType {SOURCE_GITHUB_RELEASE} but carries no release block; re-register it with `skillherd install <owner/repo> --release-asset … --tag-prefix … --skill {stem} --adopt {stem}`"
                )));
            }
        }
        records.insert(stem.to_string(), record);
    }
    Ok(records)
}

/// The import record for `tag` of `source`: the source fields reconcile
/// writes in place of the npx lock's. Variant and agent hashes carry over from
/// the previous record, so their edit guards keep working.
pub fn record_for(source: &ReleaseSource, tag: &str, previous: Option<&NpxMeta>) -> NpxMeta {
    NpxMeta {
        source: source.repo.clone(),
        source_type: SOURCE_GITHUB_RELEASE.to_string(),
        source_url: format!("https://github.com/{}", source.repo),
        r#ref: None,
        skill_path: format!("{}/SKILL.md", source.dir),
        skill_folder_hash: String::new(),
        imported_hash: String::new(),
        imported_at: previous
            .map(|record| record.imported_at.clone())
            .unwrap_or_default(),
        revision: tag.to_string(),
        variants: previous
            .map(|record| record.variants.clone())
            .unwrap_or_default(),
        variants_pending: false,
        harness_agents: previous
            .map(|record| record.harness_agents.clone())
            .unwrap_or_default(),
        release: Some(source.clone()),
    }
}

/// How a `--dry-run` reports the fetch it did not make.
pub fn plan_op(paths: &Paths, target: &Target) -> PlanOp {
    PlanOp {
        op: "github-release".to_string(),
        path: paths.stage.clone(),
        target: Some(PathBuf::from(format!(
            "{} {}* {}",
            target.source.repo, target.source.tag_prefix, target.source.asset
        ))),
    }
}

// ---- selection -------------------------------------------------------

/// The dotted numeric version after the prefix, when it is one.
fn version_key(version: &str) -> Option<Vec<u64>> {
    version
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect()
}

/// The newest non-draft, non-prerelease release whose tag starts with
/// `prefix`. Newest is the highest dotted version after the prefix; the
/// publication stamp breaks ties and orders tags that are not versions (a
/// version outranks a non-version). The listing's own order is not trusted:
/// GitHub lists `@composio/cli@0.4.1` below a later-created beta.
pub fn pick_latest<'a>(releases: &'a [Release], prefix: &str) -> Option<&'a Release> {
    releases
        .iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter(|release| release.tag_name.starts_with(prefix))
        .max_by(|a, b| {
            let key = |release: &Release| {
                (
                    version_key(&release.tag_name[prefix.len()..]),
                    release.published_at.clone().unwrap_or_default(),
                )
            };
            key(a).cmp(&key(b))
        })
}

// ---- the fetch seam --------------------------------------------------

/// How this run reaches GitHub. Never anonymous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// The GitHub CLI, which carries its own login (or `GH_TOKEN`).
    Gh(PathBuf),
    /// `curl` with a token read from the environment and passed on stdin.
    Token(String),
}

impl Backend {
    /// `gh` when `Paths.gh` names one; else a token from `lookup`
    /// (`GH_TOKEN`, then `GITHUB_TOKEN`); else `github_auth_failed`.
    pub fn resolve(
        paths: &Paths,
        repo: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Backend> {
        if let Some(gh) = &paths.gh {
            return Ok(Backend::Gh(gh.clone()));
        }
        for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
            if let Some(token) = lookup(key).filter(|token| !token.trim().is_empty()) {
                return Ok(Backend::Token(token.trim().to_string()));
            }
        }
        Err(Error::github_auth_failed(
            repo,
            "no `gh` on PATH (or SKILLHERD_GH) and neither GH_TOKEN nor GITHUB_TOKEN is set; release fetches never go out anonymously",
        ))
    }

    /// GET one API path (`repos/o/r/…`) as JSON.
    fn json(&self, repo: &str, api: &str, cwd: &Path) -> Result<Value> {
        let body = self.get(repo, api, cwd, false, None)?;
        serde_json::from_slice(&body).map_err(|err| {
            Error::release_failed(
                repo,
                &format!("GitHub's answer for {api} is not JSON: {err}"),
            )
        })
    }

    /// GET one API path as bytes into `dest`.
    fn download(&self, repo: &str, api: &str, cwd: &Path, dest: &Path) -> Result<()> {
        self.get(repo, api, cwd, true, Some(dest)).map(|_| ())
    }

    /// One request. With `dest`, the body goes there and nothing is returned.
    fn get(
        &self,
        repo: &str,
        api: &str,
        cwd: &Path,
        binary: bool,
        dest: Option<&Path>,
    ) -> Result<Vec<u8>> {
        match self {
            Backend::Gh(bin) => gh_get(bin, repo, api, cwd, binary, dest),
            Backend::Token(token) => curl_get(token, repo, api, cwd, binary, dest),
        }
    }
}

/// The `gh api` argv for one request (without the binary).
pub fn gh_argv(api: &str, binary: bool) -> Vec<String> {
    let mut argv = vec!["api".to_string()];
    if binary {
        argv.push("-H".to_string());
        argv.push("Accept: application/octet-stream".to_string());
    }
    argv.push(api.to_string());
    argv
}

fn gh_get(
    bin: &Path,
    repo: &str,
    api: &str,
    cwd: &Path,
    binary: bool,
    dest: Option<&Path>,
) -> Result<Vec<u8>> {
    let mut command = Command::new(bin);
    command
        .args(gh_argv(api, binary))
        .current_dir(cwd)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::piped());
    match dest {
        Some(dest) => {
            let file = fs::File::create(dest)
                .map_err(|err| Error::io(&format!("creating {}", dest.display()), &err))?;
            command.stdout(Stdio::from(file));
        }
        None => {
            command.stdout(Stdio::piped());
        }
    }
    let output = command.output().map_err(|err| {
        Error::release_failed(repo, &format!("failed to run {}: {err}", bin.display()))
    })?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(classify_gh_failure(
        repo,
        api,
        output.status.code().unwrap_or(128),
        &stderr,
    ))
}

/// `gh` exits 4 when it has no login; the API's own refusals show up as
/// `HTTP <code>` in its stderr.
pub fn classify_gh_failure(repo: &str, api: &str, exit: i32, stderr: &str) -> Error {
    let reason = crate::npx::redact(&format!("`gh api {api}` exited {exit}: {}", stderr.trim()));
    let auth = exit == 4
        || [
            "HTTP 401",
            "HTTP 403",
            "HTTP 429",
            "gh auth login",
            "rate limit",
        ]
        .iter()
        .any(|needle| stderr.contains(needle));
    if auth {
        return Error::github_auth_failed(repo, &reason);
    }
    if stderr.contains("HTTP 404") {
        return Error::release_not_found(repo, &reason);
    }
    Error::release_failed(repo, &reason)
}

/// The `curl` argv for one request: the URL and the token go on stdin as a
/// config file, so neither appears in the process table.
pub fn curl_argv(dest: Option<&Path>) -> Vec<String> {
    let mut argv = vec![
        "--silent".to_string(),
        "--show-error".to_string(),
        "--location".to_string(),
        "--config".to_string(),
        "-".to_string(),
        "--write-out".to_string(),
        "\n%{http_code}".to_string(),
    ];
    match dest {
        Some(dest) => {
            argv.push("--output".to_string());
            argv.push(dest.display().to_string());
        }
        None => {
            argv.push("--output".to_string());
            argv.push("-".to_string());
        }
    }
    argv
}

/// The config `curl` reads on stdin.
pub fn curl_config(token: &str, api: &str, binary: bool) -> String {
    let accept = if binary {
        "application/octet-stream"
    } else {
        "application/vnd.github+json"
    };
    format!(
        "url = \"https://api.github.com/{api}\"\nheader = \"Authorization: Bearer {token}\"\nheader = \"Accept: {accept}\"\nheader = \"X-GitHub-Api-Version: 2022-11-28\"\nheader = \"User-Agent: skillherd\"\n"
    )
}

fn curl_get(
    token: &str,
    repo: &str,
    api: &str,
    cwd: &Path,
    binary: bool,
    dest: Option<&Path>,
) -> Result<Vec<u8>> {
    let mut child = Command::new("curl")
        .args(curl_argv(dest))
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| Error::release_failed(repo, &format!("failed to run curl: {err}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(curl_config(token, api, binary).as_bytes())
            .map_err(|err| Error::release_failed(repo, &format!("writing curl's config: {err}")))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|err| Error::release_failed(repo, &format!("waiting for curl: {err}")))?;
    if !output.status.success() {
        return Err(Error::release_failed(
            repo,
            &format!(
                "curl exited {}: {}",
                output.status.code().unwrap_or(128),
                crate::npx::redact(&String::from_utf8_lossy(&output.stderr)).trim()
            ),
        ));
    }
    // The status code is the last line of stdout (`--write-out`).
    let mut body = output.stdout;
    let split = body.iter().rposition(|byte| *byte == b'\n').unwrap_or(0);
    let status: u16 = String::from_utf8_lossy(&body[split..])
        .trim()
        .parse()
        .unwrap_or(0);
    body.truncate(split);
    match status {
        200..=299 => Ok(body),
        401 | 403 | 429 => Err(Error::github_auth_failed(
            repo,
            &format!(
                "GitHub answered HTTP {status} for {api}: {}",
                crate::npx::redact(&String::from_utf8_lossy(&body)).trim()
            ),
        )),
        404 => Err(Error::release_not_found(
            repo,
            &format!("GitHub answered HTTP 404 for {api}"),
        )),
        _ => Err(Error::release_failed(
            repo,
            &format!("GitHub answered HTTP {status} for {api}"),
        )),
    }
}

/// Walk the release listing until the prefix's newest stable release is
/// certain: the page that first holds a candidate, plus one more page, since
/// the listing is not strictly newest-first.
pub fn latest(backend: &Backend, source: &ReleaseSource, cwd: &Path) -> Result<Release> {
    let mut seen: Vec<Release> = Vec::new();
    let mut found_on: Option<usize> = None;
    for page in 1..=MAX_PAGES {
        let api = format!(
            "repos/{}/releases?per_page={PER_PAGE}&page={page}",
            source.repo
        );
        let value = backend.json(&source.repo, &api, cwd)?;
        let releases: Vec<Release> = serde_json::from_value(value).map_err(|err| {
            Error::release_failed(&source.repo, &format!("{api} is not a release list: {err}"))
        })?;
        let count = releases.len();
        seen.extend(releases);
        if found_on.is_none() && pick_latest(&seen, &source.tag_prefix).is_some() {
            found_on = Some(page);
        }
        if count < PER_PAGE || found_on.is_some_and(|first| page > first) {
            break;
        }
    }
    pick_latest(&seen, &source.tag_prefix)
        .cloned()
        .ok_or_else(|| {
            Error::release_not_found(
                &source.repo,
                &format!(
                    "no stable (non-draft, non-prerelease) release has a tag starting with {:?} in the {} releases read",
                    source.tag_prefix,
                    seen.len()
                ),
            )
        })
}

// ---- the archive -----------------------------------------------------

/// Unpack `zip` into `dest`, refusing the whole archive before writing
/// anything when any entry is absolute, climbs with `..`, is a symlink or
/// any other non-plain type, or the archive is implausibly large.
pub fn extract(zip: &Path, dest: &Path, asset: &str) -> Result<()> {
    let file = fs::File::open(zip)
        .map_err(|err| Error::io(&format!("opening {}", zip.display()), &err))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|err| {
        Error::release_archive_rejected(asset, "", &format!("not a readable zip: {err}"))
    })?;
    if archive.len() > MAX_ENTRIES {
        return Err(Error::release_archive_rejected(
            asset,
            "",
            &format!("{} entries (limit {MAX_ENTRIES})", archive.len()),
        ));
    }

    // Preflight every entry before the first write.
    let mut entries: Vec<(usize, PathBuf, bool, bool)> = Vec::with_capacity(archive.len());
    let mut total: u64 = 0;
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(|err| {
            Error::release_archive_rejected(asset, "", &format!("entry {index}: {err}"))
        })?;
        let name = entry
            .name()
            .map_err(|err| {
                Error::release_archive_rejected(asset, "", &format!("entry {index}: name: {err}"))
            })?
            .to_string();
        let rel = entry_path(&name)
            .map_err(|reason| Error::release_archive_rejected(asset, &name, reason))?;
        let kind = entry.unix_mode().map(|mode| mode & 0o170_000).unwrap_or(0);
        let is_dir = entry.is_dir();
        match kind {
            0 => {}
            0o040_000 if is_dir => {}
            0o100_000 if !is_dir => {}
            0o120_000 => {
                return Err(Error::release_archive_rejected(asset, &name, "a symlink"));
            }
            _ => {
                return Err(Error::release_archive_rejected(
                    asset,
                    &name,
                    "not a plain file or directory",
                ));
            }
        }
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES {
            return Err(Error::release_archive_rejected(
                asset,
                &name,
                &format!("unpacks past {MAX_UNPACKED_BYTES} bytes"),
            ));
        }
        let executable = entry.unix_mode().is_some_and(|mode| mode & 0o111 != 0);
        entries.push((index, rel, is_dir, executable));
    }

    fs::create_dir_all(dest)
        .map_err(|err| Error::io(&format!("creating {}", dest.display()), &err))?;
    for (index, rel, is_dir, executable) in entries {
        let target = dest.join(&rel);
        if is_dir {
            fs::create_dir_all(&target)
                .map_err(|err| Error::io(&format!("creating {}", target.display()), &err))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| Error::io(&format!("creating {}", parent.display()), &err))?;
        }
        let mut entry = archive.by_index(index).map_err(|err| {
            Error::release_archive_rejected(asset, &rel.display().to_string(), &err.to_string())
        })?;
        // `create_new`: a second entry with the same name is refused, and no
        // write ever follows a path something else put there.
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|err| {
                Error::release_archive_rejected(
                    asset,
                    &rel.display().to_string(),
                    &format!("cannot create it: {err}"),
                )
            })?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|err| Error::io(&format!("unpacking {}", target.display()), &err))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if executable { 0o755 } else { 0o644 };
            fs::set_permissions(&target, fs::Permissions::from_mode(mode))
                .map_err(|err| Error::io(&format!("chmod {}", target.display()), &err))?;
        }
        #[cfg(not(unix))]
        let _ = executable;
    }
    Ok(())
}

/// An entry name as a relative path of plain components, or why not.
fn entry_path(name: &str) -> std::result::Result<PathBuf, &'static str> {
    if name.is_empty() {
        return Err("an empty name");
    }
    if name.contains('\0') {
        return Err("a NUL byte in the name");
    }
    if name.contains('\\') {
        return Err("a backslash in the name");
    }
    if name.starts_with('/') {
        return Err("an absolute path");
    }
    let mut rel = PathBuf::new();
    for part in name.trim_end_matches('/').split('/') {
        match part {
            "" => return Err("an empty path component"),
            "." => return Err("a `.` component"),
            ".." => return Err("a `..` component"),
            part if part.len() >= 2 && part.as_bytes()[1] == b':' => {
                return Err("a drive-letter path");
            }
            part => rel.push(part),
        }
    }
    Ok(rel)
}

// ---- the staged run --------------------------------------------------

/// Check every target, fetch the ones whose newest stable tag differs from
/// the record (or all, for `force`), and place each skill directory in the
/// stage inbox under its name. Returns what was checked and the records
/// reconcile writes for the fetched names.
///
/// Runs before npx in the same stage, so a GitHub failure stops the run while
/// the live lock is still untouched.
pub fn fetch_all(
    paths: &Paths,
    stage: &Stage,
    targets: &[Target],
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<(Vec<ReleaseCheck>, BTreeMap<String, NpxMeta>)> {
    let mut checks = Vec::new();
    let mut fetched = BTreeMap::new();
    for target in targets {
        let source = &target.source;
        let backend = Backend::resolve(paths, &source.repo, &lookup)?;
        let work = stage.home().join(WORK_DIR).join(&target.name);
        fs::create_dir_all(&work)
            .map_err(|err| Error::io(&format!("creating {}", work.display()), &err))?;
        let release = latest(&backend, source, &work)?;
        let previous = target
            .previous
            .as_ref()
            .map(|record| record.revision.clone())
            .unwrap_or_default();
        let current = !target.force && previous == release.tag_name;
        checks.push(ReleaseCheck {
            name: target.name.clone(),
            repo: source.repo.clone(),
            tag: release.tag_name.clone(),
            previous,
            fetched: !current,
        });
        if current {
            continue;
        }

        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == source.asset)
            .ok_or_else(|| {
                Error::release_not_found(
                    &source.repo,
                    &format!(
                        "release {} carries no asset named {}",
                        release.tag_name, source.asset
                    ),
                )
            })?;
        let zip = work.join(&source.asset);
        backend.download(
            &source.repo,
            &format!("repos/{}/releases/assets/{}", source.repo, asset.id),
            &work,
            &zip,
        )?;
        let unpacked = work.join("unpacked");
        extract(&zip, &unpacked, &source.asset)?;
        let skill = unpacked.join(&source.dir);
        let is_dir = fs::symlink_metadata(&skill).is_ok_and(|meta| meta.is_dir());
        if !is_dir || !skill.join("SKILL.md").is_file() {
            return Err(Error::release_not_found(
                &source.repo,
                &format!(
                    "{} of {} has no {}/SKILL.md",
                    source.asset, release.tag_name, source.dir
                ),
            ));
        }
        let arrival = stage.inbox().join(&target.name);
        rename_noreplace(&skill, &arrival).map_err(|err| {
            Error::io(&format!("placing {} in the stage inbox", target.name), &err)
        })?;
        fetched.insert(
            target.name.clone(),
            record_for(source, &release.tag_name, target.previous.as_ref()),
        );
    }
    Ok((checks, fetched))
}

#[cfg(test)]
mod tests {
    #[test]
    fn release_failure_diagnostics_redact_url_credentials() {
        let error = super::classify_gh_failure(
            "o/r",
            "/repos/o/r/releases",
            1,
            "HTTP 403 https://user:password@example.invalid",
        );
        assert!(!error.to_string().contains("user:password"), "{error}");
    }

    use super::*;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;

    fn release(tag: &str, prerelease: bool, draft: bool, published: &str) -> Release {
        Release {
            tag_name: tag.to_string(),
            draft,
            prerelease,
            published_at: Some(published.to_string()),
            assets: vec![Asset {
                id: 1,
                name: "composio-skill.zip".to_string(),
            }],
        }
    }

    fn source() -> ReleaseSource {
        ReleaseSource {
            repo: "ComposioHQ/composio".into(),
            asset: "composio-skill.zip".into(),
            tag_prefix: "@composio/cli@".into(),
            dir: "composio-cli".into(),
        }
    }

    /// A multi-package listing: betas above the stable
    /// release, other packages interleaved, and a later-created beta of the
    /// next version listed below the stable one.
    #[test]
    fn latest_stable_skips_prereleases_drafts_and_other_prefixes() {
        let releases = vec![
            release(
                "@composio/core@0.21.0",
                false,
                false,
                "2026-09-24T12:07:36Z",
            ),
            release(
                "@composio/cli@0.4.2-beta.402",
                true,
                false,
                "2026-09-23T16:09:11Z",
            ),
            release("@composio/cli@0.5.0", false, true, "2026-09-23T17:00:00Z"),
            release("@composio/cli@0.6.0", true, false, "2026-09-23T18:00:00Z"),
            release("@composio/cli@0.4.1", false, false, "2026-09-04T18:38:20Z"),
            release(
                "@composio/cli@0.4.2-beta.379",
                true,
                false,
                "2026-09-04T22:46:07Z",
            ),
            release("@composio/cli@0.4.0", false, false, "2026-08-23T22:55:41Z"),
            release(
                "@composio/cli-extra@9.9.9",
                false,
                false,
                "2026-09-25T00:00:00Z",
            ),
            release("@composio/cxx@9.9.9", false, false, "2026-09-25T00:00:00Z"),
        ];
        // Neither `@composio/cli-extra@` nor `@composio/cxx@` starts with
        // `@composio/cli@` (the second one's version sits where the prefix's
        // would, so only the prefix filter keeps it out).
        let picked = pick_latest(&releases, "@composio/cli@").unwrap();
        assert_eq!(picked.tag_name, "@composio/cli@0.4.1");
    }

    #[test]
    fn latest_stable_orders_by_version_not_listing_or_publication() {
        // A backport published after the newer release does not win.
        let releases = vec![
            release("p@0.3.9", false, false, "2026-09-10T00:00:00Z"),
            release("p@0.10.0", false, false, "2026-09-01T00:00:00Z"),
            release("p@0.9.0", false, false, "2026-08-01T00:00:00Z"),
        ];
        assert_eq!(pick_latest(&releases, "p@").unwrap().tag_name, "p@0.10.0");
        assert!(pick_latest(&releases, "q@").is_none());
        let betas = vec![release(
            "p@1.0.0-beta.1",
            true,
            false,
            "2026-09-01T00:00:00Z",
        )];
        assert!(pick_latest(&betas, "p@").is_none());
    }

    #[test]
    fn sources_that_could_steer_a_request_or_a_path_are_refused() {
        assert!(validate_source(&source()).is_ok());
        for (repo, asset, prefix, dir) in [
            ("ComposioHQ", "a.zip", "p@", "d"),
            ("a/b/c", "a.zip", "p@", "d"),
            ("../x", "a.zip", "p@", "d"),
            ("o/r?x=1", "a.zip", "p@", "d"),
            ("o/r", "../a.zip", "p@", "d"),
            ("o/r", "a.zip", "", "d"),
            ("o/r", "a.zip", "p@", "../d"),
            ("o/r", "a.zip", "p@", "/d"),
        ] {
            let source = ReleaseSource {
                repo: repo.into(),
                asset: asset.into(),
                tag_prefix: prefix.into(),
                dir: dir.into(),
            };
            assert_eq!(
                validate_source(&source).unwrap_err().code(),
                "usage",
                "{repo} {asset} {prefix:?} {dir}"
            );
        }
    }

    #[test]
    fn no_gh_and_no_token_is_a_typed_auth_failure() {
        let dir = TempDir::new().unwrap();
        let mut paths = Paths::fixture(dir.path());
        paths.gh = None;
        let error = Backend::resolve(&paths, "o/r", |_| None).unwrap_err();
        assert_eq!(error.code(), "github_auth_failed");
        assert_eq!(error.exit_code(), 77);

        let backend = Backend::resolve(&paths, "o/r", |key| {
            (key == "GITHUB_TOKEN").then(|| "t0k3n".to_string())
        })
        .unwrap();
        assert_eq!(backend, Backend::Token("t0k3n".into()));
        paths.gh = Some(PathBuf::from("/opt/gh"));
        assert_eq!(
            Backend::resolve(&paths, "o/r", |_| Some("t".into())).unwrap(),
            Backend::Gh(PathBuf::from("/opt/gh"))
        );
    }

    #[test]
    fn gh_refusals_are_auth_failures_and_other_failures_are_not() {
        let api = "repos/o/r/releases";
        for (exit, stderr) in [
            (1, "gh: API rate limit exceeded for 1.2.3.4. (HTTP 403)"),
            (1, "gh: Bad credentials (HTTP 401)"),
            (
                4,
                "To get started with GitHub CLI, please run:  gh auth login",
            ),
        ] {
            assert_eq!(
                classify_gh_failure("o/r", api, exit, stderr).code(),
                "github_auth_failed",
                "{stderr}"
            );
        }
        assert_eq!(
            classify_gh_failure("o/r", api, 1, "gh: Not Found (HTTP 404)").code(),
            "input_not_found"
        );
        let other = classify_gh_failure("o/r", api, 1, "dial tcp: i/o timeout");
        assert_eq!(other.code(), "release_failed");
        assert!(other.retryable());
    }

    #[test]
    fn the_token_never_reaches_argv() {
        let argv = curl_argv(Some(Path::new("/stage/a.zip")));
        assert!(argv.iter().all(|arg| !arg.contains("s3cr3t")), "{argv:?}");
        assert!(argv.windows(2).any(|pair| pair == ["--config", "-"]));
        let config = curl_config("s3cr3t", "repos/o/r/releases", false);
        assert!(config.contains("Authorization: Bearer s3cr3t"));
        assert!(config.contains("url = \"https://api.github.com/repos/o/r/releases\""));
    }

    fn write_zip(path: &Path, build: impl FnOnce(&mut zip::ZipWriter<fs::File>)) {
        let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
        build(&mut writer);
        writer.finish().unwrap();
    }

    fn opts() -> SimpleFileOptions {
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
    }

    #[test]
    fn a_plain_archive_unpacks_with_its_directories() {
        let dir = TempDir::new().unwrap();
        let zip = dir.path().join("a.zip");
        write_zip(&zip, |w| {
            w.add_directory("composio-cli/", opts()).unwrap();
            w.start_file("composio-cli/SKILL.md", opts()).unwrap();
            w.write_all(b"---\nname: composio-cli\n---\n").unwrap();
            w.start_file("composio-cli/references/a.md", opts())
                .unwrap();
            w.write_all(b"ref").unwrap();
        });
        let out = dir.path().join("out");
        extract(&zip, &out, "a.zip").unwrap();
        assert!(out.join("composio-cli/SKILL.md").is_file());
        assert_eq!(
            fs::read_to_string(out.join("composio-cli/references/a.md")).unwrap(),
            "ref"
        );
    }

    #[test]
    fn hostile_entries_refuse_the_whole_archive_before_any_write() {
        type Build = Box<dyn FnOnce(&mut zip::ZipWriter<fs::File>)>;
        let cases: Vec<(&str, Build)> = vec![
            (
                "`..`",
                Box::new(|w| {
                    w.start_file("ok/SKILL.md", opts()).unwrap();
                    w.start_file("ok/../../escape", opts()).unwrap();
                }),
            ),
            (
                "an absolute path",
                Box::new(|w| {
                    w.start_file("ok/SKILL.md", opts()).unwrap();
                    w.start_file("/tmp/escape", opts()).unwrap();
                }),
            ),
            (
                "a symlink",
                Box::new(|w| {
                    w.start_file("ok/SKILL.md", opts()).unwrap();
                    w.add_symlink("ok/link", "/etc/passwd", opts()).unwrap();
                }),
            ),
            (
                "a backslash",
                Box::new(|w| {
                    w.start_file("ok/SKILL.md", opts()).unwrap();
                    w.start_file("ok\\..\\..\\escape", opts()).unwrap();
                }),
            ),
        ];
        for (label, build) in cases {
            let dir = TempDir::new().unwrap();
            let zip = dir.path().join("a.zip");
            write_zip(&zip, build);
            let out = dir.path().join("out");
            let error = extract(&zip, &out, "a.zip").unwrap_err();
            assert_eq!(error.code(), "release_archive_rejected", "{label}: {error}");
            assert!(
                error.to_string().contains(label),
                "rejected for {label}: {error}"
            );
            assert!(
                !out.exists(),
                "{label}: nothing is written before the preflight passes"
            );
            assert!(!dir.path().join("escape").exists(), "{label}");
        }
    }

    #[test]
    fn record_for_names_the_tag_and_keeps_the_previous_stamp_and_guards() {
        let mut previous = record_for(&source(), "@composio/cli@0.4.0", None);
        previous.imported_at = "2026-09-01T00:00:00Z".into();
        previous.variants.insert("claude".into(), "abc".into());
        let record = record_for(&source(), "@composio/cli@0.4.1", Some(&previous));
        assert_eq!(record.revision, "@composio/cli@0.4.1");
        assert_eq!(record.source_type, SOURCE_GITHUB_RELEASE);
        assert_eq!(record.source, "ComposioHQ/composio");
        assert_eq!(record.imported_at, "2026-09-01T00:00:00Z");
        assert_eq!(record.variants["claude"], "abc");
        assert_eq!(record.release, Some(source()));
    }
}
