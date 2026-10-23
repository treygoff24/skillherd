//! Filesystem primitives the rest of the crate must not reinvent.
//!
//! One rule lives here: a rename that lands the pool's only copy of something
//! never overwrites what is at the destination. `fs::rename` replaces an empty
//! directory, a file or a symlink at `to` silently, so a check followed by a
//! rename has a window in which a destination created in between is lost.
//! [`rename_noreplace`] closes it: the check and the rename are one syscall.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Rename `from` to `to`, failing with [`io::ErrorKind::AlreadyExists`] when
/// anything — a directory, a file, a symlink, dangling or not — sits at `to`.
///
/// Atomic with respect to the destination: macOS `renamex_np(RENAME_EXCL)`,
/// Linux `renameat2(RENAME_NOREPLACE)`. A filesystem without the flag
/// (`EINVAL` on Linux, `ENOTSUP` on macOS) is an error, not a downgrade: a
/// check-then-rename has exactly the window this function exists to close, so
/// the caller refuses instead. APFS, ext4, xfs, btrfs and OpenZFS ≥ 2.2
/// support the flag.
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let from_c = CString::new(from.as_os_str().as_bytes())?;
    let to_c = CString::new(to.as_os_str().as_bytes())?;
    let rc = unsafe { rename_excl(from_c.as_ptr(), to_c.as_ptr()) };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EINVAL) | Some(libc::ENOTSUP) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "the filesystem holding {} has no no-replace rename ({err}); this tool does not fall back to a rename that could overwrite the destination",
                to.display()
            ),
        )),
        _ => Err(err),
    }
}

#[cfg(target_os = "macos")]
unsafe fn rename_excl(from: *const libc::c_char, to: *const libc::c_char) -> libc::c_int {
    unsafe { libc::renamex_np(from, to, libc::RENAME_EXCL) }
}

#[cfg(target_os = "linux")]
unsafe fn rename_excl(from: *const libc::c_char, to: *const libc::c_char) -> libc::c_int {
    unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from,
            libc::AT_FDCWD,
            to,
            libc::RENAME_NOREPLACE,
        )
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("rename_noreplace needs renamex_np (macOS) or renameat2 (Linux)");

struct TrustedRoot {
    path: std::path::PathBuf,
    anchor: std::fs::File,
    missing: std::path::PathBuf,
}

thread_local! {
    // Paths belong to the invoking operator, not to shared library content.
    // Retain the resolved fd: replacing a symlink later cannot retarget it.
    static ROOTS: std::cell::RefCell<Vec<TrustedRoot>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Resolve an operator-chosen root once, following its existing ancestors.
/// Any not-yet-existing suffix is subsequently walked with O_NOFOLLOW.
/// Never register a directory chosen by library metadata as a trusted root.
pub fn trust_root(path: &Path) -> io::Result<()> {
    let path = crate::paths::lexical_abs(&std::env::current_dir()?, path);
    ROOTS.with(|roots| {
        let mut roots = roots.borrow_mut();
        if roots.iter().any(|root| root.path == path) {
            return Ok(());
        }
        let mut existing = path.as_path();
        let mut missing = std::path::PathBuf::new();
        loop {
            match std::fs::canonicalize(existing) {
                Ok(resolved) => {
                    let anchor = std::fs::File::open(resolved)?;
                    if !anchor.metadata()?.is_dir() {
                        return Err(io::Error::new(
                            io::ErrorKind::NotADirectory,
                            format!("operator root {} is not a directory", existing.display()),
                        ));
                    }
                    roots.push(TrustedRoot {
                        path,
                        anchor,
                        missing,
                    });
                    return Ok(());
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    let name = existing.file_name().ok_or(err)?;
                    missing = Path::new(name).join(missing);
                    existing = existing.parent().unwrap_or(Path::new("/"));
                }
                Err(err) => return Err(err),
            }
        }
    })
}

/// Tool-local persistent state. XDG roots, like HOME, are operator input;
/// the `skill` and recovery children below them are tool-owned directories.
pub fn state_dir(paths: &crate::paths::Paths) -> std::path::PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| paths.home.join(".local/state"))
        .join("skillherd")
}

/// Register only the roots resolved from local operator configuration.
pub fn trust_paths(paths: &crate::paths::Paths) -> io::Result<()> {
    for root in [
        &paths.home,
        &paths.home.join(".agents"),
        &paths.library,
        &paths.stage,
        &state_dir(paths).parent().unwrap().to_path_buf(),
    ] {
        trust_root(root)?;
    }
    Ok(())
}

