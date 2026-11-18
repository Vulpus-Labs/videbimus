//! The progress tree (ADR 0007 §2): scheduler events folded into a
//! hierarchy that mirrors the scene, with each node's state, progress,
//! elapsed time, ETA and machine; and the [`Snapshot`] of it that the live
//! view draws and the status file holds.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use videbimus_scheduler::{Event, EventKind, Outcome, TaskRef};
use videbimus_store::{ChunkId, Shape};

/// The status file's format, written as [`Snapshot::format`].
pub const SNAPSHOT_FORMAT: u32 = 1;

/// Warnings a snapshot keeps: the newest.
const KEEP_WARNINGS: usize = 5;

/// Where a task, or a group of them, stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Planned; not yet queued (its inputs aren't built, or its key isn't
    /// known yet).
    Waiting,
    /// Chunks are waiting for budget; none is running.
    Queued,
    /// A chunk is running.
    Running,
    /// Built in this run (for a preview, the requested frames).
    Done,
    /// Already complete in the store.
    Cached,
    /// Not built: everything that reads it is cached.
    NotNeeded,
    /// It failed.
    Failed,
    /// An input failed, so it can't run.
    Blocked,
    /// The run stopped before it finished; building again resumes it.
    Cancelled,
}

impl State {
    /// How it reads in the tree.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            State::Waiting => "waiting",
            State::Queued => "queued",
            State::Running => "running",
            State::Done => "done",
            State::Cached => "cached",
            State::NotNeeded => "not needed",
            State::Failed => "failed",
            State::Blocked => "blocked",
            State::Cancelled => "cancelled",
        }
    }

    /// Whether nothing more will happen to it in this run.
    #[must_use]
    pub fn is_over(self) -> bool {
        !matches!(self, State::Waiting | State::Queued | State::Running)
    }

    /// Whether its output is there: built or cached.
    #[must_use]
    pub fn is_built(self) -> bool {
        matches!(self, State::Done | State::Cached | State::NotNeeded)
    }
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunOutcome {
    /// Everything requested is built.
    Complete,
    /// Some task failed.
    Failed,
    /// Stopped on request; building again resumes.
    Stopped,
}

impl RunOutcome {
    /// How it reads.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            RunOutcome::Complete => "complete",
            RunOutcome::Failed => "failed",
            RunOutcome::Stopped => "stopped",
        }
    }
}

impl From<Outcome> for RunOutcome {
    fn from(o: Outcome) -> Self {
        match o {
            Outcome::Complete => RunOutcome::Complete,
            Outcome::Failed => RunOutcome::Failed,
            Outcome::Stopped => RunOutcome::Stopped,
        }
    }
}

/// The run: where, under which profile, for what.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunInfo {
    /// The machine running the scheduler.
    pub machine: String,
    /// The profile's name.
    pub profile: String,
    /// Its subprocesses' niceness.
    pub niceness: i32,
    /// The requested outputs' names (empty: everything).
    pub targets: Vec<String>,
    /// When it started, in Unix milliseconds.
    pub started_ms: u64,
    /// The process running it.
    pub pid: u32,
    /// Its event log, if one is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_log: Option<PathBuf>,
}

/// The budget in use against the profile's limit (ADR 0004 §3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Cores granted to running chunks.
    pub cores_in_use: u32,
    /// The profile's core budget.
    pub cores_limit: u32,
    /// Declared memory of running chunks, in bytes.
    pub memory_in_use: u64,
    /// The profile's memory budget, in bytes.
    pub memory_limit: u64,
}

/// A failed task, with what explains it (ADR 0007 §5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// The task's name.
    pub name: String,
    /// What failed.
    pub message: String,
    /// The last lines of its output.
    pub last_lines: Vec<String>,
    /// Its task log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<PathBuf>,
}

