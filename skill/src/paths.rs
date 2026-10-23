//! Every root the tool touches, resolved from the plan's environment table.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{PathsInfo, Platform};

/// Default npx package, pinned to the version the plan's facts describe.
pub const DEFAULT_NPX_PKG: &str = "skills@1.7.0";

/// A conventional Homebrew npx location; automatic discovery scans PATH.
pub const MAC_NPX: &str = "/opt/homebrew/bin/npx";

/// A skill gets exactly one path component, so nothing here resolves `.` or `..`.
fn is_bare_name(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

/// Lexically normalize a path: drop `.`, collapse `..` against the preceding
/// real component, and never touch the filesystem.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut depth = 0usize;
    let mut rooted = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                out.push(prefix.as_os_str());
                depth = 0;
                rooted = false;
            }
            Component::RootDir => {
                out.push(Component::RootDir.as_os_str());
                depth = 0;
                rooted = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if depth > 0 {
                    out.pop();
                    depth -= 1;
                } else if !rooted {
                    out.push("..");
                }
            }
            Component::Normal(part) => {
                out.push(part);
                depth += 1;
            }
        }
    }
    if out.as_os_str().is_empty() {
        out.push(Component::CurDir.as_os_str());
    }
    out
}

/// Make `path` absolute against `base` when it is relative, then normalize.
/// Purely lexical: symlinks are never resolved.
pub fn lexical_abs(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&base.join(path))
    }
}

/// Translate a no-follow metadata refusal at the call site, rather than
/// presenting an expected safety refusal as an internal error/raw errno.
pub(crate) fn metadata_error(path: &Path, err: &std::io::Error) -> Error {
    if matches!(err.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR))
        || err.kind() == std::io::ErrorKind::InvalidInput
    {
        Error::config_error(format!(
            "refused unsafe metadata path {}: tool-owned directory components must not be symlinks, and metadata must be a regular file",
            path.display()
        ))
    } else {
        Error::io(&format!("accessing {}", path.display()), err)
    }
}

/// Resolve the longest existing ancestor, retaining not-yet-created suffixes.
/// Unlike lexical comparison this catches symlink aliases; dangling symlinks
/// and inaccessible ancestors are errors, not silently treated as absent.
pub fn physical(path: &Path) -> Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => std::fs::canonicalize(path)
            .map_err(|err| Error::io(&format!("resolving {}", path.display()), &err)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| Error::config_error("path has no existing ancestor"))?;
            let name = path
                .file_name()
                .ok_or_else(|| Error::config_error("path has no final component"))?;
            Ok(physical(parent)?.join(name))
        }
        Err(err) => Err(Error::io(&format!("resolving {}", path.display()), &err)),
    }
}

/// A dangling link at a surface root (or one of its parents) is unavailable,
/// not an absent directory that the renderer may create through the link.
/// Permission failures and link loops are not hidden here: physical validation
/// still reports those as configuration errors.
pub fn surface_available(root: &Path) -> bool {
    !root.ancestors().any(|component| {
        std::fs::symlink_metadata(component).is_ok_and(|meta| meta.file_type().is_symlink())
            && std::fs::canonicalize(component)
                .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
    })
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Git preserves URLs and scp-style remotes verbatim, but stores local clone
/// origins as absolute paths. Resolve local settings before cloning so a later
/// sync compares the same origin, independent of the caller's working directory.
fn normalize_remote(home: &Path, base: &Path, value: &str) -> String {
    if value.trim().is_empty() || value.starts_with('-') || value.contains(':') {
        return value.to_owned();
    }
    let path = Path::new(value);
    let path = if value == "~" || value.starts_with("~/") {
        crate::config::config_path(home, path)
    } else {
        lexical_abs(base, path)
    };
    path.to_string_lossy().into_owned()
}

fn env_override(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn default_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Mac
    } else {
        Platform::Linux
    }
}

