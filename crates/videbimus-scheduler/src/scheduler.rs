//! The scheduler: resolves keys, skips cache hits, chunks what is left,
//! and runs chunks within the profile's budget, critical path first.

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use videbimus_core::{AssetKey, Frame, Interval};
use videbimus_store::{AssetWriter, ChunkId, KeyInputs, Shape, Status, Store, key_hex};

use crate::context::{CancelToken, TaskLog};
use crate::event::{EarlyCutoff, Event, EventKind, EventSink, NoEvents, Outcome, TaskRef};
use crate::executor::{Done, Executor, Job, JobOutcome, LocalExecutor};
use crate::graph::{Graph, NodeId, Target};
use crate::history::{self, OwnLoad};
use crate::now_ms;
use crate::profile::{Machine, Profile};
use crate::signal;
use crate::task::{Resources, TaskError};

/// Reads the 1-minute load average (or a stand-in, in tests).
pub type LoadProbe = Arc<dyn Fn() -> Option<f64> + Send + Sync>;

/// Tuning for a [`Scheduler`].
#[derive(Clone)]
pub struct SchedulerOptions {
    /// Estimated seconds of work per chunk of a frame sequence (ADR 0004
    /// §5). Default 120.
    pub chunk_seconds: f64,
    /// How long a cancelled subprocess group gets between `SIGTERM` and
    /// `SIGKILL`. Default 2 s.
    pub kill_grace: Duration,
    /// Other load, in cores, above which a run counts as contended and its
    /// measured cost is ignored as history (ADR 0004 §2). `None`: a
    /// quarter of the machine's cores, at least 1.
    ///
    /// At each chunk start the scheduler samples the load average (see
    /// [`load_probe`](Self::load_probe)) and subtracts its own share of
    /// it: the cores of its running chunks, smoothed with the same
    /// one-minute decay the kernel uses, so its own recent work doesn't
    /// read as someone else's. The rest is other work; the highest seen
    /// is recorded in the manifest as
    /// [`external_load_pct`](videbimus_store::Manifest::external_load_pct).
    /// It is a heuristic: tasks that use more threads than they declare
    /// look like contention. A manifest without a measurement counts as
    /// uncontended.
    pub contention_threshold_cores: Option<f64>,
    /// Whether to use measured history in place of declared costs.
    /// Default true.
    pub use_history: bool,
    /// How often the scheduler checks for stop requests and profile
    /// changes while waiting for chunks. Default 50 ms.
    pub poll: Duration,
    /// The load probe. `None`: the system's 1-minute load average.
    pub load_probe: Option<LoadProbe>,
}

impl Default for SchedulerOptions {
    fn default() -> Self {
        Self {
            chunk_seconds: 120.0,
            kill_grace: Duration::from_secs(2),
            contention_threshold_cores: None,
            use_history: true,
            poll: Duration::from_millis(50),
            load_probe: None,
        }
    }
}

impl fmt::Debug for SchedulerOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SchedulerOptions")
            .field("chunk_seconds", &self.chunk_seconds)
            .field("kill_grace", &self.kill_grace)
            .field(
                "contention_threshold_cores",
                &self.contention_threshold_cores,
            )
            .field("use_history", &self.use_history)
            .field("poll", &self.poll)
            .field("load_probe", &self.load_probe.as_ref().map(|_| "custom"))
            .finish()
    }
}

#[derive(Debug)]
struct ControlInner {
    stop: AtomicBool,
    profile: Mutex<Profile>,
    profile_changed: AtomicBool,
    previews: Mutex<Vec<Target>>,
}

/// Steers a running build from another thread: stop it, switch profile,
/// or ask for a preview. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Control(Arc<ControlInner>);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Control {
    fn new(profile: Profile) -> Self {
        Self(Arc::new(ControlInner {
            stop: AtomicBool::new(false),
            profile: Mutex::new(profile),
            profile_changed: AtomicBool::new(false),
            previews: Mutex::new(Vec::new()),
        }))
    }

    /// Stops the current run (ADR 0008 §2): running chunks are cancelled
    /// and their subprocesses killed, and [`Scheduler::run`] returns
    /// [`Outcome::Stopped`]. Building again resumes. The request is
    /// cleared when the run returns.
    pub fn stop(&self) {
        self.0.stop.store(true, Ordering::SeqCst);
    }

    /// Whether a stop has been requested.
    #[must_use]
    pub fn is_stopping(&self) -> bool {
        self.0.stop.load(Ordering::SeqCst)
    }

    /// Switches profile. Chunks already running keep their cores and
    /// niceness; chunks started from now on are packed into the new
    /// budget (ADR 0004 §3).
    pub fn set_profile(&self, profile: Profile) {
        *lock(&self.0.profile) = profile;
        self.0.profile_changed.store(true, Ordering::SeqCst);
    }

    /// The profile in force.
    #[must_use]
    pub fn profile(&self) -> Profile {
        lock(&self.0.profile).clone()
    }

    /// Asks the current run for `targets` first (ADR 0004 §4): they and
    /// what they need jump the queue. Targets outside the run's request
    /// are added to it.
    pub fn preview(&self, targets: impl IntoIterator<Item = Target>) {
        lock(&self.0.previews).extend(targets);
    }
}