/// One node of the tree: a group (a scene path) or a task (an asset).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeView {
    /// Its own name: the last part of its path.
    pub label: String,
    /// Its full path: the group path, or the asset's name.
    pub path: String,
    /// Whether it is a task (else a group).
    pub task: bool,
    /// Its state; a group's aggregates its children's.
    pub state: State,
    /// Frames committed under the current key (ADR 0007 §4).
    pub done_frames: u64,
    /// Frames in all (0 for single outputs).
    pub total_frames: u64,
    /// Assets built or cached.
    pub done_assets: usize,
    /// Assets in all.
    pub total_assets: usize,
    /// Time since it started running, until it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// Estimated seconds until it is done, waiting for inputs included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_s: Option<f64>,
    /// Where it runs (several, comma-separated, for a group across
    /// machines).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// For a blocked task, the failed input it waits on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
    /// Why a task is being built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// A task's log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<PathBuf>,
    /// Whether this group is all cached, shown as one line without its
    /// children.
    #[serde(default)]
    pub collapsed: bool,
    /// Its children, in plan order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<NodeView>,
}

/// The tree at one moment: what the live view draws and the status file
/// holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// [`SNAPSHOT_FORMAT`].
    pub format: u32,
    /// The run.
    pub run: RunInfo,
    /// When it was taken, in Unix milliseconds.
    pub at_ms: u64,
    /// The run's wall time so far.
    pub elapsed_ms: u64,
    /// How the run ended, once it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<RunOutcome>,
    /// Whether a stop was requested.
    #[serde(default)]
    pub stopping: bool,
    /// The budget in use.
    pub budget: Budget,
    /// Estimated seconds until the run is done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_s: Option<f64>,
    /// The top of the tree.
    pub nodes: Vec<NodeView>,
    /// Failed tasks, in plan order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<Failure>,
    /// The newest warnings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Snapshot {
    /// The task node named `name`, anywhere in the tree (collapsed groups
    /// hide theirs).
    #[must_use]
    pub fn find(&self, path: &str) -> Option<&NodeView> {
        fn walk<'a>(nodes: &'a [NodeView], path: &str) -> Option<&'a NodeView> {
            nodes.iter().find_map(|n| {
                (n.path == path)
                    .then_some(n)
                    .or_else(|| walk(&n.children, path))
            })
        }
        walk(&self.nodes, path)
    }
}

#[derive(Clone, Debug)]
struct ChunkRun {
    frames: u64,
    estimate_s: f64,
    started_ms: Option<u64>,
    cores: u32,
    done: u64,
}

#[derive(Clone, Debug)]
struct TaskNode {
    tref: TaskRef,
    inputs: Vec<String>,
    frames: bool,
    total_frames: u64,
    estimate_s: f64,
    cores: u32,
    memory: u64,
    state: State,
    begun: bool,
    done_frames: u64,
    pending: HashMap<ChunkId, ChunkRun>,
    committed_ms: u64,
    committed_frames: u64,
    first_start_ms: Option<u64>,
    end_ms: Option<u64>,
    machines: BTreeSet<String>,
    why: Option<String>,
    log: Option<PathBuf>,
    blocked_by: Option<String>,
    failure: Option<Failure>,
}

impl TaskNode {
    fn new(tref: TaskRef) -> Self {
        Self {
            tref,
            inputs: Vec::new(),
            frames: false,
            total_frames: 0,
            estimate_s: 0.0,
            cores: 1,
            memory: 0,
            state: State::Waiting,
            begun: false,
            done_frames: 0,
            pending: HashMap::new(),
            committed_ms: 0,
            committed_frames: 0,
            first_start_ms: None,
            end_ms: None,
            machines: BTreeSet::new(),
            why: None,
            log: None,
            blocked_by: None,
            failure: None,
        }
    }

    fn running(&self) -> usize {
        self.pending
            .values()
            .filter(|c| c.started_ms.is_some())
            .count()
    }

    /// Back to queued or running from the chunks pending, unless over.
    fn refresh(&mut self) {
        if self.state.is_over() {
            return;
        }
        self.state = if self.running() > 0 {
            State::Running
        } else if !self.pending.is_empty() {
            State::Queued
        } else if self.first_start_ms.is_some() {
            State::Running
        } else {
            State::Waiting
        };
    }

    /// Seconds per frame: measured from this run's committed chunks, else
    /// the plan's estimate (from cost history where there is some).
    #[allow(clippy::cast_precision_loss)]
    fn per_frame_s(&self) -> f64 {
        if self.committed_frames > 0 {
            self.committed_ms as f64 / 1000.0 / self.committed_frames as f64
        } else if self.total_frames > 0 {
            self.estimate_s / self.total_frames as f64
        } else {
            self.estimate_s
        }
    }

