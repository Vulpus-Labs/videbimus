//! One writer per store (ADR 0008 §6).
//!
//! The lock is a file, `<store>/lock`, holding the writer's pid, host and
//! start time as JSON. It is staged under a unique name and hard-linked into
//! place, so it appears with its content in one step and never half-written.
//! A lock whose process is gone on this host is stale: it is cleared, and
//! the caller gets a [`Warning`] to show. A lock held on another host can't
//! be checked, so it is an error.
//!
//! Caveats:
//! - pids are reused. A stale lock whose pid now belongs to an unrelated
//!   process reads as live; the error names the pid so the user can check
//!   and delete the lock.
//! - Two processes clearing the same stale lock at the same instant can
//!   race; both may then believe they hold it. Starting two builds on one
//!   store within milliseconds of a crash is not a case worth more
//!   machinery.
//! - On Windows, liveness isn't checked (every lock reads as live) and hard
//!   links need NTFS. Unix (macOS, Linux) is the supported target.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, io_err};
use crate::fsutil::{now_ms, write_synced};

/// Who holds a store's lock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockHolder {
    /// Its process id.
    pub pid: u32,
    /// Its machine's host name.
    pub host: String,
    /// When it took the lock, in Unix milliseconds.
    pub started_ms: u64,
}

impl fmt::Display for LockHolder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pid {} on {}, since {}",
            self.pid,
            self.host,
            fmt_utc(self.started_ms)
        )
    }
}

/// Something the caller should tell the user about; the store has no
/// logging of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Warning {
    /// A lock left by a process that is gone was cleared.
    StaleLockCleared {
        /// Who held it.
        holder: LockHolder,
    },
    /// Temporaries from an interrupted build were deleted (ADR 0008 §1).
    TemporariesRemoved {
        /// How many.
        count: usize,
    },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::StaleLockCleared { holder } => write!(
                f,
                "cleared a stale store lock held by {holder}: that process is gone"
            ),
            Warning::TemporariesRemoved { count } => write!(
                f,
                "deleted {count} part-written temporar{} from an interrupted build",
                if *count == 1 { "y" } else { "ies" }
            ),
        }
    }
}

/// A held lock, released on drop.
#[derive(Debug)]
pub(crate) struct Lock {
    path: PathBuf,
    holder: LockHolder,
}

impl Lock {
    pub(crate) fn acquire(root: &Path) -> Result<(Lock, Option<Warning>)> {
        let path = root.join("lock");
        let me = LockHolder {
            pid: std::process::id(),
            host: machine_name(),
            started_ms: now_ms(),
        };
        let staged = root.join(format!(
            "lock.{}.{}.staged",
            me.pid,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let json = serde_json::to_vec_pretty(&me).expect("a lock holder serialises");
        write_synced(&staged, &json)?;
        let result = Self::link(&path, &staged, &me);
        let _ = fs::remove_file(&staged);
        result.map(|warning| (Lock { path, holder: me }, warning))
    }

    fn link(path: &Path, staged: &Path, me: &LockHolder) -> Result<Option<Warning>> {
        let mut warning = None;
        for _ in 0..3 {
            match fs::hard_link(staged, path) {
                Ok(()) => return Ok(warning),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io_err(path)(e)),
            }
            let holder = match read_holder(path) {
                Ok(Some(h)) => h,
                Ok(None) => continue, // released between our link and read
                Err(e) => return Err(e),
            };
            if holder.host != me.host {
                return Err(Error::Locked {
                    path: path.to_owned(),
                    holder,
                    other_host: true,
                });
            }
            if process_alive(holder.pid) {
                return Err(Error::Locked {
                    path: path.to_owned(),
                    holder,
                    other_host: false,
                });
            }
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_err(path)(e)),
            }
            warning = Some(Warning::StaleLockCleared { holder });
        }
        Err(Error::Io {
            path: path.to_owned(),
            source: io::Error::other("the lock kept changing; is another build starting?"),
        })
    }
}

fn read_holder(path: &Path) -> Result<Option<LockHolder>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| Error::Corrupt {
                path: path.to_owned(),
                message: format!("not a store lock ({e})"),
            }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path)(e)),
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if let Ok(Some(h)) = read_holder(&self.path)
            && h == self.holder
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// This machine's host name, as recorded in locks and manifests.
#[must_use]
pub fn machine_name() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length; gethostname writes at
        // most that many bytes.
        let r = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
        if r == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            return String::from_utf8_lossy(&buf[..end]).into_owned();
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown-host".to_owned())
}

/// Whether process `pid` exists on this machine.
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists and may be
    // signalled; nothing is sent.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

/// `YYYY-MM-DD HH:MM:SS UTC` from Unix milliseconds.
fn fmt_utc(ms: u64) -> String {
    let secs = ms / 1000;
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc() {
        assert_eq!(super::fmt_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(super::fmt_utc(1_795_000_000_000), "2026-11-18 11:06:40 UTC");
    }
}
