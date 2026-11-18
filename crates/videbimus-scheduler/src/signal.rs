//! Ctrl-C as a graceful stop (ADR 0008 §2), kept minimal.
//!
//! [`stop_on_sigint`] installs a `SIGINT` handler. The first Ctrl-C sets a
//! flag that every running [`Scheduler`](crate::Scheduler) polls: it
//! cancels running tasks, kills their subprocesses and returns
//! [`Outcome::Stopped`](crate::Outcome::Stopped). A second Ctrl-C, for a
//! task that won't stop, kills every subprocess group the scheduler
//! started and then lets the default action end the process. Stopping
//! either way is safe, because nothing half-written is ever committed.
//!
//! Subprocesses run in their own process groups (so the terminal's Ctrl-C
//! reaches only videbimus, which then stops them in order). The handler
//! knows their groups through a fixed table of [`MAX_GROUPS`] slots; a
//! group beyond that is still killed on a graceful stop, but not by a
//! second Ctrl-C.
//!
//! On Windows, [`stop_on_sigint`] returns an `Unsupported` error: call
//! [`Control::stop`](crate::Control::stop) from your own handler.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Subprocess groups the second-Ctrl-C handler can kill.
pub const MAX_GROUPS: usize = 256;

static SIGINT: AtomicBool = AtomicBool::new(false);
static GROUPS: [AtomicI32; MAX_GROUPS] = [const { AtomicI32::new(0) }; MAX_GROUPS];

/// Whether a Ctrl-C has asked for a stop that hasn't yet been handled.
#[must_use]
pub fn sigint_received() -> bool {
    SIGINT.load(Ordering::SeqCst)
}

pub(crate) fn clear_sigint() {
    SIGINT.store(false, Ordering::SeqCst);
}

#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn register_group(pid: i32) -> Option<usize> {
    GROUPS.iter().position(|slot| {
        slot.compare_exchange(0, pid, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    })
}

#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn unregister_group(slot: Option<usize>) {
    if let Some(i) = slot {
        GROUPS[i].store(0, Ordering::SeqCst);
    }
}

#[cfg(unix)]
extern "C" fn on_sigint(_: libc::c_int) {
    // Only atomics, kill, signal and raise: all async-signal-safe.
    if SIGINT.swap(true, Ordering::SeqCst) {
        for slot in &GROUPS {
            let pid = slot.load(Ordering::SeqCst);
            if pid > 0 {
                // SAFETY: kill is async-signal-safe.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
        }
        // SAFETY: signal and raise are async-signal-safe.
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::raise(libc::SIGINT);
        }
    }
}

/// Makes Ctrl-C stop running builds gracefully; a second Ctrl-C kills
/// their subprocesses and the process. Process-wide; call it once, from a
/// binary's `main` (never from a library or a test).
///
/// # Errors
///
/// If the handler can't be installed, or on non-Unix platforms.
pub fn stop_on_sigint() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let handler = on_sigint as extern "C" fn(libc::c_int);
        // SAFETY: the handler only does async-signal-safe things.
        let previous = unsafe { libc::signal(libc::SIGINT, handler as libc::sighandler_t) };
        if previous == libc::SIG_ERR {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Ctrl-C handling is Unix-only; call Control::stop from your own handler",
        ))
    }
}
