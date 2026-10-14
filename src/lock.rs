//! Cross-process locking, so two dv runs cannot write the same state at once.
//!
//! An editor's language server and a terminal can both decide to install
//! 10.0.402 within a second of each other. They would share a staging
//! directory and a cache entry, and the loser would extract into a tree the
//! winner was already renaming — corruption that looks like a bad download.
//!
//! The lock is an advisory lock on a file, taken with the standard library's
//! own file locking. A kernel lock is used rather than a pid file because it is
//! released when the process dies, however it dies: a lock left by `kill -9`
//! disappears on its own, where a stale pid file would wedge every later
//! install until someone deleted it by hand.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};

use crate::exit::{self, Failed};

/// Held for as long as the work runs; released when dropped.
#[derive(Debug)]
pub struct Lock {
    _file: File,
}

/// Take the lock for `name`, or explain who has it.
///
/// Non-blocking on purpose: waiting silently on another process looks like a
/// hang, and the honest report — someone else is already doing this — is
/// something the user can act on.
pub fn acquire(directory: &Path, name: &str, activity: &str) -> Result<Lock> {
    std::fs::create_dir_all(directory)
        .with_context(|| format!("could not create {}", directory.display()))?;
    let path = directory.join(format!(".{name}.lock"));

    let file = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("could not open {}", path.display()))?;

    match file.try_lock() {
        Ok(()) => Ok(Lock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(Failed::new(
            exit::BUSY,
            format!(
                "another dv process is already {activity} — wait for it to finish, \
                 then try again"
            ),
        )),
        Err(std::fs::TryLockError::Error(err)) => {
            Err(err).with_context(|| format!("could not lock {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_is_refused_and_told_why() {
        let dir = tempfile::tempdir().unwrap();
        let held = acquire(dir.path(), "10.0.402", "installing .NET SDK 10.0.402").unwrap();

        let error = acquire(dir.path(), "10.0.402", "installing .NET SDK 10.0.402")
            .unwrap_err()
            .to_string();
        assert!(error.contains("installing .NET SDK 10.0.402"), "{error}");

        // A different version is unrelated work and must not be blocked.
        assert!(acquire(dir.path(), "8.0.426", "installing .NET SDK 8.0.426").is_ok());

        drop(held);
        assert!(acquire(dir.path(), "10.0.402", "installing .NET SDK 10.0.402").is_ok());
    }
}
