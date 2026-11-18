//! What a schedulable unit of work is: the [`Task`] trait, and what it
//! declares (ADR 0004 §2).

use std::fmt;

use serde::Serialize;
use videbimus_store::{KeyBuilder, Shape};

use crate::context::TaskContext;

/// What a task needs while it runs. The scheduler packs tasks so that the
/// sum of running tasks' cores and memory stays within the profile's
/// budget (ADR 0004 §3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Resources {
    /// Cores it keeps busy. At least 1 is assumed.
    pub cores: u32,
    /// Its peak memory, in bytes. Replaced by measured history when there
    /// is some (ADR 0004 §2).
    pub memory_bytes: u64,
    /// Whether it wants the GPU. At most one GPU task runs at a time.
    pub gpu: bool,
}

impl Resources {
    /// `cores` cores and `memory_bytes` of memory, no GPU.
    #[must_use]
    pub fn new(cores: u32, memory_bytes: u64) -> Self {
        Self {
            cores,
            memory_bytes,
            gpu: false,
        }
    }

    /// The same, wanting the GPU.
    #[must_use]
    pub fn with_gpu(mut self) -> Self {
        self.gpu = true;
        self
    }

    pub(crate) fn cores_at_least_one(self) -> u32 {
        self.cores.max(1)
    }
}

/// A task's estimated cost, in seconds of wall time with its declared
/// cores (ADR 0004 §2). Replaced by measured history when there is some.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "cost", content = "seconds")]
pub enum Cost {
    /// The whole task, however its output is shaped.
    Seconds(f64),
    /// Per frame, for a frame sequence; for a single output, the whole.
    PerFrame(f64),
}

impl Cost {
    /// The whole task's estimated seconds for an output of `shape`.
    #[must_use]
    pub fn total_seconds(self, shape: Shape) -> f64 {
        match (self, shape) {
            (Cost::Seconds(s), _) | (Cost::PerFrame(s), Shape::Single) => s.max(0.0),
            #[allow(clippy::cast_precision_loss)]
            (Cost::PerFrame(s), Shape::Frames(iv)) => s.max(0.0) * iv.len().max(0) as f64,
        }
    }
}

/// One asset's recipe, as the scheduler runs it: the seam a recipe kind
/// (ticket 0010's `RecipeKind`) produces.
///
/// A task declares its name, output shape, resources, cost and key; the
/// scheduler decides whether it runs at all (cache hit or not), when,
/// with how many cores, and in which chunks. A frame-sequence task is
/// split into chunks of about two minutes' estimated work each
/// ([`SchedulerOptions::chunk_seconds`](crate::SchedulerOptions::chunk_seconds));
/// [`run`](Self::run) is called once per chunk, possibly concurrently,
/// and builds exactly [`TaskContext::chunk`]'s frames into
/// [`TaskContext::out_dir`]. The scheduler commits the chunk when `run`
/// returns `Ok`, and discards it otherwise (ADR 0008 §1).
///
/// Dependencies are edges in the [`Graph`](crate::Graph), each with a
/// label: the scheduler adds every input's identity to the key under its
/// label ([`KeyBuilder::input`]), so a task can't forget one. An input
/// that hashes its output ([`hash_output`](Self::hash_output)) only has an
/// identity once built (early cutoff, ADR 0003 §4), so a dependant's key
/// may be resolved late, after its inputs finish.
pub trait Task: Send + Sync + 'static {
    /// The asset's name: its stable slot in the scene (`s07/blender`).
    fn name(&self) -> &str;

    /// What it outputs: a single output or a frame range.
    fn shape(&self) -> Shape;

    /// What it needs while running.
    fn resources(&self) -> Resources;

    /// Its estimated cost.
    fn cost(&self) -> Cost;

    /// Its key, without its graph inputs: kind, code identity, config,
    /// sources, scripts and tools. The scheduler adds each graph input's
    /// identity under the input's label.
    fn key(&self) -> KeyBuilder;

    /// Whether to hash its output, for early cutoff (ADR 0003 §4): a
    /// rebuild whose output equals the previous one leaves dependants
    /// cached. Dependants' keys then wait until this task is built.
    fn hash_output(&self) -> bool {
        false
    }

    /// Frames per chunk, overriding the scheduler's choice from cost.
    fn chunk_frames(&self) -> Option<u32> {
        None
    }

    /// Its resolved config, as text, for its task log (ADR 0007 §1). The
    /// key holds only the config's digest; this is what a reader of the
    /// log needs to see. `None` logs nothing.
    fn config_text(&self) -> Option<String> {
        None
    }

    /// Builds one chunk: [`TaskContext::chunk`]'s frames (or the whole
    /// output) into [`TaskContext::out_dir`]. Poll
    /// [`TaskContext::is_cancelled`] in long loops; subprocesses run
    /// through [`TaskContext::command`] are killed on cancel.
    ///
    /// # Errors
    ///
    /// Any failure; the chunk is discarded and the asset marked failed.
    fn run(&self, ctx: &TaskContext<'_>) -> Result<(), TaskError>;
}

/// A task's failure: a message, and the last lines of output that explain
/// it (ADR 0007 §5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskError {
    /// What failed.
    pub message: String,
    /// The last lines of the failing command's output, if any.
    pub last_lines: Vec<String>,
    pub(crate) cancelled: bool,
}

impl TaskError {
    /// A failure with `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            last_lines: Vec::new(),
            cancelled: false,
        }
    }

    /// The task stopped because it was cancelled.
    #[must_use]
    pub fn cancelled() -> Self {
        Self {
            message: "cancelled".to_owned(),
            last_lines: Vec::new(),
            cancelled: true,
        }
    }

    /// Whether it is a cancellation rather than a failure.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// With these last lines of output.
    #[must_use]
    pub fn with_last_lines(mut self, lines: Vec<String>) -> Self {
        self.last_lines = lines;
        self
    }
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TaskError {}

impl From<std::io::Error> for TaskError {
    fn from(e: std::io::Error) -> Self {
        Self::new(e.to_string())
    }
}

impl From<videbimus_store::Error> for TaskError {
    fn from(e: videbimus_store::Error) -> Self {
        Self::new(e.to_string())
    }
}

impl From<String> for TaskError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for TaskError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}
