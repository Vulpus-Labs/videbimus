//! Where chunks run: the [`Executor`] seam. One implementation, local
//! threads; a remote executor (ADR 0006) would ship the job's task to
//! another machine and bring its chunk back.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::Sender;
use std::time::Duration;

use videbimus_core::AssetKey;
use videbimus_store::{AssetWriter, ChunkId, ChunkRecord, CommitOptions, Store};

use crate::context::{CancelToken, TaskContext, TaskLog};
use crate::event::{EventSink, TaskRef};
use crate::task::{Task, TaskError};

/// Runs jobs. [`execute`](Self::execute) must not block: it starts the
/// job and returns; the job reports its own completion. Every job must
/// eventually be [`run`](Job::run) or dropped (dropping reports it
/// failed).
pub trait Executor: Send + Sync {
    /// The machine jobs run on, for events and manifests.
    fn machine(&self) -> &str;

    /// Starts `job`.
    fn execute(&self, job: Job);
}

/// Runs each job on its own thread on this machine.
#[derive(Debug)]
pub struct LocalExecutor {
    machine: String,
}

impl LocalExecutor {
    /// An executor for this machine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            machine: videbimus_store::machine_name(),
        }
    }
}

impl Default for LocalExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl Executor for LocalExecutor {
    fn machine(&self) -> &str {
        &self.machine
    }

    fn execute(&self, job: Job) {
        let name = format!("videbimus {}", job.task().name);
        // If the thread can't be spawned, the closure and its job are
        // dropped, and dropping a job reports it failed.
        let _ = std::thread::Builder::new()
            .name(name)
            .spawn(move || job.run());
    }
}

/// How a chunk ended.
#[derive(Debug)]
pub(crate) enum JobOutcome {
    Committed(ChunkRecord),
    Failed(TaskError),
    Cancelled,
}

#[derive(Debug)]
pub(crate) struct Done {
    pub unit: usize,
    pub outcome: JobOutcome,
}

/// One chunk of one task, ready to run: what an [`Executor`] is handed.
pub struct Job {
    pub(crate) unit: usize,
    pub(crate) task: Arc<dyn Task>,
    pub(crate) task_ref: TaskRef,
    pub(crate) store: Store,
    pub(crate) key: AssetKey,
    pub(crate) writer: Option<Arc<AssetWriter>>,
    pub(crate) chunk: ChunkId,
    pub(crate) cores: u32,
    pub(crate) niceness: i32,
    pub(crate) cancel: CancelToken,
    pub(crate) log: Arc<TaskLog>,
    pub(crate) sink: Arc<dyn EventSink>,
    pub(crate) inputs: Vec<(String, AssetKey)>,
    pub(crate) kill_grace: Duration,
    pub(crate) done: Option<Sender<Done>>,
}

impl Job {
    /// The task.
    #[must_use]
    pub fn task(&self) -> &TaskRef {
        &self.task_ref
    }

    /// The chunk to build.
    #[must_use]
    pub fn chunk(&self) -> ChunkId {
        self.chunk
    }

    /// The asset's key.
    #[must_use]
    pub fn key(&self) -> AssetKey {
        self.key
    }

    /// Its inputs' keys, by label: what a remote executor would send.
    #[must_use]
    pub fn inputs(&self) -> &[(String, AssetKey)] {
        &self.inputs
    }

    /// Cores granted.
    #[must_use]
    pub fn cores(&self) -> u32 {
        self.cores
    }

    /// The task log.
    #[must_use]
    pub fn log_path(&self) -> PathBuf {
        self.log.path().to_owned()
    }

    /// Runs the chunk here, on the calling thread: opens the chunk in the
    /// store, runs the task, and commits the chunk if it succeeded and
    /// wasn't cancelled. An uncommitted chunk is discarded.
    pub fn run(mut self) {
        let outcome = self.run_inner();
        self.finish(outcome);
    }

    fn finish(&mut self, outcome: JobOutcome) {
        // Release our hold on the asset before reporting, so the scheduler
        // can finish it as soon as its last chunk is in.
        self.writer = None;
        if let Some(tx) = self.done.take() {
            let _ = tx.send(Done {
                unit: self.unit,
                outcome,
            });
        }
    }

    fn run_inner(&self) -> JobOutcome {
        let Some(writer) = self.writer.as_deref() else {
            return JobOutcome::Failed(TaskError::new("the job has no asset to write"));
        };
        let chunk = match writer.chunk(self.chunk) {
            Ok(c) => c,
            Err(e) => return JobOutcome::Failed(e.into()),
        };
        let ctx = TaskContext {
            task: &self.task_ref,
            store: &self.store,
            key: writer.key(),
            chunk: self.chunk,
            out_dir: chunk.out_dir(),
            cores: self.cores,
            niceness: self.niceness,
            cancel: &self.cancel,
            log: &self.log,
            sink: &*self.sink,
            inputs: &self.inputs,
            kill_grace: self.kill_grace,
            peak_memory: AtomicU64::new(0),
        };
        ctx.log(&format!(
            "chunk started: {} cores, nice {}",
            self.cores, self.niceness
        ));
        let ran = catch_unwind(AssertUnwindSafe(|| self.task.run(&ctx)));
        let ran = ran.unwrap_or_else(|p| {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_owned());
            Err(TaskError::new(format!("task panicked: {msg}")))
        });
        if self.cancel.is_cancelled() {
            ctx.log("chunk cancelled; discarded");
            return JobOutcome::Cancelled;
        }
        if let Err(e) = ran {
            if e.is_cancelled() {
                ctx.log("chunk cancelled; discarded");
                return JobOutcome::Cancelled;
            }
            ctx.log(&format!("chunk failed: {e}"));
            return JobOutcome::Failed(e);
        }
        let peak = ctx.peak_memory();
        match chunk.commit(CommitOptions {
            hash: self.task.hash_output(),
            peak_memory_bytes: peak,
        }) {
            Ok(rec) => {
                ctx.log(&format!(
                    "chunk committed in {} ms{}",
                    rec.duration_ms(),
                    peak.map_or(String::new(), |p| format!(", peak memory {p} bytes"))
                ));
                JobOutcome::Committed(rec)
            }
            Err(e) => {
                ctx.log(&format!("chunk commit failed: {e}"));
                JobOutcome::Failed(e.into())
            }
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if self.done.is_some() {
            self.finish(JobOutcome::Failed(TaskError::new(
                "the executor dropped the job",
            )));
        }
    }
}