/// Walk owned components below a previously resolved operator root. Unknown
/// paths fail closed with a strict walk from `/`. Holding each parent fd
/// prevents a concurrent path swap redirecting a metadata operation.
fn directory(path: &Path, create: bool) -> io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::path::Component;
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let registered = ROOTS.with(|roots| {
        let roots = roots.borrow();
        roots
            .iter()
            .filter(|root| absolute.starts_with(&root.path))
            .max_by_key(|root| root.path.components().count())
            .map(|root| {
                Ok::<_, io::Error>((
                    root.anchor.try_clone()?,
                    root.missing
                        .join(absolute.strip_prefix(&root.path).unwrap()),
                ))
            })
            .transpose()
    })?;
    let (mut parent, remainder) = match registered {
        Some(registered) => registered,
        None => (std::fs::File::open("/")?, absolute.clone()),
    };
    let mut traversed = absolute
        .ancestors()
        .nth(
            remainder
                .components()
                .filter(|part| matches!(part, Component::Normal(_)))
                .count(),
        )
        .unwrap_or(Path::new("/"))
        .to_path_buf();
    for component in remainder.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => CString::new(name.as_bytes())?,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsafe parent component",
                ));
            }
        };
        traversed.push(std::ffi::OsStr::from_bytes(name.as_bytes()));
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 && create && io::Error::last_os_error().kind() == io::ErrorKind::NotFound {
            let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) };
            if rc < 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
                return Err(io::Error::last_os_error());
            }
            fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        }
        if fd < 0 {
            let err = io::Error::last_os_error();
            return Err(io::Error::new(
                err.kind(),
                format!(
                    "refusing directory component {}: expected a real directory, not a symlink ({err})",
                    traversed.display()
                ),
            ));
        }
        parent = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(parent)
}

/// Verify and open an existing directory with no-follow on every component.
/// Subsequent path-named operations still need their own re-proof; this fd
/// only anchors operations that actually use it.
pub fn open_dir_nofollow(path: &Path) -> io::Result<std::fs::File> {
    directory(path, false)
}

/// Create missing directories and open the result, refusing all symlinks.
pub fn create_dir_all_nofollow(path: &Path) -> io::Result<std::fs::File> {
    directory(path, true)
}

/// List a directory through its no-follow-opened descriptor.
pub fn read_dir_names_nofollow(path: &Path) -> io::Result<Vec<std::ffi::OsString>> {
    use std::ffi::{CStr, OsString};
    use std::os::fd::{FromRawFd, IntoRawFd};
    let fd = directory(path, false)?.into_raw_fd();
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let err = io::Error::last_os_error();
        drop(unsafe { std::fs::File::from_raw_fd(fd) });
        return Err(err);
    }
    struct Stream(*mut libc::DIR);
    impl Drop for Stream {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let stream = Stream(stream);
    let mut names = Vec::new();
    loop {
        // readdir uses null for both EOF and failure. Clear the thread-local
        // errno first so a prior, unrelated syscall cannot turn EOF into error.
        unsafe {
            #[cfg(target_os = "macos")]
            {
                *libc::__error() = 0;
            }
            #[cfg(target_os = "linux")]
            {
                *libc::__errno_location() = 0;
            }
        }
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(0) {
                return Err(err);
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(OsString::from(std::ffi::OsStr::from_bytes(name)));
        }
    }
    names.sort();
    Ok(names)
}

fn leaf(path: &Path) -> io::Result<CString> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?;
    CString::new(name.as_bytes()).map_err(Into::into)
}

/// Read a regular file without following a symlink at the leaf or any parent.
/// `NotFound` is preserved for optional metadata. No directories are created.
pub fn read_to_string(path: &Path) -> io::Result<String> {
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd};
    let parent = directory(path.parent().unwrap_or(Path::new(".")), false)?;
    let name = leaf(path)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "metadata is not a regular file",
        ));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

