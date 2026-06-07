//! Advisory file locking. The one place the `flock` primitive lives, so every
//! cross-process lock — the Chrome launch lock, the policy store, the context
//! store — shares a single audited implementation instead of re-deriving the
//! `unsafe` call and the open-options incantation at each site.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;

/// Open `path` (creating it and its parent directory) and take an exclusive
/// advisory `flock`. The lock is held until the returned [`File`] is dropped.
///
/// With `nonblocking`, returns `Ok(None)` when another holder already owns the
/// lock, rather than waiting; a blocking call always returns `Ok(Some(_))` or
/// an error. Callers that block can therefore `.expect()` the `Some`.
pub fn flock_exclusive(path: &Path, nonblocking: bool) -> std::io::Result<Option<File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;

    let mut op = libc::LOCK_EX;
    if nonblocking {
        op |= libc::LOCK_NB;
    }
    // SAFETY: flock() is a POSIX advisory lock; no memory-safety implications.
    if unsafe { libc::flock(file.as_raw_fd(), op) } != 0 {
        let err = std::io::Error::last_os_error();
        // A non-blocking lock that is already held is `EWOULDBLOCK`, the one
        // non-error outcome — report it as "not acquired", not a failure.
        if nonblocking && err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            return Ok(None);
        }
        return Err(err);
    }
    Ok(Some(file))
}
