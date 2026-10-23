//! One exclusive flock per mutator, held on `library/.skill.lock`.

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::paths::Paths;

/// Holds the library lock; dropping it unlocks.
#[derive(Debug)]
pub struct MutatorGuard {
    file: File,
    path: PathBuf,
}

impl MutatorGuard {
    /// The lock file this guard holds.
    pub fn lock_path(&self) -> &Path {
        &self.path
    }
}

impl Drop for MutatorGuard {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// Take the exclusive library lock, creating the library and the lock file.
///
/// Held by another process → `library_busy` (75, retryable). A `--dry-run`
/// caller must not call this.
///
/// The guard is **not reentrant**: flock is held per open file description, so a
/// second `lock_library` — same process, same thread, or another — is
/// `library_busy` too. A caller therefore takes it once and either releases it or
/// hands it on; there is no "nested" acquire. The mutators follow one hand-off
/// rule: `render` takes the lock itself, so `on`/`off` (T2) and `remove` (T4)
/// drop their guard immediately before calling it, while `install`/`update` hand
/// their guard straight to `reconcile::run_locked`, which never re-takes it.
pub fn lock_library(paths: &Paths) -> Result<MutatorGuard> {
    if paths.pool_remote.is_some() && !paths.library.exists() {
        return Err(Error::config_error(
            "remote pool is absent; run `skillherd sync` to clone it before mutating or rendering",
        ));
    }
    let path = paths.library.join(".skill.lock");
    let file = crate::fsx::open_lock_file(&path)
        .map_err(|err| crate::paths::metadata_error(&path, &err))?;
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(MutatorGuard { file, path }),
        // flock reports contention as EWOULDBLOCK, which maps to WouldBlock.
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Err(Error::library_busy(&path)),
        Err(err) => Err(Error::io(&format!("locking {}", path.display()), &err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn the_second_mutator_is_busy_until_the_first_releases() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());

        let guard = lock_library(&paths).unwrap();
        assert_eq!(guard.lock_path(), paths.library.join(".skill.lock"));
        assert!(paths.library.join(".skill.lock").is_file());

        // The lock is not reentrant: a second acquire in the same process is
        // refused, so a caller that needs it for a nested step hands the guard
        // on (or releases it) rather than locking again.
        let busy = lock_library(&paths).unwrap_err();
        assert_eq!(busy.code(), "library_busy");
        assert_eq!(busy.exit_code(), 75);
        assert!(busy.retryable());
        assert!(busy.to_string().contains(".skill.lock"));
        assert_eq!(
            guard.lock_path(),
            paths.library.join(".skill.lock"),
            "the refused acquire left the held guard alone"
        );

        drop(guard);
        let again = lock_library(&paths).expect("the lock is free after the guard drops");
        drop(again);
    }

    #[test]
    fn locking_accepts_a_linked_library_but_refuses_a_linked_lock() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        std::fs::create_dir_all(&paths.library).unwrap();
        let target = dir.path().join("user-file");
        std::fs::write(&target, "user data\n").unwrap();
        let lock = paths.library.join(".skill.lock");
        std::os::unix::fs::symlink(&target, &lock).unwrap();
        assert!(
            std::fs::symlink_metadata(&lock)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(lock_library(&paths).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "user data\n");

        std::fs::remove_file(&lock).unwrap();
        assert!(!lock.exists());
        let mut alias = paths.clone();
        alias.library = dir.path().join("alias");
        std::os::unix::fs::symlink(&paths.library, &alias.library).unwrap();
        // Paths::resolve registers operator roots; a hand-edited fixture must too.
        crate::fsx::trust_paths(&alias).unwrap();
        let guard = lock_library(&alias).expect("an operator-chosen library root may be a symlink");
        drop(guard);

        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let owned = paths.library.join(".globals");
        std::os::unix::fs::symlink(&outside, &owned).unwrap();
        assert!(
            std::fs::symlink_metadata(&owned)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let error = crate::fsx::atomic_write(&owned.join("sets"), b"must not escape").unwrap_err();
        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(!outside.join("sets").exists());
    }

    #[test]
    fn locking_creates_a_missing_library() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        assert!(!paths.library.exists());
        let _guard = lock_library(&paths).unwrap();
        assert!(paths.library.is_dir());
    }
}
