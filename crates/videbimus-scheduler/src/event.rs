//! The scheduler's event stream (ADR 0007 §1): everything the logs, the
//! status file and the live tree need, so they can be built without
//! changing the scheduler.
//!
//! For each task, events arrive in this order:
//!
//! ```text
//! planned → key-resolved → cached                                   (hit)
//! planned → key-resolved → begun → (queued → started → progress*
//!           → chunk-committed | failed | cancelled)* → finished | failed | partial
//! planned → blocked                                       (an input failed)
//! planned → not-needed                     (a cached dependant didn't need it)
//! ```
//!
//! Chunks of one task may interleave. Every event is serialisable, one JSON
//! object per line, tagged by `event`.

use std::path::PathBuf;
use std::sync::{Mutex, mpsc};

use serde::{Serialize, Serializer};
use videbimus_core::AssetKey;
use videbimus_store::{ChunkId, Cutoff, Shape, Why, key_hex};

use crate::profile::Profile;
use crate::task::Resources;

/// Which task an event is about, with enough to place it in the tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TaskRef {
    /// Its index in the graph.
    pub id: usize,
    /// Its asset name (`s07/blender`).
    pub name: String,
    /// The scene path it sits under, if given (`scenes/s07`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// One thing that happened, and when.
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    /// When, in Unix milliseconds.
    pub at_ms: u64,
    /// What.
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Whether a rebuilt output equals the previous one (ADR 0003 §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EarlyCutoff {
    /// The same as the previous build: dependants stay cached.
    Same,
    /// Different.
    Different,
    /// No previous complete build to compare with.
    NoPrevious,
    /// Not hashed.
    NotHashed,
}

impl From<&Cutoff> for EarlyCutoff {
    fn from(c: &Cutoff) -> Self {
        match c {
            Cutoff::Same { .. } => EarlyCutoff::Same,
            Cutoff::Different { .. } => EarlyCutoff::Different,
            Cutoff::NoPrevious => EarlyCutoff::NoPrevious,
            Cutoff::NotHashed => EarlyCutoff::NotHashed,
        }
    }
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// Every target is built (or, for a partial target, its frames are).
    Complete,
    /// Some task failed; everything not blocked by it was built.
    Failed,
    /// Stopped on request (ADR 0008 §2); building again resumes.
    Stopped,
}

/// What happened.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "kebab-case", tag = "event")]
pub enum EventKind {
    /// A run began.
    RunStarted {
        /// The machine running the scheduler.
        machine: String,
        /// The profile in force.
        profile: Profile,
        /// The requested outputs' names.
        targets: Vec<String>,
    },
    /// A task is part of this run (whether or not it will need building).
    Planned {
        /// The task.
        task: TaskRef,
        /// Its inputs' task names.
        inputs: Vec<String>,
        /// Its output's shape.
        #[serde(flatten)]
        shape: Shape,
        /// Its estimated seconds for the whole output.
        estimate_s: f64,
        /// Its resources (memory from history where there is some).
        resources: Resources,
        /// Whether the estimate comes from measured history.
        from_history: bool,
    },
    /// A task's key is known (for a lazy key, once its inputs are built).
    KeyResolved {
        /// The task.
        task: TaskRef,
        /// Its key.
        #[serde(serialize_with = "ser_key")]
        key: AssetKey,
    },
    /// A task's key is complete in the store: nothing to do.
    Cached {
        /// The task.
        task: TaskRef,
        /// Its key.
        #[serde(serialize_with = "ser_key")]
        key: AssetKey,
    },
    /// A task's asset was begun (or resumed): why, and how much is left.
    Begun {
        /// The task.
        task: TaskRef,
        /// Its key.
        #[serde(serialize_with = "ser_key")]
        key: AssetKey,
        /// Why it is being built (ADR 0003 §7).
        why: Why,
        /// Chunks queued for this run.
        chunks: usize,
        /// Frames already committed under this key.
        done_frames: u64,
        /// Frames in total (0 for a single output).
        total_frames: u64,
        /// Its task log.
        log: PathBuf,
    },
    /// A chunk is waiting for budget.
    Queued {
        /// The task.
        task: TaskRef,
        /// The chunk.
        #[serde(serialize_with = "ser_chunk")]
        chunk: ChunkId,
        /// Its estimated seconds.
        estimate_s: f64,
    },
    /// A chunk started.
    Started {
        /// The task.
        task: TaskRef,
        /// The chunk.
        #[serde(serialize_with = "ser_chunk")]
        chunk: ChunkId,
        /// Where it runs.
        machine: String,
        /// Cores granted.
        cores: u32,
        /// Its subprocesses' niceness.
        niceness: i32,
        /// Whether it runs alone because it is bigger than the budget.
        alone: bool,
    },
    /// A chunk reported progress.
    Progress {
        /// The task.
        task: TaskRef,
        /// The chunk.
        #[serde(serialize_with = "ser_chunk")]
        chunk: ChunkId,
        /// Units done (frames, usually).
        done: u64,
        /// Units in the chunk.
        total: u64,
    },
    /// A chunk was committed to the store.
    ChunkCommitted {
        /// The task.
        task: TaskRef,
        /// The chunk.
        #[serde(serialize_with = "ser_chunk")]
        chunk: ChunkId,
        /// How long it took, in milliseconds.
        ms: u64,
        /// Its peak memory, if measured.
        peak_memory_bytes: Option<u64>,
    },
    /// A task's asset is complete.
    Finished {
        /// The task.
        task: TaskRef,
        /// Its key.
        #[serde(serialize_with = "ser_key")]
        key: AssetKey,
        /// The asset's total build time, all runs.
        build_ms: Option<u64>,
        /// Its peak memory.
        peak_memory_bytes: Option<u64>,
        /// Whether its output equals the previous build's.
        cutoff: EarlyCutoff,
    },
    /// A partial (preview) target's frames are built; the asset isn't.
    Partial {
        /// The task.
        task: TaskRef,
        /// Its key.
        #[serde(serialize_with = "ser_key")]
        key: AssetKey,
    },
    /// A chunk, or the task as a whole, failed.
    Failed {
        /// The task.
        task: TaskRef,
        /// The chunk, if a chunk failed.
        #[serde(serialize_with = "ser_opt_chunk")]
        chunk: Option<ChunkId>,
        /// What failed.
        message: String,
        /// The last lines of output (ADR 0007 §5).
        last_lines: Vec<String>,
        /// Its task log, if it got that far.
        log: Option<PathBuf>,
    },
    /// A running chunk was cancelled and discarded.
    Cancelled {
        /// The task.
        task: TaskRef,
        /// The chunk.
        #[serde(serialize_with = "ser_chunk")]
        chunk: ChunkId,
    },
    /// A task can't run because an input failed.
    Blocked {
        /// The task.
        task: TaskRef,
        /// The failed (or blocked) input's name.
        by: String,
    },
    /// A task wasn't needed: everything that reads it is cached.
    NotNeeded {
        /// The task.
        task: TaskRef,
    },
    /// The profile changed; tasks started from now on use it.
    ProfileChanged {
        /// The new profile.
        profile: Profile,
    },
    /// A preview request jumped the queue.
    Preview {
        /// Its targets' names.
        targets: Vec<String>,
    },
    /// Something worth telling the user.
    Warning {
        /// The task, if about one.
        #[serde(skip_serializing_if = "Option::is_none")]
        task: Option<TaskRef>,
        /// What.
        message: String,
    },
    /// A stop was requested: running chunks are being cancelled.
    StopRequested,
    /// The run ended.
    RunFinished {
        /// How.
        outcome: Outcome,
        /// Tasks built.
        built: usize,
        /// Tasks cached.
        cached: usize,
        /// Tasks failed.
        failed: usize,
        /// Chunks committed.
        chunks: usize,
        /// Wall time, in milliseconds.
        ms: u64,
    },
}

impl EventKind {
    /// The task it is about, if any.
    #[must_use]
    pub fn task(&self) -> Option<&TaskRef> {
        match self {
            EventKind::Planned { task, .. }
            | EventKind::KeyResolved { task, .. }
            | EventKind::Cached { task, .. }
            | EventKind::Begun { task, .. }
            | EventKind::Queued { task, .. }
            | EventKind::Started { task, .. }
            | EventKind::Progress { task, .. }
            | EventKind::ChunkCommitted { task, .. }
            | EventKind::Finished { task, .. }
            | EventKind::Partial { task, .. }
            | EventKind::Failed { task, .. }
            | EventKind::Cancelled { task, .. }
            | EventKind::Blocked { task, .. }
            | EventKind::NotNeeded { task } => Some(task),
            EventKind::Warning { task, .. } => task.as_ref(),
            _ => None,
        }
    }

    /// Its `event` tag, as serialised.
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            EventKind::RunStarted { .. } => "run-started",
            EventKind::Planned { .. } => "planned",
            EventKind::KeyResolved { .. } => "key-resolved",
            EventKind::Cached { .. } => "cached",
            EventKind::Begun { .. } => "begun",
            EventKind::Queued { .. } => "queued",
            EventKind::Started { .. } => "started",
            EventKind::Progress { .. } => "progress",
            EventKind::ChunkCommitted { .. } => "chunk-committed",
            EventKind::Finished { .. } => "finished",
            EventKind::Partial { .. } => "partial",
            EventKind::Failed { .. } => "failed",
            EventKind::Cancelled { .. } => "cancelled",
            EventKind::Blocked { .. } => "blocked",
            EventKind::NotNeeded { .. } => "not-needed",
            EventKind::ProfileChanged { .. } => "profile-changed",
            EventKind::Preview { .. } => "preview",
            EventKind::Warning { .. } => "warning",
            EventKind::StopRequested => "stop-requested",
            EventKind::RunFinished { .. } => "run-finished",
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn ser_key<S: Serializer>(key: &AssetKey, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&key_hex(key))
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn ser_chunk<S: Serializer>(chunk: &ChunkId, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&chunk.dir_name())
}

#[allow(clippy::ref_option)]
fn ser_opt_chunk<S: Serializer>(chunk: &Option<ChunkId>, s: S) -> Result<S::Ok, S::Error> {
    match chunk {
        Some(c) => s.serialize_str(&c.dir_name()),
        None => s.serialize_none(),
    }
}

/// Where events go. Called from the scheduler's thread and from task
/// threads (progress), so it must be quick and must not block for long.
pub trait EventSink: Send + Sync {
    /// Receives one event.
    fn event(&self, event: &Event);
}

impl EventSink for mpsc::Sender<Event> {
    fn event(&self, event: &Event) {
        let _ = self.send(event.clone());
    }
}

impl<F: Fn(&Event) + Send + Sync> EventSink for F {
    fn event(&self, event: &Event) {
        self(event);
    }
}

/// Discards events.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoEvents;

impl EventSink for NoEvents {
    fn event(&self, _: &Event) {}
}

/// Keeps every event, for tests and short runs.
#[derive(Debug, Default)]
pub struct Collect(Mutex<Vec<Event>>);

impl Collect {
    /// An empty collector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The events so far.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Forgets them.
    pub fn clear(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

impl EventSink for Collect {
    fn event(&self, event: &Event) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone());
    }
}