fn platform_from_env() -> Result<Platform> {
    match env_override("SKILLHERD_PLATFORM") {
        Some(value) => match value.to_str().map(str::trim) {
            Some("mac") => Ok(Platform::Mac),
            Some("linux") => Ok(Platform::Linux),
            _ => Err(Error::config_error(format!(
                "SKILLHERD_PLATFORM must be `mac` or `linux`, got {}",
                value.display()
            ))),
        },
        None => Ok(default_platform()),
    }
}

/// The first `npx` on `path_env` that is not this tool's opt-in wrapper.
///
/// The shim rewrites `npx skills …` into `skillherd …`; the wrapper must call the
/// real binary, so a shim hit is skipped rather than selected.
pub fn pick_npx_from_path(path_env: &OsStr) -> Option<PathBuf> {
    for dir in std::env::split_paths(path_env) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join("npx");
        if !candidate.is_file() {
            continue;
        }
        let resolved = std::fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
        if is_npx_wrapper(&candidate) {
            continue;
        }
        // A version manager's shim canonicalizes to the manager (mise's
        // `shims/npx` is a link to `/usr/bin/mise`), which run as npx does
        // something else entirely. The manager says where npx is; a shim
        // that cannot be asked is passed over. The candidate itself, not its
        // canonical `npx-cli.js`, is the answer: its directory is where the
        // `node` its shebang wants lives.
        match resolved.file_name().and_then(OsStr::to_str) {
            Some("mise" | "rtx" | "asdf") => {
                if let Some(answer) = manager_which_npx(&resolved) {
                    return Some(answer);
                }
                continue;
            }
            _ => return Some(candidate),
        }
    }
    None
}

pub(crate) fn is_npx_wrapper(path: &Path) -> bool {
    use std::io::Read;
    let mut header = [0; 1024];
    std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut header))
        .is_ok_and(|n| {
            String::from_utf8_lossy(&header[..n])
                .lines()
                .any(|line| line == "# skillherd-npx-shim: 1")
        })
}