/// Atomically replace tool-owned metadata, never following a symlink.
///
/// The entire parent chain is opened no-follow, creating missing directories.
/// The destination must be absent or a regular file. Bytes are written and
/// synced through an exclusively created, no-follow, same-directory temporary
/// fd, then published with `renameat` on the held parent fd and the directory
/// synced. A swapped parent cannot redirect publication; a swapped leaf link
/// is replaced, never followed. A failed write leaves the old file intact.
/// This intentionally replaces metadata; use `rename_noreplace` for skill trees.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};
    let parent = directory(path.parent().unwrap_or(Path::new(".")), true)?;
    let name = leaf(path)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    let mut mode = None;
    if rc == 0 {
        let stat = unsafe { stat.assume_init() };
        mode = Some(stat.st_mode & 0o777);
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "metadata destination is not a regular file",
            ));
        }
    } else if io::Error::last_os_error().kind() != io::ErrorKind::NotFound {
        return Err(io::Error::last_os_error());
    }
    let temp = CString::new(format!(".skill-write-{}", crate::output::random_uuid_v4()))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            temp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o666,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let result = (|| {
        if let Some(mode) = mode
            && unsafe { libc::fchmod(file.as_raw_fd(), mode) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        if unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                temp.as_ptr(),
                parent.as_raw_fd(),
                name.as_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        parent.sync_all().map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "write to {} was published, but syncing its directory failed: {err}",
                    path.display()
                ),
            )
        })
    })();
    if result.is_err() {
        // This is only our exclusively created temporary entry, never the live file.
        unsafe { libc::unlinkat(parent.as_raw_fd(), temp.as_ptr(), 0) };
    }
    result
}

/// Open the stable lock inode without following links in it or its parents.
/// Locks must not use atomic replacement: doing so would split concurrent
/// flock holders across distinct inodes.
pub fn open_lock_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let parent = directory(path.parent().unwrap_or(Path::new(".")), true)?;
    let name = leaf(path)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "lock is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::rename_noreplace;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn scratch() -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "skill-fsx-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn operator_roots_accept_symlinked_ancestors_but_metadata_children_do_not() {
        let root = scratch();
        let real = root.join("real");
        fs::create_dir(&real).unwrap();
        let alias = root.join("alias");
        symlink(&real, &alias).unwrap();
        let paths = crate::paths::Paths::fixture(alias.join("home"));
        let target = paths.library.join(".globals/house");
        super::atomic_write(&target, b"safe\n").unwrap();
        assert_eq!(super::read_to_string(&target).unwrap(), "safe\n");
        fs::remove_file(&target).unwrap();
        fs::remove_dir(target.parent().unwrap()).unwrap();
        symlink(&real, target.parent().unwrap()).unwrap();
        let error = super::atomic_write(&target, b"unsafe\n").unwrap_err();
        assert!(error.to_string().contains(".globals"), "{error}");
        assert!(!real.join("house").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_listing_refuses_symlinked_metadata_children() {
        let root = scratch();
        let paths = crate::paths::Paths::fixture(&root);
        fs::create_dir_all(&paths.library).unwrap();
        let outside = root.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("marker"), "foreign\n").unwrap();
        let link = paths.library.join(".globals");
        symlink(&outside, &link).unwrap();
        assert!(super::read_dir_names_nofollow(&link).is_err());
        assert_eq!(
            super::read_dir_names_nofollow(&outside).unwrap(),
            vec![std::ffi::OsString::from("marker")]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomic_metadata_replacement_never_truncates_an_existing_inode() {
        let root = scratch();
        super::trust_root(&root).unwrap();
        let path = root.join("metadata");
        fs::write(&path, "old").unwrap();
        let other_link = root.join("old-inode");
        fs::hard_link(&path, &other_link).unwrap();
        assert_eq!(fs::read_to_string(&other_link).unwrap(), "old");
        super::atomic_write(&path, b"new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(fs::read_to_string(&other_link).unwrap(), "old");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomic_metadata_replacement_preserves_the_existing_mode() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch();
        let paths = crate::paths::Paths::fixture(&root);
        let file = paths.library.join(".globals/house");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "old\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        super::atomic_write(&file, b"new\n").unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_free_destination_is_taken_and_a_taken_one_is_refused() {
        let root = scratch();
        let from = root.join("from");
        fs::create_dir(&from).unwrap();
        fs::write(from.join("SKILL.md"), "body\n").unwrap();
        let free = root.join("free");
        rename_noreplace(&from, &free).unwrap();
        assert!(free.join("SKILL.md").is_file());
        assert!(!from.exists());

        // `fs::rename` would replace each of these in silence: an empty
        // directory outright, and (for a file source) a symlink or a file.
        let empty_dir = root.join("empty");
        fs::create_dir(&empty_dir).unwrap();
        let err = rename_noreplace(&free, &empty_dir).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
        assert!(free.join("SKILL.md").is_file(), "the source is untouched");
        assert!(empty_dir.is_dir(), "the destination is untouched");

        let file = root.join("file");
        fs::write(&file, "one\n").unwrap();
        let link = root.join("link");
        symlink("/nonexistent", &link).unwrap();
        let err = rename_noreplace(&file, &link).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
        assert_eq!(
            fs::read_link(&link).unwrap().to_string_lossy(),
            "/nonexistent",
            "the dangling link at the destination survives"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "one\n");
        fs::remove_dir_all(&root).unwrap();
    }
}