    /// Seconds of work left, one chunk at a time.
    #[allow(clippy::cast_precision_loss)]
    fn remaining_s(&self, now_ms: u64) -> f64 {
        if self.state.is_over() {
            return 0.0;
        }
        let pf = self.per_frame_s();
        if !self.begun && self.pending.is_empty() {
            return if self.frames {
                self.total_frames.saturating_sub(self.done_frames) as f64 * pf
            } else {
                self.estimate_s
            };
        }
        self.pending
            .values()
            .map(|c| {
                let elapsed = c
                    .started_ms
                    .map_or(0.0, |s| now_ms.saturating_sub(s) as f64 / 1000.0);
                if c.frames > 0 && pf > 0.0 {
                    let frames = c.frames as f64;
                    let credit = if c.done > 0 {
                        (c.done as f64).min(frames)
                    } else {
                        (elapsed / pf).min(frames * 0.9)
                    };
                    (frames - credit) * pf
                } else {
                    let est = if c.estimate_s > 0.0 {
                        c.estimate_s
                    } else {
                        self.estimate_s
                    };
                    (est - elapsed).max(est * 0.1)
                }
            })
            .sum()
    }

    /// Seconds until it is done by itself, its running chunks in
    /// parallel.
    #[allow(clippy::cast_precision_loss)]
    fn own_eta_s(&self, now_ms: u64) -> f64 {
        self.remaining_s(now_ms) / self.running().max(1) as f64
    }

    fn elapsed_ms(&self, now_ms: u64) -> Option<u64> {
        self.first_start_ms
            .map(|s| self.end_ms.unwrap_or(now_ms).saturating_sub(s))
    }
}

/// The progress tree, fed scheduler events with [`apply`](Self::apply).
#[derive(Clone, Debug, Default)]
pub struct Tree {
    run: RunInfo,
    budget: Budget,
    finished_ms: Option<u64>,
    outcome: Option<RunOutcome>,
    stopping: bool,
    tasks: Vec<TaskNode>,
    by_id: HashMap<usize, usize>,
    by_name: HashMap<String, usize>,
    warnings: Vec<String>,
}

impl Tree {
    /// An empty tree.
    #[must_use]
    pub fn new() -> Self {
        Self {
            run: RunInfo {
                pid: std::process::id(),
                ..RunInfo::default()
            },
            ..Self::default()
        }
    }

    /// Records where the run's event log is.
    pub fn set_run_log(&mut self, path: PathBuf) {
        self.run.run_log = Some(path);
    }