/// Why a run couldn't start.
#[derive(Debug)]
pub enum RunError {
    /// The store was opened read-only.
    ReadOnly,
    /// A target isn't a node of the graph.
    UnknownTarget(NodeId),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::ReadOnly => {
                f.write_str("the store was opened read-only; open it with Store::open to build")
            }
            RunError::UnknownTarget(id) => write!(
                f,
                "target node {} isn't in the graph; pass ids from Graph::add",
                id.0
            ),
        }
    }
}

impl std::error::Error for RunError {}

/// Where a task ended up after a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskState {
    /// Not reached from the run's targets.
    NotRequested,
    /// Everything reading it was cached, so it wasn't needed.
    NotNeeded,
    /// Its key was already complete.
    Cached,
    /// Built in this run.
    Built,
    /// A partial target's frames were built; the asset isn't complete.
    Partial,
    /// It failed, with this message.
    Failed(String),
    /// An input failed: this one.
    Blocked(String),
    /// The run stopped before it completed.
    Stopped,
}

/// One task's result.
#[derive(Clone, Debug)]
pub struct TaskReport {
    /// Its name.
    pub name: String,
    /// Where it ended up.
    pub state: TaskState,
    /// Its key, if resolved.
    pub key: Option<AssetKey>,
    /// Chunks committed in this run.
    pub chunks_committed: usize,
    /// Running chunks cancelled (and discarded) by a stop.
    pub chunks_cancelled: usize,
}

/// What a run did.
#[derive(Clone, Debug)]
pub struct RunReport {
    /// How it ended.
    pub outcome: Outcome,
    /// Every task in the graph, in graph order.
    pub tasks: Vec<TaskReport>,
}

impl RunReport {
    /// The report for the task named `name`.
    #[must_use]
    pub fn task(&self, name: &str) -> Option<&TaskReport> {
        self.tasks.iter().find(|t| t.name == name)
    }
}

/// The one scheduler that owns all heavy work (ADR 0004 §1).
///
/// ```no_run
/// # use videbimus_scheduler::*;
/// # fn demo(graph: Graph, store: videbimus_store::Store) -> Result<(), RunError> {
/// let machine = Machine::detect();
/// let scheduler = Scheduler::new(store, Profile::background(&machine));
/// let control = scheduler.control(); // stop, set_profile, preview
/// let report = scheduler.run(&graph, &[])?; // everything
/// # Ok(()) }
/// ```
pub struct Scheduler {
    store: Store,
    machine: Machine,
    executor: Arc<dyn Executor>,
    sink: Arc<dyn EventSink>,
    control: Control,
    options: SchedulerOptions,
}

impl Scheduler {
    /// A scheduler building into `store` (opened for writing) under
    /// `profile`, running locally, sending events nowhere.
    #[must_use]
    pub fn new(store: Store, profile: Profile) -> Self {
        Self {
            store,
            machine: Machine::detect(),
            executor: Arc::new(LocalExecutor::new()),
            sink: Arc::new(NoEvents),
            control: Control::new(profile),
            options: SchedulerOptions::default(),
        }
    }