/// `<manager> which npx`, trusted only when it names an existing file.
fn manager_which_npx(manager: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new(manager)
        .args(["which", "npx"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let answer = PathBuf::from(text.lines().next()?.trim());
    answer.is_file().then_some(answer)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn default_npx() -> Result<PathBuf> {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    // Listing/rendering a local pool does not require Node. A staged install
    // reports a missing executable only when it actually needs npx.
    Ok(pick_npx_from_path(&path_env).unwrap_or_else(|| PathBuf::from("npx")))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn default_npx() -> Result<PathBuf> {
    Err(Error::config_error(
        "no default npx path on this platform; set SKILLHERD_NPX",
    ))
}

/// The first executable `gh` on `path_env`: the GitHub CLI a release fetch
/// authenticates through. `None` sends the fetch to the token fallback.
pub fn pick_gh_from_path(path_env: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path_env)
        .map(|dir| dir.join("gh"))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// The resolved roots. `HOME` is the root of every default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub library: PathBuf,
    pub agents_dir: PathBuf,
    pub lock: PathBuf,
    pub stage: PathBuf,
    pub claude_shared: PathBuf,
    pub codex_home: PathBuf,
    pub grok_home: PathBuf,
    pub pi_skills: PathBuf,
    pub cursor_skills: PathBuf,
    pub platform: Platform,
    pub npx: PathBuf,
    pub npx_pkg: String,
    /// The GitHub CLI for release fetches (`SKILLHERD_GH`, else `gh` on PATH).
    pub gh: Option<PathBuf>,
    pub home: PathBuf,
    pub pool_remote: Option<String>,
    pub sync_command: Option<PathBuf>,
}

impl Paths {
    /// Read the environment table. `validate()` is a separate call so callers
    /// can report a config error before touching anything.
    pub fn resolve() -> Result<Paths> {
        Self::resolve_pool(None, None)
    }

    pub fn resolve_pool(pool: Option<&Path>, remote: Option<&str>) -> Result<Paths> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home = env_override("HOME").ok_or_else(|| {
            Error::config_error("HOME is not set; every skill root is resolved from it")
        })?;
        let home = lexical_abs(&cwd, &home);

        let config = crate::config::Config::load(&home)?;
        let library = pool
            .map(|path| lexical_abs(&cwd, path))
            .or_else(|| env_override("SKILLHERD_LIBRARY").map(|path| lexical_abs(&cwd, &path)))
            .or_else(|| {
                config
                    .library
                    .as_ref()
                    .map(|path| crate::config::config_path(&home, path))
            })
            .unwrap_or_else(|| home.join(".agents/skill-library"));
        let agents_dir = env_override("SKILLHERD_AGENTS_DIR")
            .map(|path| lexical_abs(&cwd, &path))
            .or_else(|| {
                config
                    .agents_dir
                    .as_ref()
                    .map(|path| crate::config::config_path(&home, path))
            })
            .unwrap_or_else(|| home.join(".agents/skills"));
        let pool_remote = remote
            .map(|value| normalize_remote(&home, &cwd, value))
            .or_else(|| {
                std::env::var("SKILLHERD_POOL_REMOTE")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .map(|value| normalize_remote(&home, &cwd, &value))
            })
            .or_else(|| {
                config
                    .remote
                    .map(|value| normalize_remote(&home, &home, &value))
            });
        let sync_command = env_override("SKILLHERD_SYNC_COMMAND").or(config.sync_command);
        let sync_command = sync_command.map(|path| {
            if is_bare_name(&path) {
                path
            } else {
                crate::config::config_path(&home, &path)
            }
        });
        let lock = match env_override("SKILLHERD_LOCK") {
            Some(path) => lexical_abs(&cwd, &path),
            None => match env_override("XDG_STATE_HOME") {
                Some(state) => lexical_abs(&cwd, &state).join("skillherd/.skill-lock.json"),
                None => home.join(".agents/.skill-lock.json"),
            },
        };
        let stage = env_override("SKILLHERD_STAGE")
            .map(|path| lexical_abs(&cwd, &path))
            .unwrap_or_else(|| home.join(".agents/.skill-stage"));
        let claude_shared = env_override("CLAUDE_SHARED")
            .map(|path| lexical_abs(&cwd, &path))
            .or_else(|| {
                config
                    .claude_home
                    .as_ref()
                    .map(|path| crate::config::config_path(&home, path))
            })
            .or_else(|| env_override("CLAUDE_CONFIG_DIR").map(|path| lexical_abs(&cwd, &path)))
            .unwrap_or_else(|| home.join(".claude"));
        let codex_home = env_override("CODEX_HOME")
            .map(|path| lexical_abs(&cwd, &path))
            .or_else(|| {
                config
                    .codex_home
                    .as_ref()
                    .map(|path| crate::config::config_path(&home, path))
            })
            .unwrap_or_else(|| home.join(".codex"));
        let grok_home = env_override("GROK_HOME")
            .map(|path| lexical_abs(&cwd, &path))
            .unwrap_or_else(|| home.join(".grok"));
        let pi_skills = env_override("SKILLHERD_PI_SKILLS")
            .map(|path| lexical_abs(&cwd, &path))
            .or_else(|| {
                config
                    .pi_skills
                    .as_ref()
                    .map(|path| crate::config::config_path(&home, path))
            })
            .unwrap_or_else(|| home.join(".pi/agent/skills"));
        let cursor_skills = env_override("SKILLHERD_CURSOR_SKILLS")
            .map(|path| lexical_abs(&cwd, &path))
            .or_else(|| {
                config
                    .cursor_skills
                    .as_ref()
                    .map(|path| crate::config::config_path(&home, path))
            })
            .unwrap_or_else(|| home.join(".cursor/skills"));

        let platform = platform_from_env()?;

        let npx = match env_override("SKILLHERD_NPX") {
            Some(path) if path == Path::new("npx") => default_npx()?,
            Some(path) if is_bare_name(&path) => path,
            Some(path) => lexical_abs(&cwd, &path),
            None => default_npx()?,
        };
        let npx_pkg = env_override("SKILLHERD_NPX_PKG")
            .and_then(|value| value.to_str().map(str::to_owned))
            .unwrap_or_else(|| DEFAULT_NPX_PKG.to_string());
        let gh = match env_override("SKILLHERD_GH") {
            Some(path) if is_bare_name(&path) => Some(path),
            Some(path) => Some(lexical_abs(&cwd, &path)),
            None => pick_gh_from_path(&std::env::var_os("PATH").unwrap_or_default()),
        };

        let paths = Paths {
            library,
            agents_dir,
            lock,
            stage,
            claude_shared,
            codex_home,
            grok_home,
            pi_skills,
            cursor_skills,
            platform,
            npx,
            npx_pkg,
            gh,
            home,
            pool_remote,
            sync_command,
        };
        crate::fsx::trust_paths(&paths)
            .map_err(|err| Error::io("resolving operator roots", &err))?;
        Ok(paths)
    }

    /// The plan's defaults for a given `home`, taking nothing from the
    /// environment. Unit tests and lane fixtures use this so a run is
    /// reproducible; it skips the PATH scan for npx.
    pub fn fixture(home: impl Into<PathBuf>) -> Paths {
        let home = home.into();
        let paths = Paths {
            library: home.join(".agents/skill-library"),
            agents_dir: home.join(".agents/skills"),
            lock: home.join(".agents/.skill-lock.json"),
            stage: home.join(".agents/.skill-stage"),
            claude_shared: home.join(".claude"),
            codex_home: home.join(".codex"),
            grok_home: home.join(".grok"),
            pi_skills: home.join(".pi/agent/skills"),
            cursor_skills: home.join(".cursor/skills"),
            platform: default_platform(),
            npx: PathBuf::from(MAC_NPX),
            npx_pkg: DEFAULT_NPX_PKG.to_string(),
            gh: None,
            home,
            pool_remote: None,
            sync_command: None,
        };
        crate::fsx::trust_paths(&paths).expect("fixture operator roots resolve");
        paths
    }

    /// Every surface the renderer owns, keyed by harness name.
    pub fn surfaces(&self) -> Vec<(&'static str, PathBuf)> {
        crate::policy::HARNESS_REGISTRY
            .iter()
            .map(|spec| (spec.name, (spec.surface)(self)))
            .collect()
    }

    /// Reject overlapping physical roots before any command touches the filesystem.
    pub fn validate(&self) -> Result<()> {
        let surfaces = self.surfaces();
        self.validate_surfaces(surfaces.iter().map(|(name, path)| (*name, path.as_path())))?;
        let library = physical(&self.library)?;
        let stage = physical(&self.stage)?;
        if overlaps(&library, &stage) {
            return Err(Error::config_error(
                "stage and library overlap; choose disjoint roots",
            ));
        }
        Ok(())
    }

    /// Validate the entire loaded table, not just a requested subset. Existing
    /// symlink ancestors are resolved even when the final directory is absent.
    pub fn validate_surfaces<'a>(
        &self,
        roots: impl IntoIterator<Item = (&'a str, &'a Path)>,
    ) -> Result<()> {
        let home = physical(&self.home)?;
        let protected = [
            ("library", physical(&self.library)?),
            ("stage", physical(&self.stage)?),
            ("lock", physical(&self.lock)?),
            (
                "state",
                physical(&self.home.join(".local/state/skillherd/reconcile"))?,
            ),
        ];
        let mut surfaces: Vec<(&str, PathBuf)> = Vec::new();
        for (name, root) in roots {
            if !surface_available(root) {
                crate::output::warn(
                    "surface_unavailable",
                    format!(
                        "surface {name} ({}) has a dangling symlink; this harness is unavailable and will be skipped. Repair or remove that symlink to enable it",
                        root.display()
                    ),
                );
                continue;
            }
            let root = physical(root)?;
            if home.starts_with(&root) {
                return Err(Error::config_error(format!(
                    "surface {name} ({}) is HOME or an ancestor of HOME",
                    root.display()
                )));
            }
            for (kind, path) in &protected {
                if overlaps(&root, path) {
                    return Err(Error::config_error(format!(
                        "surface {name} ({}) overlaps {kind} ({})",
                        root.display(),
                        path.display()
                    )));
                }
            }
            for (other_name, other) in &surfaces {
                if overlaps(&root, other) {
                    return Err(Error::config_error(format!(
                        "surfaces {name} and {other_name} overlap ({})",
                        root.display()
                    )));
                }
            }
            surfaces.push((name, root));
        }
        Ok(())
    }

    /// A linked library entry may live outside the library root. It is still
    /// a source, never disposable output. Check these physical trees too.
    pub fn validate_sources(&self, harnesses: &[crate::model::HarnessRow]) -> Result<()> {
        if let Some(message) = self.source_overlaps(harnesses)?.first() {
            return Err(Error::config_error(message));
        }
        Ok(())
    }

    /// Inspection remains available to explain a source/output overlap. Only
    /// mutators must refuse it; this never grants a write to the affected tree.
    pub fn source_overlaps(&self, harnesses: &[crate::model::HarnessRow]) -> Result<Vec<String>> {
        let mut messages = Vec::new();
        let read = match std::fs::read_dir(&self.library) {
            Ok(read) => read,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(messages),
            Err(err) => return Err(Error::io("reading library source roots", &err)),
        };
        let surfaces: Vec<_> = harnesses
            .iter()
            .filter(|row| surface_available(&row.surface))
            .map(|row| physical(&row.surface))
            .collect::<Result<_>>()?;
        for entry in read {
            let entry = entry.map_err(|err| Error::io("reading source entry", &err))?;
            if entry.file_name().to_string_lossy().starts_with('.')
                || !entry.path().join("SKILL.md").is_file()
            {
                continue;
            }
            let mut sources = vec![physical(&entry.path())?];
            for key in harnesses
                .iter()
                .filter_map(|row| row.variant_key.as_deref())
            {
                let variant = entry.path().join(crate::library::VARIANTS_DIR).join(key);
                if variant.is_dir() {
                    sources.push(physical(&variant)?);
                }
            }
            for source in sources {
                for surface in &surfaces {
                    if overlaps(surface, &source) {
                        messages.push(format!(
                            "library entry {} resolves to source tree ({}) overlapping surface ({}). Move the source into a separate directory, then update the library symlink (or replace it with the moved directory). Do not remove the surface's only copy; reconcile cannot repair this layout automatically",
                            entry.path().display(),
                            source.display(),
                            surface.display()
                        ));
                    }
                }
            }
        }
        Ok(messages)
    }

    pub fn info(&self) -> PathsInfo {
        PathsInfo::from(self)
    }
}

impl From<&Paths> for PathsInfo {
    fn from(paths: &Paths) -> Self {
        Self {
            library: paths.library.clone(),
            agents_dir: paths.agents_dir.clone(),
            lock: paths.lock.clone(),
            stage: paths.stage.clone(),
            claude_shared: paths.claude_shared.clone(),
            codex_home: paths.codex_home.clone(),
            grok_home: paths.grok_home.clone(),
            pi_skills: paths.pi_skills.clone(),
            cursor_skills: paths.cursor_skills.clone(),
            platform: paths.platform,
            npx: paths.npx.clone(),
            npx_pkg: paths.npx_pkg.clone(),
            gh: paths.gh.clone(),
            home: paths.home.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn npx_wrapper_detection_requires_the_exact_owned_marker() {
        let dir = TempDir::new().unwrap();
        let candidate = dir.path().join("npx");
        for header in [
            "#!/bin/sh\n# My wrapper around the real npx\n",
            "#!/bin/sh\n# not # skillherd-npx-shim: 1\n",
            "#!/bin/sh\n# skillherd-npx-shim: 2\n",
        ] {
            fs::write(&candidate, header).unwrap();
            assert_eq!(
                pick_npx_from_path(dir.path().as_os_str()),
                Some(candidate.clone()),
                "{header}"
            );
        }
        fs::write(&candidate, "#!/bin/sh\n# skillherd-npx-shim: 1\n").unwrap();
        assert_eq!(pick_npx_from_path(dir.path().as_os_str()), None);
    }

    #[test]
    fn atomic_metadata_write_refuses_non_regular_leaf() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("user-data");
        let path = dir.path().join("metadata");
        fs::write(&target, "keep me").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(crate::fsx::atomic_write(&path, b"replacement").is_err());
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep me");
    }

    #[test]
    fn normalize_collapses_dot_and_parent_components() {
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(normalize(Path::new("/a/..")), PathBuf::from("/"));
        assert_eq!(normalize(Path::new("a/../..")), PathBuf::from(".."));
        assert_eq!(normalize(Path::new("/a//b/")), PathBuf::from("/a/b"));
        assert_eq!(normalize(Path::new("")), PathBuf::from("."));
    }

    #[test]
    fn lexical_abs_never_resolves_symlinks() {
        let home = Path::new("/sandbox/user");
        // The Claude surface link text into the pool: two levels up, then down.
        assert_eq!(
            lexical_abs(
                &home.join(".claude/skills"),
                Path::new("../../.agents/skill-library/post")
            ),
            home.join(".agents/skill-library/post")
        );
        assert_eq!(
            lexical_abs(home, Path::new("/etc/skills")),
            PathBuf::from("/etc/skills")
        );
        assert_eq!(lexical_abs(home, Path::new("rel")), home.join("rel"));
    }

    #[test]
    fn fixture_roots_are_the_documented_defaults() {
        let paths = Paths::fixture("/sandbox/user");
        assert_eq!(
            paths.library,
            PathBuf::from("/sandbox/user/.agents/skill-library")
        );
        assert_eq!(
            paths.agents_dir,
            PathBuf::from("/sandbox/user/.agents/skills")
        );
        assert_eq!(
            paths.lock,
            PathBuf::from("/sandbox/user/.agents/.skill-lock.json")
        );
        assert_eq!(
            paths.stage,
            PathBuf::from("/sandbox/user/.agents/.skill-stage")
        );
        assert_eq!(paths.claude_shared, PathBuf::from("/sandbox/user/.claude"));
        assert_eq!(
            paths.pi_skills,
            PathBuf::from("/sandbox/user/.pi/agent/skills")
        );
        assert_eq!(
            paths.cursor_skills,
            PathBuf::from("/sandbox/user/.cursor/skills")
        );
        assert_eq!(paths.npx_pkg, "skills@1.7.0");
        assert!(
            paths.validate().is_ok(),
            "the documented defaults are disjoint"
        );
    }

    #[test]
    fn validate_rejects_a_library_inside_a_surface() {
        let mut paths = Paths::fixture("/sandbox/user");
        paths.library = paths.claude_shared.join("skills/pool");
        let err = paths.validate().unwrap_err();
        assert_eq!(err.code(), "config_error");
        assert_eq!(err.exit_code(), 78);
        let message = err.to_string();
        assert!(message.contains("claude"), "{message}");
        assert!(
            message.contains("skill-library") || message.contains("pool"),
            "{message}"
        );
    }

    #[test]
    fn validate_rejects_a_surface_inside_the_library() {
        let mut paths = Paths::fixture("/sandbox/user");
        paths.agents_dir = paths.library.join("links");
        let err = paths.validate().unwrap_err();
        assert_eq!(err.code(), "config_error");
        assert!(err.to_string().contains("agents"), "{err}");
    }

    #[test]
    fn validate_rejects_two_surfaces_on_one_path() {
        let mut paths = Paths::fixture("/sandbox/user");
        paths.pi_skills = paths.cursor_skills.clone();
        let err = paths.validate().unwrap_err();
        assert!(err.to_string().contains("pi"), "{err}");
        assert!(err.to_string().contains("cursor"), "{err}");
    }

    #[test]
    fn validate_rejects_a_stage_inside_the_library() {
        let mut paths = Paths::fixture("/sandbox/user");
        paths.stage = paths.library.join(".skill-stage");
        let err = paths.validate().unwrap_err();
        assert!(err.to_string().contains("stage"), "{err}");
    }

    /// A version manager's shim canonicalizes to the manager itself
    /// (`~/.local/share/mise/shims/npx` → `/usr/bin/mise`); run as npx it
    /// may interpret arguments as its own commands. The manager is
    /// asked where npx really is.
    #[test]
    fn npx_scan_resolves_a_mise_shim_through_the_manager() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let shims = home.join("mise/shims");
        let tool = home.join("mise/installs/node/26/bin");
        let bin = home.join("usr/bin");
        for d in [&shims, &tool, &bin] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(tool.join("npx"), "#!/bin/sh\n").unwrap();
        fs::write(
            bin.join("mise"),
            format!(
                "#!/bin/sh\n[ \"$1\" = which ] && [ \"$2\" = npx ] && printf '%s\\n' '{}'\n",
                tool.join("npx").display()
            ),
        )
        .unwrap();
        fs::set_permissions(bin.join("mise"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(bin.join("mise"), shims.join("npx")).unwrap();

        let path = std::env::join_paths([&shims]).unwrap();
        assert_eq!(
            pick_npx_from_path(&path),
            Some(tool.join("npx")),
            "the manager's answer, not the manager"
        );
    }

    #[test]
    fn npx_scan_passes_over_a_manager_with_no_answer() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let shims = home.join("mise/shims");
        let bin = home.join("usr/bin");
        let real_dir = home.join("real/bin");
        for d in [&shims, &bin, &real_dir] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(bin.join("mise"), "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(bin.join("mise"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(bin.join("mise"), shims.join("npx")).unwrap();
        fs::write(real_dir.join("npx"), "#!/bin/sh\n").unwrap();

        let shim_first = std::env::join_paths([&shims, &real_dir]).unwrap();
        assert_eq!(
            pick_npx_from_path(&shim_first).map(|p| fs::canonicalize(p).unwrap()),
            Some(fs::canonicalize(real_dir.join("npx")).unwrap()),
            "a manager that names nothing is passed over for the next candidate"
        );
        let only_shim = std::env::join_paths([&shims]).unwrap();
        assert_eq!(pick_npx_from_path(&only_shim), None);
    }

    #[test]
    fn npx_scan_skips_the_local_bin_shim() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let shim_dir = home.join(".local/bin");
        let real_dir = home.join("real/bin");
        fs::create_dir_all(&shim_dir).unwrap();
        fs::create_dir_all(&real_dir).unwrap();
        fs::write(shim_dir.join("npx"), "#!/bin/sh\n# skillherd-npx-shim: 1\n").unwrap();
        fs::write(real_dir.join("npx"), "#!/bin/sh\n").unwrap();

        let shim_first = std::env::join_paths([&shim_dir, &real_dir]).unwrap();
        let picked = pick_npx_from_path(&shim_first).expect("the real npx is on PATH");
        assert_eq!(
            std::fs::canonicalize(&picked).unwrap(),
            std::fs::canonicalize(real_dir.join("npx")).unwrap()
        );

        let only_shim = std::env::join_paths([&shim_dir]).unwrap();
        assert_eq!(pick_npx_from_path(&only_shim), None);

        let empty = std::env::join_paths([home.join("nowhere")]).unwrap();
        assert_eq!(pick_npx_from_path(&empty), None);
    }

    #[test]
    fn surfaces_are_the_six_harness_roots() {
        let paths = Paths::fixture("/sandbox/user");
        let names: Vec<&str> = paths.surfaces().iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["claude", "codex", "agents", "grok", "pi", "cursor"]);
    }

    #[test]
    fn info_mirrors_every_root() {
        let paths = Paths::fixture("/sandbox/user");
        let info = paths.info();
        let value = serde_json::to_value(&info).unwrap();
        for key in [
            "library",
            "agents_dir",
            "lock",
            "stage",
            "claude_shared",
            "codex_home",
            "grok_home",
            "pi_skills",
            "cursor_skills",
            "platform",
            "npx",
            "npx_pkg",
            "home",
        ] {
            assert!(value.get(key).is_some(), "meta.paths is missing {key}");
        }
        assert_eq!(value["library"], "/sandbox/user/.agents/skill-library");
    }
}