    /// Whether the run has finished.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.outcome.is_some()
    }

    fn task(&mut self, t: &TaskRef) -> &mut TaskNode {
        let i = if let Some(&i) = self.by_id.get(&t.id) {
            i
        } else {
            let i = self.tasks.len();
            self.tasks.push(TaskNode::new(t.clone()));
            self.by_id.insert(t.id, i);
            self.by_name.insert(t.name.clone(), i);
            i
        };
        &mut self.tasks[i]
    }

    /// Folds in one event.
    #[allow(clippy::too_many_lines)]
    pub fn apply(&mut self, e: &Event) {
        let at = e.at_ms;
        match &e.kind {
            EventKind::RunStarted {
                machine,
                profile,
                targets,
            } => {
                self.run.machine.clone_from(machine);
                self.run.profile.clone_from(&profile.name);
                self.run.niceness = profile.niceness;
                self.run.targets.clone_from(targets);
                self.run.started_ms = at;
                self.budget.cores_limit = profile.cores;
                self.budget.memory_limit = profile.memory_bytes;
            }
            EventKind::ProfileChanged { profile } => {
                self.run.profile.clone_from(&profile.name);
                self.run.niceness = profile.niceness;
                self.budget.cores_limit = profile.cores;
                self.budget.memory_limit = profile.memory_bytes;
            }
            EventKind::Planned {
                task,
                inputs,
                shape,
                estimate_s,
                resources,
                ..
            } => {
                let t = self.task(task);
                t.inputs.clone_from(inputs);
                (t.frames, t.total_frames) = match shape {
                    Shape::Single => (false, 0),
                    Shape::Frames(iv) => (true, u64::try_from(iv.len()).unwrap_or(0)),
                };
                t.estimate_s = *estimate_s;
                t.cores = resources.cores.max(1);
                t.memory = resources.memory_bytes;
            }
            EventKind::KeyResolved { task, .. } => {
                self.task(task);
            }
            EventKind::Cached { task, .. } => {
                let t = self.task(task);
                t.state = State::Cached;
                t.done_frames = t.total_frames;
            }
            EventKind::Begun {
                task,
                why,
                done_frames,
                log,
                ..
            } => {
                let t = self.task(task);
                t.begun = true;
                t.why = Some(why.to_string());
                t.log = Some(log.clone());
                t.done_frames = *done_frames;
            }
            EventKind::Queued {
                task,
                chunk,
                estimate_s,
            } => {
                let t = self.task(task);
                t.pending.insert(
                    *chunk,
                    ChunkRun {
                        frames: chunk_frames(*chunk),
                        estimate_s: *estimate_s,
                        started_ms: None,
                        cores: t.cores,
                        done: 0,
                    },
                );
                t.refresh();
            }
            EventKind::Started {
                task,
                chunk,
                machine,
                cores,
                ..
            } => {
                let t = self.task(task);
                let c = t.pending.entry(*chunk).or_insert(ChunkRun {
                    frames: chunk_frames(*chunk),
                    estimate_s: 0.0,
                    started_ms: None,
                    cores: *cores,
                    done: 0,
                });
                c.started_ms = Some(at);
                c.cores = (*cores).max(1);
                t.first_start_ms.get_or_insert(at);
                t.machines.insert(machine.clone());
                t.refresh();
            }
            EventKind::Progress {
                task,
                chunk,
                done,
                total,
            } => {
                let t = self.task(task);
                if let Some(c) = t.pending.get_mut(chunk) {
                    c.done = if *total > 0 && c.frames > 0 && *total != c.frames {
                        done.saturating_mul(c.frames) / total
                    } else {
                        *done
                    };
                }
            }
            EventKind::ChunkCommitted {
                task, chunk, ms, ..
            } => {
                let t = self.task(task);
                t.pending.remove(chunk);
                let frames = chunk_frames(*chunk);
                t.done_frames = (t.done_frames + frames).min(t.total_frames.max(frames));
                if frames > 0 {
                    t.committed_ms += ms;
                    t.committed_frames += frames;
                }
                t.refresh();
            }
            EventKind::Finished { task, .. } => {
                let t = self.task(task);
                t.state = State::Done;
                t.done_frames = t.total_frames;
                t.pending.clear();
                t.end_ms = Some(at);
            }
            EventKind::Partial { task, .. } => {
                let t = self.task(task);
                t.state = State::Done;
                t.pending.clear();
                t.end_ms = Some(at);
            }
            EventKind::Failed {
                task,
                chunk,
                message,
                last_lines,
                log,
            } => {
                let t = self.task(task);
                if let Some(c) = chunk {
                    t.pending.remove(c);
                }
                let better = t
                    .failure
                    .as_ref()
                    .is_none_or(|f| f.last_lines.is_empty() && !last_lines.is_empty());
                if better {
                    t.failure = Some(Failure {
                        name: task.name.clone(),
                        message: message.clone(),
                        last_lines: last_lines.clone(),
                        log: log.clone().or_else(|| t.log.clone()),
                    });
                }
                t.state = State::Failed;
                t.end_ms = Some(at);
            }
            EventKind::Cancelled { task, chunk } => {
                let t = self.task(task);
                t.pending.remove(chunk);
                t.refresh();
            }
            EventKind::Blocked { task, by } => {
                let t = self.task(task);
                t.state = State::Blocked;
                t.blocked_by = Some(by.clone());
            }
            EventKind::NotNeeded { task } => {
                self.task(task).state = State::NotNeeded;
            }
            EventKind::Warning { task, message } => {
                self.warnings.push(match task {
                    Some(t) => format!("{}: {message}", t.name),
                    None => message.clone(),
                });
                if self.warnings.len() > KEEP_WARNINGS {
                    self.warnings.remove(0);
                }
            }
            EventKind::StopRequested => self.stopping = true,
            EventKind::Preview { .. } => {}
            EventKind::RunFinished { outcome, .. } => {
                let outcome = RunOutcome::from(*outcome);
                self.outcome = Some(outcome);
                self.finished_ms = Some(at);
                for t in &mut self.tasks {
                    t.pending.clear();
                    if !t.state.is_over()
                        && (outcome == RunOutcome::Stopped || t.first_start_ms.is_some())
                    {
                        t.state = State::Cancelled;
                        t.end_ms.get_or_insert(at);
                    }
                }
            }
        }
    }

    /// Seconds until each task is done, waiting for its inputs included.
    fn chain_etas(&self, now_ms: u64) -> Vec<f64> {
        let mut out: Vec<Option<f64>> = vec![None; self.tasks.len()];
        for i in 0..self.tasks.len() {
            self.chain(i, now_ms, &mut out, 0);
        }
        out.into_iter().map(|e| e.unwrap_or(0.0)).collect()
    }

    fn chain(&self, i: usize, now_ms: u64, memo: &mut Vec<Option<f64>>, depth: usize) -> f64 {
        if let Some(v) = memo[i] {
            return v;
        }
        let t = &self.tasks[i];
        if t.state.is_over() {
            memo[i] = Some(0.0);
            return 0.0;
        }
        let mut wait: f64 = 0.0;
        if depth < self.tasks.len() {
            for name in &t.inputs {
                if let Some(&j) = self.by_name.get(name)
                    && j != i
                {
                    wait = wait.max(self.chain(j, now_ms, memo, depth + 1));
                }
            }
        }
        let v = wait + t.own_eta_s(now_ms);
        memo[i] = Some(v);
        v
    }

    /// The tree as it stands at `now_ms`.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn snapshot(&self, now_ms: u64) -> Snapshot {
        let now = self.finished_ms.unwrap_or(now_ms);
        let chains = self.chain_etas(now);
        let mut budget = self.budget;
        let mut work = 0.0;
        for t in &self.tasks {
            for c in t.pending.values().filter(|c| c.started_ms.is_some()) {
                budget.cores_in_use += c.cores;
                budget.memory_in_use += t.memory;
            }
            work += t.remaining_s(now) * f64::from(t.cores);
        }
        let eta_s = if self.outcome.is_some() || self.tasks.is_empty() {
            None
        } else {
            let longest = chains.iter().copied().fold(0.0, f64::max);
            let spread = work / f64::from(budget.cores_limit.max(1));
            Some(longest.max(spread))
        };

        let mut root = Group::default();
        for (i, t) in self.tasks.iter().enumerate() {
            let (groups, _) = place(&t.tref);
            let mut g = &mut root;
            let mut path = String::new();
            for part in groups {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(&part);
                let found = g
                    .children
                    .iter()
                    .position(|c| matches!(c, Child::Group(sub) if sub.label == part));
                let at = found.unwrap_or_else(|| {
                    g.children.push(Child::Group(Group {
                        label: part.clone(),
                        path: path.clone(),
                        children: Vec::new(),
                    }));
                    g.children.len() - 1
                });
                let Child::Group(sub) = &mut g.children[at] else {
                    unreachable!("found as a group")
                };
                g = sub;
            }
            g.children.push(Child::Task(i));
        }
        let nodes = root
            .children
            .iter()
            .map(|c| self.view(c, now, &chains))
            .collect();

        Snapshot {
            format: SNAPSHOT_FORMAT,
            run: self.run.clone(),
            at_ms: now_ms,
            elapsed_ms: now.saturating_sub(self.run.started_ms),
            outcome: self.outcome,
            stopping: self.stopping,
            budget,
            eta_s,
            nodes,
            failures: self
                .tasks
                .iter()
                .filter_map(|t| t.failure.clone())
                .collect(),
            warnings: self.warnings.clone(),
        }
    }

    fn view(&self, c: &Child, now: u64, etas: &[f64]) -> NodeView {
        match c {
            Child::Task(i) => {
                let t = &self.tasks[*i];
                let (_, label) = place(&t.tref);
                NodeView {
                    label,
                    path: t.tref.name.clone(),
                    task: true,
                    state: t.state,
                    done_frames: t.done_frames,
                    total_frames: t.total_frames,
                    done_assets: usize::from(t.state.is_built()),
                    total_assets: 1,
                    elapsed_ms: t.elapsed_ms(now),
                    eta_s: (!t.state.is_over()).then_some(etas[*i]),
                    machine: (!t.machines.is_empty())
                        .then(|| t.machines.iter().cloned().collect::<Vec<_>>().join(",")),
                    blocked_by: t.blocked_by.clone(),
                    why: t.why.clone(),
                    log: t.log.clone(),
                    collapsed: false,
                    children: Vec::new(),
                }
            }
            Child::Group(g) => {
                let children: Vec<NodeView> =
                    g.children.iter().map(|c| self.view(c, now, etas)).collect();
                let mut tasks = Vec::new();
                g.tasks(&mut tasks);
                let ts: Vec<&TaskNode> = tasks.iter().map(|&i| &self.tasks[i]).collect();
                let state = aggregate(children.iter().map(|c| c.state));
                let start = ts.iter().filter_map(|t| t.first_start_ms).min();
                let all_over = ts.iter().all(|t| t.state.is_over());
                let end = if all_over {
                    ts.iter().filter_map(|t| t.end_ms).max()
                } else {
                    None
                };
                let machines: BTreeSet<&String> = ts.iter().flat_map(|t| &t.machines).collect();
                let collapsed = ts
                    .iter()
                    .all(|t| matches!(t.state, State::Cached | State::NotNeeded));
                NodeView {
                    label: g.label.clone(),
                    path: g.path.clone(),
                    task: false,
                    state,
                    done_frames: children.iter().map(|c| c.done_frames).sum(),
                    total_frames: children.iter().map(|c| c.total_frames).sum(),
                    done_assets: children.iter().map(|c| c.done_assets).sum(),
                    total_assets: children.iter().map(|c| c.total_assets).sum(),
                    elapsed_ms: start.map(|s| end.unwrap_or(now).saturating_sub(s)),
                    eta_s: children.iter().filter_map(|c| c.eta_s).reduce(f64::max),
                    machine: (!machines.is_empty())
                        .then(|| machines.into_iter().cloned().collect::<Vec<_>>().join(",")),
                    blocked_by: None,
                    why: None,
                    log: None,
                    collapsed,
                    children: if collapsed { Vec::new() } else { children },
                }
            }
        }
    }
}

