//! What a running task sees: [`TaskContext`].

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use videbimus_core::{AssetKey, Interval};
use videbimus_store::{ChunkId, Store};

use crate::event::{Event, EventKind, EventSink, TaskRef};
use crate::now_ms;
use crate::subprocess::{self, CommandOutput};
use crate::task::TaskError;

/// A flag a task polls to learn it has been cancelled. Cheap to clone;
/// clones share the flag.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A token not yet cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels it.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether it was cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A task's log file in the store (ADR 0007 §1), shared by its chunks.
/// Each line is written whole, so concurrent chunks' lines never mix.
#[derive(Debug)]
pub struct TaskLog {
    path: PathBuf,
    file: Mutex<Option<File>>,
}

impl TaskLog {
    pub(crate) fn open(path: PathBuf) -> Self {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        Self {
            path,
            file: Mutex::new(file),
        }
    }

    /// Its path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one line (a newline is added). Best effort: a log that
    /// can't be written never fails the build.
    pub fn line(&self, line: &str) {
        let mut guard = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(f) = guard.as_mut() {
            let mut buf = Vec::with_capacity(line.len() + 1);
            buf.extend_from_slice(line.as_bytes());
            buf.push(b'\n');
            let _ = f.write_all(&buf);
        }
    }
}

/// Everything a task's [`run`](crate::Task::run) gets: which chunk to
/// build and where, the cores it was granted, its log, progress
/// reporting, cancellation, and a subprocess helper that applies the
/// profile's niceness and thread caps.
pub struct TaskContext<'a> {
    pub(crate) task: &'a TaskRef,
    pub(crate) store: &'a Store,
    pub(crate) key: AssetKey,
    pub(crate) chunk: ChunkId,
    pub(crate) out_dir: PathBuf,
    pub(crate) cores: u32,
    pub(crate) niceness: i32,
    pub(crate) cancel: &'a CancelToken,
    pub(crate) log: &'a Arc<TaskLog>,
    pub(crate) sink: &'a dyn EventSink,
    pub(crate) inputs: &'a [(String, AssetKey)],
    pub(crate) kill_grace: Duration,
    pub(crate) peak_memory: AtomicU64,
}

impl TaskContext<'_> {
    /// The task's asset name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.task.name
    }

    /// The asset's key.
    #[must_use]
    pub fn key(&self) -> AssetKey {
        self.key
    }

    /// The chunk to build.
    #[must_use]
    pub fn chunk(&self) -> ChunkId {
        self.chunk
    }

    /// The chunk's frames, for a frame sequence.
    #[must_use]
    pub fn frames(&self) -> Option<Interval> {
        self.chunk.frames()
    }

    /// Where to write the chunk's output. It is committed whole when
    /// [`run`](crate::Task::run) returns `Ok`.
    #[must_use]
    pub fn out_dir(&self) -> &Path {
        &self.out_dir
    }

    /// Cores granted: the task's declared cores, capped by the budget.
    /// Pass it on to tools (Blender `-t`).
    #[must_use]
    pub fn cores(&self) -> u32 {
        self.cores
    }

    /// The niceness its subprocesses run at.
    #[must_use]
    pub fn niceness(&self) -> i32 {
        self.niceness
    }

    /// The store, for reading inputs.
    #[must_use]
    pub fn store(&self) -> &Store {
        self.store
    }

    /// The key of the input labelled `label`, to read from the store.
    #[must_use]
    pub fn input(&self, label: &str) -> Option<AssetKey> {
        self.inputs
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, k)| *k)
    }

    /// The output directories of the input labelled `label`'s committed
    /// chunks, in frame order.
    ///
    /// # Errors
    ///
    /// An unknown label, or the store's I/O.
    pub fn input_dirs(&self, label: &str) -> Result<Vec<PathBuf>, TaskError> {
        let key = self
            .input(label)
            .ok_or_else(|| TaskError::new(format!("no input labelled `{label}`")))?;
        Ok(self
            .store
            .committed(key)?
            .iter()
            .map(|c| self.store.chunk_dir(key, c.id()))
            .collect())
    }

    /// The cancellation token.
    #[must_use]
    pub fn cancel_token(&self) -> &CancelToken {
        self.cancel
    }

    /// Whether the run is stopping. Long-running tasks poll this and
    /// return [`TaskError::cancelled`].
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// `Err(TaskError::cancelled())` if cancelled.
    ///
    /// # Errors
    ///
    /// If cancelled.
    pub fn check_cancelled(&self) -> Result<(), TaskError> {
        if self.is_cancelled() {
            Err(TaskError::cancelled())
        } else {
            Ok(())
        }
    }

    /// The task's log, in the store.
    #[must_use]
    pub fn log_path(&self) -> &Path {
        self.log.path()
    }

    /// Appends a line to the task's log.
    pub fn log(&self, line: &str) {
        self.log
            .line(&format!("[{}] {line}", self.chunk.dir_name()));
    }

    /// Reports progress through the chunk: `done` of `total` (frames,
    /// usually).
    pub fn progress(&self, done: u64, total: u64) {
        self.sink.event(&Event {
            at_ms: now_ms(),
            kind: EventKind::Progress {
                task: self.task.clone(),
                chunk: self.chunk,
                done,
                total,
            },
        });
    }

    /// Records peak memory the task measured itself, in bytes.
    pub fn note_peak_memory(&self, bytes: u64) {
        self.peak_memory.fetch_max(bytes, Ordering::Relaxed);
    }

    /// The highest peak memory noted so far, by the task or its
    /// subprocesses.
    #[must_use]
    pub fn peak_memory(&self) -> Option<u64> {
        match self.peak_memory.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n),
        }
    }

    /// Runs a subprocess under the task's control (ADR 0004 §3, ADR 0007
    /// §1, ADR 0008 §2):
    ///
    /// - at the profile's niceness, with thread caps (`OMP_NUM_THREADS`
    ///   and friends, plus `VIDEBIMUS_CORES`) set to the granted cores;
    /// - in its own process group, so stopping kills it and everything it
    ///   started: `SIGTERM` to the group, then `SIGKILL` after a grace
    ///   period;
    /// - with its full stdout and stderr streamed into the task log;
    /// - recording its peak memory (`wait4`'s max RSS: the child, and the
    ///   descendants it waited for).
    ///
    /// Its stdin is null. The command line is logged first.
    ///
    /// # Errors
    ///
    /// If it can't be spawned, exits unsuccessfully (with its last lines
    /// of output), or is cancelled ([`TaskError::is_cancelled`]).
    pub fn command(&self, cmd: &mut Command) -> Result<CommandOutput, TaskError> {
        subprocess::run(self, cmd)
    }
}
