//! Running a task's subprocesses: niceness, thread caps, process groups,
//! full output into the task log, kill on cancel, peak memory.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::context::{TaskContext, TaskLog};
use crate::task::TaskError;

/// Lines of output kept for a failure message (ADR 0007 §5).
const TAIL_LINES: usize = 20;

/// Environment variables set to the granted cores, so common runtimes
/// don't spawn a thread per machine core.
pub const THREAD_CAP_VARS: &[&str] = &[
    "OMP_NUM_THREADS",
    "MKL_NUM_THREADS",
    "OPENBLAS_NUM_THREADS",
    "VECLIB_MAXIMUM_THREADS",
    "NUMEXPR_NUM_THREADS",
    "RAYON_NUM_THREADS",
    "VIDEBIMUS_CORES",
];

/// How a subprocess ended.
#[derive(Clone, Debug)]
pub struct CommandOutput {
    /// Its exit status.
    pub status: ExitStatus,
    /// Its peak resident memory, in bytes, where measurable.
    pub peak_memory_bytes: Option<u64>,
    /// Its last lines of output (stdout and stderr interleaved as read).
    pub last_lines: Vec<String>,
}

type Tail = Arc<Mutex<VecDeque<String>>>;

fn pump(stream: impl Read, log: &TaskLog, prefix: &str, tail: &Tail) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&buf);
                let text = text.trim_end_matches(['\n', '\r']);
                log.line(&format!("{prefix}{text}"));
                let mut t = tail
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if t.len() == TAIL_LINES {
                    t.pop_front();
                }
                t.push_back(text.to_owned());
            }
        }
    }
}