#[derive(Default)]
struct Group {
    label: String,
    path: String,
    children: Vec<Child>,
}

enum Child {
    Group(Group),
    Task(usize),
}

impl Group {
    fn tasks(&self, out: &mut Vec<usize>) {
        for c in &self.children {
            match c {
                Child::Task(i) => out.push(*i),
                Child::Group(g) => g.tasks(out),
            }
        }
    }
}

/// A group's state from its children's: anything active shows, then
/// failure, then what is left to do; all cached reads as cached.
fn aggregate(states: impl Iterator<Item = State>) -> State {
    let states: Vec<State> = states.collect();
    let any = |s: State| states.contains(&s);
    for s in [
        State::Running,
        State::Queued,
        State::Failed,
        State::Waiting,
        State::Blocked,
        State::Cancelled,
    ] {
        if any(s) {
            return s;
        }
    }
    if states
        .iter()
        .all(|s| matches!(s, State::Cached | State::NotNeeded))
    {
        State::Cached
    } else {
        State::Done
    }
}

/// Where a task sits: its group path, and its label under it. The group
/// is the task's scene group when it has one, else its name's directory
/// (`s07/blender` sits under `s07`).
fn place(t: &TaskRef) -> (Vec<String>, String) {
    let split = |s: &str| -> Vec<String> {
        s.split('/')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect()
    };
    match &t.group {
        Some(g) if !g.is_empty() => {
            let prefix = format!("{}/", g.trim_end_matches('/'));
            let label = t.name.strip_prefix(&prefix).unwrap_or(&t.name).to_owned();
            (split(g), label)
        }
        _ => {
            let mut parts = split(&t.name);
            let label = parts.pop().unwrap_or_else(|| t.name.clone());
            (parts, label)
        }
    }
}

fn chunk_frames(c: ChunkId) -> u64 {
    c.frames()
        .map_or(0, |iv| u64::try_from(iv.len()).unwrap_or(0))
}
