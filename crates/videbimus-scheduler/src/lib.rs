//! videbimus's scheduler: the one place heavy work runs (ADR 0004).
//!
//! A [`Task`] is one asset's recipe as the scheduler sees it: a name (its
//! store slot), an output [`Shape`](videbimus_store::Shape), declared
//! [`Resources`] and [`Cost`], a key, and a [`run`](Task::run) that builds
//! one chunk. Tasks go into a [`Graph`] with labelled inputs, and a
//! [`Scheduler`] builds requested [`Target`]s:
//!
//! 1. **Keys and the cache.** Each task's key is its own inputs plus each
//!    graph input's identity for dependants. A complete key is a cache hit
//!    and is skipped. An input that hashes its output only has an identity
//!    once built (early cutoff, ADR 0003 §4), so its dependants' keys are
//!    resolved *lazily*, after it finishes; an identical rebuild leaves
//!    them cached. An input whose dependants are all cached isn't built.
//! 2. **Chunks.** A frame sequence's missing frames are split into chunks
//!    of about [`SchedulerOptions::chunk_seconds`] of estimated work, each
//!    run, committed and resumed on its own (ADR 0004 §5, ADR 0008).
//! 3. **Budgets.** Chunks are packed so running chunks' declared cores and
//!    memory stay within the [`Profile`]'s budget. A chunk bigger than the
//!    whole budget runs alone, with a warning. At most one GPU chunk runs
//!    at a time. The profile can be switched mid-run through [`Control`];
//!    it applies to chunks started afterwards.
//! 4. **Cost history.** The newest uncontended complete build under a
//!    task's name replaces its declared seconds and memory; see
//!    [`SchedulerOptions::contention_threshold_cores`].
//! 5. **Order.** Ready chunks run critical path first: the longest
//!    estimated chain of remaining work from the task up to a requested
//!    output. A [`Control::preview`] request jumps the queue with only what
//!    it needs; a run whose targets are a preview builds only their needs.
//! 6. **Stop.** [`Control::stop`] (or Ctrl-C, with [`stop_on_sigint`])
//!    cancels running chunks, kills their subprocess groups, discards the
//!    in-flight chunks and returns. Running again resumes from the
//!    committed chunks, losing at most one chunk per running task.
//! 7. **Events.** Everything that happens is an [`Event`] sent to an
//!    [`EventSink`], with the task's name, group and machine: enough for
//!    the logs, status file and live tree of ADR 0007.
//! 8. **Executors.** Chunks run through an [`Executor`]; [`LocalExecutor`]
//!    runs each on a thread here.
//!
//! # Platform
//!
//! Unix first. On Windows, subprocesses get no niceness, no process
//! group (a stop kills the child only, not what it started), and no peak
//! memory; there is no load average, so no run is ever marked contended;
//! and [`stop_on_sigint`] is unsupported.

mod context;
mod event;
mod executor;
mod graph;
mod history;
mod profile;
mod scheduler;
mod signal;
mod subprocess;
mod task;

pub use context::{CancelToken, TaskContext, TaskLog};
pub use event::{Collect, EarlyCutoff, Event, EventKind, EventSink, NoEvents, Outcome, TaskRef};
pub use executor::{Executor, Job, LocalExecutor};
pub use graph::{Graph, GraphError, Node, NodeId, Target};
pub use history::load_average;
pub use profile::{Machine, Profile};
pub use scheduler::{
    Control, LoadProbe, RunError, RunReport, Scheduler, SchedulerOptions, TaskReport, TaskState,
};
pub use signal::{MAX_GROUPS, sigint_received, stop_on_sigint};
pub use subprocess::{CommandOutput, THREAD_CAP_VARS};
pub use task::{Cost, Resources, Task, TaskError};

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