    /// Sends events to `sink`.
    #[must_use]
    pub fn with_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.sink = sink;
        self
    }

    /// Runs chunks through `executor`.
    #[must_use]
    pub fn with_executor(mut self, executor: Arc<dyn Executor>) -> Self {
        self.executor = executor;
        self
    }

    /// Sets the options.
    #[must_use]
    pub fn with_options(mut self, options: SchedulerOptions) -> Self {
        self.options = options;
        self
    }

    /// Overrides the detected machine (its cores set the default
    /// contention threshold).
    #[must_use]
    pub fn with_machine(mut self, machine: Machine) -> Self {
        self.machine = machine;
        self
    }

    /// A handle to stop the run, switch profile or ask for a preview,
    /// from another thread (or from an event sink).
    #[must_use]
    pub fn control(&self) -> Control {
        self.control.clone()
    }

    /// The store.
    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Builds `targets` (all of the graph's sinks if empty) and what they
    /// need, and returns when they are done, a task has failed and
    /// nothing else can proceed, or a stop was requested. Failures don't
    /// stop independent work.
    ///
    /// # Errors
    ///
    /// A read-only store, or a target not in `graph`.
    pub fn run(&self, graph: &Graph, targets: &[Target]) -> Result<RunReport, RunError> {
        if self.store.is_read_only() {
            return Err(RunError::ReadOnly);
        }
        for t in targets {
            if t.node.0 >= graph.len() {
                return Err(RunError::UnknownTarget(t.node));
            }
        }
        let targets: Vec<Target> = if targets.is_empty() {
            graph.sinks().into_iter().map(Target::all).collect()
        } else {
            targets.to_vec()
        };
        let report = Run::new(self, graph, targets).go();
        self.control.0.stop.store(false, Ordering::SeqCst);
        signal::clear_sigint();
        Ok(report)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Phase {
    NotRequested,
    Unresolved,
    Resolved,
    Cached,
    Building,
    Done,
    Partial,
    NotNeeded,
    Failed(String),
    Blocked(String),
    Stopped,
}

impl Phase {
    fn complete(&self) -> bool {
        matches!(self, Phase::Cached | Phase::Done)
    }
    fn broken(&self) -> bool {
        matches!(self, Phase::Failed(_) | Phase::Blocked(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Demand {
    None,
    Frames(Vec<Interval>),
    Full,
}

impl Demand {
    fn add(&mut self, frames: Option<Interval>) {
        match (frames, &mut *self) {
            (_, Demand::Full) => {}
            (None, _) => *self = Demand::Full,
            (Some(iv), Demand::Frames(v)) => v.push(iv),
            (Some(iv), Demand::None) => *self = Demand::Frames(vec![iv]),
        }
    }
    fn any(&self) -> bool {
        *self != Demand::None
    }
    fn wants(&self, chunk: ChunkId) -> bool {
        match (self, chunk) {
            (Demand::None, _) => false,
            (Demand::Full, _) | (Demand::Frames(_), ChunkId::Whole) => true,
            (Demand::Frames(v), ChunkId::Frames(c)) => v.iter().any(|iv| iv.overlap(c).is_some()),
        }
    }
}

struct NodeRun {
    tref: TaskRef,
    phase: Phase,
    inputs: Option<KeyInputs>,
    key: Option<AssetKey>,
    identity: Option<AssetKey>,
    input_keys: Vec<(String, AssetKey)>,
    resources: Resources,
    total_s: f64,
    remaining_frac: f64,
    writer: Option<Arc<AssetWriter>>,
    log: Option<Arc<TaskLog>>,
    waiting: usize,
    running: usize,
    failure: Option<TaskError>,
    peak: Option<u64>,
    external_pct: Option<u32>,
    committed: usize,
    cancelled: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnitState {
    Waiting,
    Running,
    Over,
}

struct UnitRun {
    node: usize,
    chunk: ChunkId,
    est_s: f64,
    state: UnitState,
    cancel: CancelToken,
    cores: u32,
    memory: u64,
    gpu: bool,
    alone: bool,
}

struct Run<'a> {
    s: &'a Scheduler,
    g: &'a Graph,
    nodes: Vec<NodeRun>,
    units: Vec<UnitRun>,
    targets: Vec<Target>,
    previews: Vec<Target>,
    demand: Vec<Demand>,
    preview: Vec<bool>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    running: usize,
    running_cores: u32,
    running_memory: u64,
    gpu_busy: bool,
    exclusive: bool,
    own_load: OwnLoad,
    threshold_pct: u32,
    stopping: bool,
    chunks: usize,
    started: Instant,
}

impl<'a> Run<'a> {
    fn new(s: &'a Scheduler, g: &'a Graph, targets: Vec<Target>) -> Self {
        let nodes = g
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| NodeRun {
                tref: TaskRef {
                    id: i,
                    name: n.name.clone(),
                    group: n.group.clone(),
                },
                phase: Phase::NotRequested,
                inputs: None,
                key: None,
                identity: None,
                input_keys: Vec::new(),
                resources: n.task.resources(),
                total_s: 0.0,
                remaining_frac: 1.0,
                writer: None,
                log: None,
                waiting: 0,
                running: 0,
                failure: None,
                peak: None,
                external_pct: None,
                committed: 0,
                cancelled: 0,
            })
            .collect();
        let (tx, rx) = mpsc::channel();
        let threshold = s
            .options
            .contention_threshold_cores
            .unwrap_or_else(|| (f64::from(s.machine.cores) / 4.0).max(1.0));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let threshold_pct = (threshold * 100.0).round().max(0.0) as u32;
        Self {
            s,
            g,
            nodes,
            units: Vec::new(),
            targets,
            previews: Vec::new(),
            demand: vec![Demand::None; g.len()],
            preview: vec![false; g.len()],
            tx,
            rx,
            running: 0,
            running_cores: 0,
            running_memory: 0,
            gpu_busy: false,
            exclusive: false,
            own_load: OwnLoad::new(),
            threshold_pct,
            stopping: false,
            chunks: 0,
            started: Instant::now(),
        }
    }

    fn emit(&self, kind: EventKind) {
        self.s.sink.event(&Event {
            at_ms: now_ms(),
            kind,
        });
    }

    fn warn(&self, task: Option<usize>, message: String) {
        self.emit(EventKind::Warning {
            task: task.map(|i| self.nodes[i].tref.clone()),
            message,
        });
    }

    fn go(mut self) -> RunReport {
        let names = self
            .targets
            .iter()
            .map(|t| self.nodes[t.node.0].tref.name.clone())
            .collect();
        self.emit(EventKind::RunStarted {
            machine: self.s.machine.name.clone(),
            profile: self.s.control.profile(),
            targets: names,
        });
        let targets = self.targets.clone();
        self.consider(&targets);
        loop {
            self.poll_control();
            if !self.stopping {
                self.advance();
                self.start_units();
            }
            let waiting = self.units.iter().any(|u| u.state == UnitState::Waiting);
            if self.running == 0 && (self.stopping || !waiting) {
                break;
            }
            match self.rx.recv_timeout(self.s.options.poll) {
                Ok(done) => {
                    self.handle(done);
                    while let Ok(done) = self.rx.try_recv() {
                        self.handle(done);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.wrap_up()
    }

    /// Plans the targets' closure: every input, transitively.
    fn consider(&mut self, targets: &[Target]) {
        let mut stack: Vec<usize> = targets.iter().map(|t| t.node.0).collect();
        let mut new = HashSet::new();
        while let Some(i) = stack.pop() {
            if self.nodes[i].phase != Phase::NotRequested || !new.insert(i) {
                continue;
            }
            stack.extend(self.g.nodes[i].inputs.iter().map(|(_, n)| n.0));
        }
        let mut new: Vec<usize> = new.into_iter().collect();
        new.sort_unstable();
        for i in new {
            self.plan(i);
        }
    }

    fn plan(&mut self, i: usize) {
        let task = &self.g.nodes[i].task;
        let shape = task.shape();
        let mut resources = task.resources();
        let mut total_s = task.cost().total_seconds(shape);
        let mut from_history = false;
        if self.s.options.use_history
            && let Some(m) = history::lookup(
                &self.s.store,
                &self.nodes[i].tref.name,
                shape,
                self.threshold_pct,
            )
        {
            total_s = m.total_s;
            if let Some(mem) = m.memory_bytes {
                resources.memory_bytes = mem;
            }
            from_history = true;
        }
        let n = &mut self.nodes[i];
        n.phase = Phase::Unresolved;
        n.resources = resources;
        n.total_s = total_s;
        let inputs = self.g.nodes[i]
            .inputs
            .iter()
            .map(|(_, id)| self.g.nodes[id.0].name.clone())
            .collect();
        self.emit(EventKind::Planned {
            task: self.nodes[i].tref.clone(),
            inputs,
            shape,
            estimate_s: total_s,
            resources,
            from_history,
        });
    }

    fn poll_control(&mut self) {
        let s = self.s;
        let ctl = &s.control.0;
        if ctl.profile_changed.swap(false, Ordering::SeqCst) {
            self.emit(EventKind::ProfileChanged {
                profile: self.s.control.profile(),
            });
        }
        let previews: Vec<Target> = std::mem::take(&mut *lock(&ctl.previews));
        if !previews.is_empty() && !self.stopping {
            let valid: Vec<Target> = previews
                .into_iter()
                .filter(|t| {
                    let ok = t.node.0 < self.nodes.len();
                    if !ok {
                        self.warn(
                            None,
                            format!("preview of unknown node {} ignored", t.node.0),
                        );
                    }
                    ok
                })
                .collect();
            self.emit(EventKind::Preview {
                targets: valid
                    .iter()
                    .map(|t| self.nodes[t.node.0].tref.name.clone())
                    .collect(),
            });
            self.consider(&valid);
            self.previews.extend(valid);
        }
        if !self.stopping && (ctl.stop.load(Ordering::SeqCst) || signal::sigint_received()) {
            self.stopping = true;
            self.emit(EventKind::StopRequested);
            for u in &mut self.units {
                match u.state {
                    UnitState::Running => u.cancel.cancel(),
                    UnitState::Waiting => {
                        u.state = UnitState::Over;
                        self.nodes[u.node].waiting -= 1;
                    }
                    UnitState::Over => {}
                }
            }
            for i in 0..self.nodes.len() {
                if self.nodes[i].phase == Phase::Building && self.nodes[i].running == 0 {
                    self.settle(i);
                }
            }
        }
    }

    fn compute_demand(&mut self) {
        let n = self.nodes.len();
        let mut demand = vec![Demand::None; n];
        let mut preview = vec![false; n];
        for t in &self.targets {
            demand[t.node.0].add(t.frames);
        }
        for t in &self.previews {
            demand[t.node.0].add(t.frames);
            preview[t.node.0] = true;
        }
        for i in (0..n).rev() {
            if demand[i].any() && !self.nodes[i].phase.complete() {
                for (_, input) in &self.g.nodes[i].inputs {
                    demand[input.0] = Demand::Full;
                    if preview[i] {
                        preview[input.0] = true;
                    }
                }
            }
        }
        self.demand = demand;
        self.preview = preview;
    }

    /// Resolves keys, records cache hits and begins ready assets, until
    /// nothing more changes.
    fn advance(&mut self) {
        loop {
            self.compute_demand();
            let mut changed = false;
            for i in 0..self.nodes.len() {
                match self.nodes[i].phase {
                    Phase::Unresolved | Phase::Resolved => {}
                    _ => continue,
                }
                let inputs = &self.g.nodes[i].inputs;
                if let Some((_, by)) = inputs.iter().find(|(_, n)| self.nodes[n.0].phase.broken()) {
                    let by = self.nodes[by.0].tref.name.clone();
                    self.nodes[i].phase = Phase::Blocked(by.clone());
                    self.emit(EventKind::Blocked {
                        task: self.nodes[i].tref.clone(),
                        by,
                    });
                    changed = true;
                    continue;
                }
                if self.nodes[i].phase == Phase::Unresolved {
                    if inputs
                        .iter()
                        .all(|(_, n)| self.nodes[n.0].identity.is_some())
                    {
                        self.resolve(i);
                        changed = true;
                    }
                } else if self.demand[i].any()
                    && inputs.iter().all(|(_, n)| self.nodes[n.0].phase.complete())
                {
                    let key = self.nodes[i].key;
                    let twin = self
                        .nodes
                        .iter()
                        .any(|n| n.phase == Phase::Building && n.key == key);
                    if twin {
                        // Another task builds the same key: wait for it,
                        // then take its output as a cache hit.
                        continue;
                    }
                    if !self.cache_hit(i) {
                        self.begin(i);
                    }
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn fail_node(&mut self, i: usize, message: String) {
        let log = self.nodes[i].log.as_ref().map(|l| l.path().to_owned());
        self.emit(EventKind::Failed {
            task: self.nodes[i].tref.clone(),
            chunk: None,
            message: message.clone(),
            last_lines: Vec::new(),
            log,
        });
        self.nodes[i].phase = Phase::Failed(message);
    }

    /// If node `i`'s key is complete in the store, makes it a cache hit
    /// and says so.
    fn cache_hit(&mut self, i: usize) -> bool {
        let Some(key) = self.nodes[i].key else {
            return false;
        };
        let store = &self.s.store;
        match store.manifest(key) {
            Ok(Some(m)) if m.status == Status::Complete => {}
            _ => return false,
        }
        let identity = match store.identity_for_dependants(key) {
            Ok(id) => id.unwrap_or(key),
            Err(e) => {
                self.fail_node(i, e.to_string());
                return true;
            }
        };
        if let Err(e) = store.record(&self.nodes[i].tref.name, key) {
            self.warn(Some(i), format!("couldn't record the cache hit: {e}"));
        }
        let n = &mut self.nodes[i];
        n.identity = Some(identity);
        n.phase = Phase::Cached;
        n.remaining_frac = 0.0;
        n.inputs = None;
        self.emit(EventKind::Cached {
            task: self.nodes[i].tref.clone(),
            key,
        });
        true
    }

    fn resolve(&mut self, i: usize) {
        let def = &self.g.nodes[i];
        let mut kb = def.task.key();
        let mut input_keys = Vec::new();
        for (label, input) in &def.inputs {
            let n = &self.nodes[input.0];
            let (Some(identity), Some(key)) = (n.identity, n.key) else {
                return;
            };
            kb = kb.input(label, identity);
            input_keys.push((label.clone(), key));
        }
        let inputs = match kb.build() {
            Ok(k) => k,
            Err(e) => return self.fail_node(i, format!("its key couldn't be built: {e}")),
        };
        let key = inputs.key();
        {
            let n = &mut self.nodes[i];
            n.key = Some(key);
            n.input_keys = input_keys;
        }
        self.emit(EventKind::KeyResolved {
            task: self.nodes[i].tref.clone(),
            key,
        });
        if self.cache_hit(i) {
            return;
        }
        let frac = match self.s.store.progress(key) {
            Ok(Some(p)) if p.total_frames > 0 => {
                #[allow(clippy::cast_precision_loss)]
                let f = (p.total_frames - p.done_frames.min(p.total_frames)) as f64
                    / p.total_frames as f64;
                f
            }
            _ => 1.0,
        };
        let hash = self.g.nodes[i].task.hash_output();
        let n = &mut self.nodes[i];
        n.inputs = Some(inputs);
        n.identity = (!hash).then_some(key);
        n.remaining_frac = frac;
        n.phase = Phase::Resolved;
    }

    fn per_frame_s(&self, i: usize) -> f64 {
        match self.g.nodes[i].task.shape() {
            #[allow(clippy::cast_precision_loss)]
            Shape::Frames(iv) if !iv.is_empty() => self.nodes[i].total_s / iv.len() as f64,
            _ => self.nodes[i].total_s,
        }
    }

    fn begin(&mut self, i: usize) {
        let def = &self.g.nodes[i];
        let Some(inputs) = self.nodes[i].inputs.take() else {
            return self.fail_node(i, "its key inputs are missing".to_owned());
        };
        let writer = match self.s.store.begin(&def.name, &inputs, def.task.shape()) {
            Ok(w) => w,
            Err(e) => return self.fail_node(i, format!("couldn't begin: {e}")),
        };
        let log = Arc::new(TaskLog::open(writer.log_path()));
        log.line(&format!(
            "== {} {} on {}, profile {}",
            def.name,
            key_hex(&writer.key()),
            self.s.executor.machine(),
            self.s.control.profile().name
        ));
        log.line(&format!("why: {}", writer.why()));
        if let Some(config) = def.task.config_text() {
            log.line("config:");
            for l in config.lines() {
                log.line(&format!("    {l}"));
            }
        }
        for r in inputs.records() {
            log.line(&format!(
                "input: {} {} {}{}",
                r.class,
                if r.label.is_empty() { "-" } else { &r.label },
                &key_hex(&AssetKey(r.digest))[..16],
                r.detail
                    .as_ref()
                    .map_or(String::new(), |d| format!(" ({d})"))
            ));
        }
        let key = writer.key();
        let why = writer.why().clone();
        let n = &mut self.nodes[i];
        n.writer = Some(Arc::new(writer));
        n.log = Some(log.clone());
        n.phase = Phase::Building;
        let chunks = match self.plan_chunks(i) {
            Ok(c) => c,
            Err(e) => {
                self.nodes[i].failure = Some(TaskError::new(e.to_string()));
                return self.settle(i);
            }
        };
        let (done_frames, total_frames) = match self.s.store.progress(key) {
            Ok(Some(p)) => (p.done_frames, p.total_frames),
            _ => (0, 0),
        };
        self.emit(EventKind::Begun {
            task: self.nodes[i].tref.clone(),
            key,
            why,
            chunks: chunks.len(),
            done_frames,
            total_frames,
            log: log.path().to_owned(),
        });
        if chunks.is_empty() {
            return self.settle(i);
        }
        self.queue(i, chunks);
    }

    /// The chunks still missing from node `i`'s asset that its demand
    /// wants, split to about `chunk_seconds` each.
    fn plan_chunks(&self, i: usize) -> Result<Vec<ChunkId>, videbimus_store::Error> {
        let Some(writer) = &self.nodes[i].writer else {
            return Ok(Vec::new());
        };
        let task = &self.g.nodes[i].task;
        let per_frame = self.per_frame_s(i);
        let step: i64 = match task.chunk_frames() {
            Some(n) => i64::from(n.max(1)),
            #[allow(clippy::cast_possible_truncation)]
            None if per_frame > 0.0 => (self.s.options.chunk_seconds / per_frame)
                .ceil()
                .clamp(1.0, 1e12) as i64,
            None => i64::MAX,
        };
        let mut out = Vec::new();
        for gap in writer.missing()? {
            match gap {
                ChunkId::Whole => out.push(ChunkId::Whole),
                ChunkId::Frames(iv) => {
                    let mut at = iv.start.0;
                    while at < iv.end.0 {
                        let end = at.saturating_add(step).min(iv.end.0);
                        out.push(ChunkId::Frames(Interval::new(Frame(at), Frame(end))));
                        at = end;
                    }
                }
            }
        }
        out.retain(|c| self.demand[i].wants(*c));
        Ok(out)
    }

    fn queue(&mut self, i: usize, chunks: Vec<ChunkId>) {
        let per_frame = self.per_frame_s(i);
        let res = self.nodes[i].resources;
        for chunk in chunks {
            #[allow(clippy::cast_precision_loss)]
            let est_s = match chunk {
                ChunkId::Whole => self.nodes[i].total_s,
                ChunkId::Frames(iv) => per_frame * iv.len() as f64,
            };
            self.units.push(UnitRun {
                node: i,
                chunk,
                est_s,
                state: UnitState::Waiting,
                cancel: CancelToken::new(),
                cores: res.cores_at_least_one(),
                memory: res.memory_bytes,
                gpu: res.gpu,
                alone: false,
            });
            self.nodes[i].waiting += 1;
            self.emit(EventKind::Queued {
                task: self.nodes[i].tref.clone(),
                chunk,
                estimate_s: est_s,
            });
        }
    }

    /// Each node's critical path: its remaining estimate plus the longest
    /// chain of demanded dependants above it (ADR 0004 §4).
    fn ranks(&self) -> Vec<f64> {
        let n = self.nodes.len();
        let mut remaining = vec![0.0; n];
        for (i, node) in self.nodes.iter().enumerate() {
            remaining[i] = match node.phase {
                Phase::Unresolved | Phase::Resolved => node.total_s * node.remaining_frac,
                _ => 0.0,
            };
        }
        for u in &self.units {
            if u.state != UnitState::Over {
                remaining[u.node] += u.est_s;
            }
        }
        let mut rank = vec![0.0f64; n];
        for i in (0..n).rev() {
            let above = self.g.nodes[i]
                .dependants
                .iter()
                .filter(|d| self.demand[d.0].any())
                .map(|d| rank[d.0])
                .fold(0.0, f64::max);
            rank[i] = remaining[i] + above;
        }
        rank
    }

    fn start_units(&mut self) {
        if self.exclusive || self.stopping {
            return;
        }
        let profile = self.s.control.profile();
        let rank = self.ranks();
        let mut order: Vec<usize> = (0..self.units.len())
            .filter(|&u| self.units[u].state == UnitState::Waiting)
            .collect();
        order.sort_by(|&a, &b| {
            let (ua, ub) = (&self.units[a], &self.units[b]);
            let start = |c: ChunkId| c.frames().map_or(i64::MIN, |f| f.start.0);
            self.preview[ub.node]
                .cmp(&self.preview[ua.node])
                .then(rank[ub.node].total_cmp(&rank[ua.node]))
                .then(ua.node.cmp(&ub.node))
                .then(start(ua.chunk).cmp(&start(ub.chunk)))
                .then(a.cmp(&b))
        });
        for (pos, u) in order.into_iter().enumerate() {
            let (cores, memory, gpu) = {
                let unit = &self.units[u];
                (unit.cores, unit.memory, unit.gpu)
            };
            let oversized = cores > profile.cores || memory > profile.memory_bytes;
            if oversized {
                if self.running == 0 {
                    self.warn(
                        Some(self.units[u].node),
                        format!(
                            "needs {cores} cores and {memory} bytes, more than the `{}` budget ({} cores, {} bytes): running it alone",
                            profile.name, profile.cores, profile.memory_bytes
                        ),
                    );
                    self.start(u, profile.cores, profile.niceness, true);
                    return;
                }
                if pos == 0 {
                    // It is next: let the machine drain for it.
                    return;
                }
                continue;
            }
            if gpu && self.gpu_busy {
                continue;
            }
            if self.running_cores + cores <= profile.cores
                && self.running_memory + memory <= profile.memory_bytes
            {
                self.start(u, cores, profile.niceness, false);
            }
        }
    }

    fn start(&mut self, u: usize, cores: u32, niceness: i32, alone: bool) {
        let own = self.own_load.update(self.running_cores);
        let probe = match &self.s.options.load_probe {
            Some(p) => p(),
            None => history::load_average(),
        };
        let i = self.units[u].node;
        if let Some(load) = probe {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let pct = ((load - own).max(0.0) * 100.0).round() as u32;
            let n = &mut self.nodes[i];
            n.external_pct = Some(n.external_pct.map_or(pct, |p| p.max(pct)));
        }
        let unit = &mut self.units[u];
        unit.state = UnitState::Running;
        unit.alone = alone;
        unit.cores = cores;
        self.running += 1;
        self.running_cores += cores;
        self.running_memory += unit.memory;
        self.gpu_busy |= unit.gpu;
        self.exclusive |= alone;
        let n = &mut self.nodes[i];
        n.waiting -= 1;
        n.running += 1;
        let chunk = unit.chunk;
        self.emit(EventKind::Started {
            task: self.nodes[i].tref.clone(),
            chunk,
            machine: self.s.executor.machine().to_owned(),
            cores,
            niceness,
            alone,
        });
        let n = &self.nodes[i];
        let (Some(writer), Some(log), Some(key)) = (n.writer.clone(), n.log.clone(), n.key) else {
            return;
        };
        let job = Job {
            unit: u,
            task: self.g.nodes[i].task.clone(),
            task_ref: n.tref.clone(),
            store: self.s.store.clone(),
            key,
            writer: Some(writer),
            chunk,
            cores,
            niceness,
            cancel: self.units[u].cancel.clone(),
            log,
            sink: self.s.sink.clone(),
            inputs: n.input_keys.clone(),
            kill_grace: self.s.options.kill_grace,
            done: Some(self.tx.clone()),
        };
        self.s.executor.execute(job);
    }

    fn handle(&mut self, done: Done) {
        let Some(unit) = self.units.get_mut(done.unit) else {
            return;
        };
        if unit.state != UnitState::Running {
            return;
        }
        unit.state = UnitState::Over;
        let (i, chunk, cores, memory, gpu, alone) = (
            unit.node,
            unit.chunk,
            unit.cores,
            unit.memory,
            unit.gpu,
            unit.alone,
        );
        self.own_load.update(self.running_cores);
        self.running -= 1;
        self.running_cores -= cores;
        self.running_memory -= memory;
        if gpu {
            self.gpu_busy = false;
        }
        if alone {
            self.exclusive = false;
        }
        self.nodes[i].running -= 1;
        let tref = self.nodes[i].tref.clone();
        match done.outcome {
            JobOutcome::Committed(rec) => {
                let n = &mut self.nodes[i];
                n.committed += 1;
                if let Some(p) = rec.peak_memory_bytes {
                    n.peak = Some(n.peak.map_or(p, |q| q.max(p)));
                }
                self.chunks += 1;
                self.emit(EventKind::ChunkCommitted {
                    task: tref,
                    chunk,
                    ms: rec.duration_ms(),
                    peak_memory_bytes: rec.peak_memory_bytes,
                });
            }
            JobOutcome::Failed(e) => {
                let log = self.nodes[i].log.as_ref().map(|l| l.path().to_owned());
                self.emit(EventKind::Failed {
                    task: tref,
                    chunk: Some(chunk),
                    message: e.message.clone(),
                    last_lines: e.last_lines.clone(),
                    log,
                });
                if self.nodes[i].failure.is_none() {
                    self.nodes[i].failure = Some(e);
                }
                for u in &mut self.units {
                    if u.node == i && u.state == UnitState::Waiting {
                        u.state = UnitState::Over;
                    }
                }
                self.nodes[i].waiting = 0;
            }
            JobOutcome::Cancelled => {
                self.nodes[i].cancelled += 1;
                self.emit(EventKind::Cancelled { task: tref, chunk });
            }
        }
        if self.nodes[i].running == 0 && self.nodes[i].waiting == 0 {
            self.settle(i);
        }
    }

    /// Node `i` has nothing running or waiting: finish it, fail it, queue
    /// what is still missing, or leave it for the next run.
    fn settle(&mut self, i: usize) {
        if self.nodes[i].phase != Phase::Building {
            return;
        }
        let Some(writer) = self.nodes[i].writer.take() else {
            return;
        };
        if let Some(e) = self.nodes[i].failure.clone() {
            if let Ok(w) = Arc::try_unwrap(writer)
                && let Err(err) = w.fail(&e.message)
            {
                self.warn(Some(i), format!("couldn't record the failure: {err}"));
            }
            let log = self.nodes[i].log.as_ref().map(|l| l.path().to_owned());
            self.emit(EventKind::Failed {
                task: self.nodes[i].tref.clone(),
                chunk: None,
                message: e.message.clone(),
                last_lines: e.last_lines,
                log,
            });
            self.nodes[i].phase = Phase::Failed(e.message);
            return;
        }
        if self.stopping {
            // Dropped unfinished: the asset stays `Building`, to resume.
            self.nodes[i].phase = Phase::Stopped;
            return;
        }
        let missing = match writer.missing() {
            Ok(m) => m,
            Err(e) => return self.fail_node(i, e.to_string()),
        };
        if missing.is_empty() {
            return self.finish(i, writer);
        }
        self.nodes[i].writer = Some(writer);
        let chunks = match self.plan_chunks(i) {
            Ok(c) => c,
            Err(e) => {
                self.nodes[i].writer = None;
                return self.fail_node(i, e.to_string());
            }
        };
        if chunks.is_empty() {
            self.nodes[i].writer = None;
            self.nodes[i].phase = Phase::Partial;
            let key = self.nodes[i].key.unwrap_or(AssetKey([0; 32]));
            self.emit(EventKind::Partial {
                task: self.nodes[i].tref.clone(),
                key,
            });
        } else {
            self.queue(i, chunks);
        }
    }

    fn finish(&mut self, i: usize, writer: Arc<AssetWriter>) {
        let Ok(mut w) = Arc::try_unwrap(writer) else {
            return self.fail_node(i, "its asset is still in use by a chunk".to_owned());
        };
        if let Some(pct) = self.nodes[i].external_pct {
            w.note_external_load(pct);
        }
        let key = w.key();
        let name = self.nodes[i].tref.name.clone();
        let manifest = match w.finish(self.nodes[i].peak) {
            Ok(m) => m,
            Err(e) => return self.fail_node(i, format!("couldn't finish: {e}")),
        };
        let store = &self.s.store;
        let identity = match store.identity_for_dependants(key) {
            Ok(id) => id.unwrap_or(key),
            Err(e) => return self.fail_node(i, e.to_string()),
        };
        let cutoff = match store.early_cutoff(&name, key) {
            Ok(c) => EarlyCutoff::from(&c),
            Err(_) => EarlyCutoff::NotHashed,
        };
        if let Some(log) = &self.nodes[i].log {
            log.line(&format!(
                "finished: build {} ms, peak memory {}, output {:?}",
                manifest.build_ms.unwrap_or(0),
                manifest
                    .peak_memory_bytes
                    .map_or("unmeasured".to_owned(), |p| format!("{p} bytes")),
                cutoff
            ));
        }
        let n = &mut self.nodes[i];
        n.identity = Some(identity);
        n.phase = Phase::Done;
        self.emit(EventKind::Finished {
            task: self.nodes[i].tref.clone(),
            key,
            build_ms: manifest.build_ms,
            peak_memory_bytes: manifest.peak_memory_bytes,
            cutoff,
        });
    }

    fn wrap_up(mut self) -> RunReport {
        self.compute_demand();
        for i in 0..self.nodes.len() {
            match self.nodes[i].phase {
                Phase::Unresolved | Phase::Resolved => {
                    if self.stopping && self.demand[i].any() {
                        self.nodes[i].phase = Phase::Stopped;
                    } else {
                        self.nodes[i].phase = Phase::NotNeeded;
                        self.emit(EventKind::NotNeeded {
                            task: self.nodes[i].tref.clone(),
                        });
                    }
                }
                Phase::Building => {
                    self.nodes[i].writer = None;
                    self.nodes[i].phase = Phase::Stopped;
                }
                _ => {}
            }
        }
        let count = |f: fn(&Phase) -> bool| self.nodes.iter().filter(|n| f(&n.phase)).count();
        let failed = count(Phase::broken);
        let outcome = if self.stopping {
            Outcome::Stopped
        } else if failed > 0 {
            Outcome::Failed
        } else {
            Outcome::Complete
        };
        self.emit(EventKind::RunFinished {
            outcome,
            built: count(|p| *p == Phase::Done),
            cached: count(|p| *p == Phase::Cached),
            failed,
            chunks: self.chunks,
            ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
        let tasks = self
            .nodes
            .iter()
            .map(|n| TaskReport {
                name: n.tref.name.clone(),
                state: match &n.phase {
                    Phase::NotRequested => TaskState::NotRequested,
                    Phase::NotNeeded | Phase::Unresolved | Phase::Resolved => TaskState::NotNeeded,
                    Phase::Cached => TaskState::Cached,
                    Phase::Done => TaskState::Built,
                    Phase::Partial => TaskState::Partial,
                    Phase::Failed(m) => TaskState::Failed(m.clone()),
                    Phase::Blocked(b) => TaskState::Blocked(b.clone()),
                    Phase::Building | Phase::Stopped => TaskState::Stopped,
                },
                key: n.key,
                chunks_committed: n.committed,
                chunks_cancelled: n.cancelled,
            })
            .collect();
        RunReport { outcome, tasks }
    }
}