pub(crate) fn run(ctx: &TaskContext<'_>, cmd: &mut Command) -> Result<CommandOutput, TaskError> {
    ctx.check_cancelled()?;
    let cores = ctx.cores().to_string();
    for var in THREAD_CAP_VARS {
        cmd.env(var, &cores);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    platform::prepare(cmd, ctx.niceness());
    let shown = std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|a| format!("{:?}", a.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    ctx.log(&format!("$ {shown}"));
    ctx.log(&format!(
        "  (nice {}, thread caps {cores}, own process group)",
        ctx.niceness()
    ));
    let mut child = cmd
        .spawn()
        .map_err(|e| TaskError::new(format!("couldn't start {shown}: {e}")))?;

    let log = ctx.log.clone();
    let chunk = ctx.chunk().dir_name();
    let tail: Tail = Arc::new(Mutex::new(VecDeque::new()));
    let mut pumps = Vec::new();
    if let Some(out) = child.stdout.take() {
        let (log, tail, p) = (log.clone(), tail.clone(), format!("[{chunk}] | "));
        pumps.push(thread::spawn(move || pump(out, &log, &p, &tail)));
    }
    if let Some(err) = child.stderr.take() {
        let (log, tail, p) = (log.clone(), tail.clone(), format!("[{chunk}] ! "));
        pumps.push(thread::spawn(move || pump(err, &log, &p, &tail)));
    }

    let waited = platform::wait(child, ctx.cancel_token(), ctx.kill_grace);
    for p in pumps {
        let _ = p.join();
    }
    let last_lines: Vec<String> = tail
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .cloned()
        .collect();
    let (status, peak) = waited.map_err(|e| TaskError::new(format!("waiting for {shown}: {e}")))?;
    if let Some(p) = peak {
        ctx.note_peak_memory(p);
    }
    ctx.log(&format!(
        "exit: {status}{}",
        peak.map_or(String::new(), |p| format!(", peak memory {p} bytes"))
    ));
    if ctx.is_cancelled() {
        return Err(TaskError::cancelled());
    }
    if !status.success() {
        return Err(TaskError::new(format!("{shown} failed: {status}")).with_last_lines(last_lines));
    }
    Ok(CommandOutput {
        status,
        peak_memory_bytes: peak,
        last_lines,
    })
}

#[cfg(unix)]
mod platform {
    use std::io;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Child, Command, ExitStatus};
    use std::time::{Duration, Instant};

    use super::{CancelToken, sleep_poll};
    use crate::signal;

    pub(super) fn prepare(cmd: &mut Command, niceness: i32) {
        cmd.process_group(0);
        // Never lower the niceness we already run at: that needs privilege.
        // SAFETY: getpriority has no preconditions.
        // (-1 is also getpriority's error value; either way it is a
        // harmless floor.)
        let current = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
        let target = niceness.max(current);
        // SAFETY: the closure runs between fork and exec and only makes
        // the async-signal-safe setpriority call; its failure is ignored.
        unsafe {
            cmd.pre_exec(move || {
                libc::setpriority(libc::PRIO_PROCESS, 0, target);
                Ok(())
            });
        }
    }

    fn kill_group(pid: i32, sig: i32) {
        // SAFETY: kill has no memory preconditions; a gone group is ESRCH.
        unsafe {
            libc::kill(-pid, sig);
        }
    }

    pub(super) fn wait(
        child: Child,
        cancel: &CancelToken,
        grace: Duration,
    ) -> io::Result<(ExitStatus, Option<u64>)> {
        let pid = i32::try_from(child.id()).map_err(io::Error::other)?;
        // We reap the child ourselves with wait4, for its rusage; `Child`
        // is dropped without waiting.
        drop(child);
        let slot = signal::register_group(pid);
        let mut term_at: Option<Instant> = None;
        let mut killed = false;
        let result = loop {
            let mut status = 0;
            // SAFETY: an all-zero rusage is a valid value to be overwritten.
            let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
            // SAFETY: status and usage are valid for writes.
            let r = unsafe { libc::wait4(pid, &raw mut status, libc::WNOHANG, &raw mut usage) };
            if r == pid {
                break Ok((ExitStatus::from_raw(status), maxrss_bytes(&usage)));
            }
            if r == -1 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break Err(e);
            }
            if cancel.is_cancelled() {
                match term_at {
                    None => {
                        kill_group(pid, libc::SIGTERM);
                        term_at = Some(Instant::now());
                    }
                    Some(t) if !killed && t.elapsed() >= grace => {
                        kill_group(pid, libc::SIGKILL);
                        killed = true;
                    }
                    Some(_) => {}
                }
            }
            sleep_poll();
        };
        // Strays left in the group (a backgrounded grandchild) would hold
        // the output pipes open and outlive the task: kill them.
        kill_group(pid, libc::SIGKILL);
        signal::unregister_group(slot);
        result
    }

    fn maxrss_bytes(usage: &libc::rusage) -> Option<u64> {
        let raw = u64::try_from(usage.ru_maxrss).ok().filter(|&n| n > 0)?;
        // macOS reports bytes; Linux and the BSDs report KiB.
        if cfg!(target_vendor = "apple") {
            Some(raw)
        } else {
            Some(raw.saturating_mul(1024))
        }
    }
}

#[cfg(not(unix))]
mod platform {
    use std::io;
    use std::process::{Child, Command, ExitStatus};
    use std::time::Duration;

    use super::{CancelToken, sleep_poll};

    pub(super) fn prepare(_cmd: &mut Command, _niceness: i32) {}

    /// Windows: no niceness, no process group (only the child is killed),
    /// no peak memory.
    pub(super) fn wait(
        mut child: Child,
        cancel: &CancelToken,
        _grace: Duration,
    ) -> io::Result<(ExitStatus, Option<u64>)> {
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok((status, None));
            }
            if cancel.is_cancelled() {
                let _ = child.kill();
                return child.wait().map(|s| (s, None));
            }
            sleep_poll();
        }
    }
}

use crate::context::CancelToken;

fn sleep_poll() {
    thread::sleep(Duration::from_millis(5));
}
