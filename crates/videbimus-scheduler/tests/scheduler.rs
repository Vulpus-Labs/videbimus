//! The scheduler against real temporary stores, with fake tasks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use videbimus_core::{Frame, Interval};
use videbimus_scheduler::{
    Collect, Cost, EarlyCutoff, Event, EventKind, EventSink, Graph, Machine, Node, NodeId, Outcome,
    Profile, Resources, Scheduler, SchedulerOptions, Target, Task, TaskContext, TaskError,
    TaskState,
};
use videbimus_store::{ChunkId, KeyBuilder, Shape, Status, Store};

const MB: u64 = 1 << 20;

/// What fake tasks saw while running.
#[derive(Default)]
struct Probe {
    state: Mutex<ProbeState>,
    runs: AtomicUsize,
}

#[derive(Default)]
struct ProbeState {
    cores: u32,
    memory: u64,
    running: usize,
    /// (task, chunk, cores in use including it, memory in use, running)
    starts: Vec<(String, ChunkId, u32, u64, usize)>,
}

impl Probe {
    fn enter(&self, name: &str, chunk: ChunkId, res: Resources) {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let mut st = self.state.lock().unwrap();
        st.cores += res.cores;
        st.memory += res.memory_bytes;
        st.running += 1;
        let entry = (name.to_owned(), chunk, st.cores, st.memory, st.running);
        st.starts.push(entry);
    }

    fn exit(&self, r: Resources) {
        let mut s = self.state.lock().unwrap();
        s.cores -= r.cores;
        s.memory -= r.memory_bytes;
        s.running -= 1;
    }

    fn starts(&self) -> Vec<(String, ChunkId, u32, u64, usize)> {
        self.state.lock().unwrap().starts.clone()
    }

    fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct Fake {
    name: String,
    key_id: String,
    shape: Shape,
    resources: Resources,
    cost: Cost,
    config: String,
    content: String,
    hash: bool,
    chunk_frames: Option<u32>,
    sleep: Duration,
    shell: Option<String>,
    shell_from: i64,
    peak: Option<u64>,
    probe: Arc<Probe>,
}

impl Fake {
    fn new(name: &str, probe: &Arc<Probe>) -> Self {
        Self {
            name: name.to_owned(),
            key_id: name.to_owned(),
            shape: Shape::Single,
            resources: Resources::new(1, 0),
            cost: Cost::Seconds(1.0),
            config: "v1".to_owned(),
            content: "out".to_owned(),
            hash: false,
            chunk_frames: None,
            sleep: Duration::from_millis(5),
            shell: None,
            shell_from: i64::MIN,
            peak: None,
            probe: probe.clone(),
        }
    }
    fn frames(mut self, start: i64, end: i64, per_chunk: Option<u32>) -> Self {
        self.shape = Shape::Frames(Interval::new(Frame(start), Frame(end)));
        self.chunk_frames = per_chunk;
        self
    }
    fn res(mut self, cores: u32, memory: u64) -> Self {
        self.resources = Resources::new(cores, memory);
        self
    }
    fn cost(mut self, cost: Cost) -> Self {
        self.cost = cost;
        self
    }
    fn config(mut self, c: &str) -> Self {
        c.clone_into(&mut self.config);
        self
    }
    fn content(mut self, c: &str) -> Self {
        c.clone_into(&mut self.content);
        self
    }
    fn hashed(mut self) -> Self {
        self.hash = true;
        self
    }
    fn sleep_ms(mut self, ms: u64) -> Self {
        self.sleep = Duration::from_millis(ms);
        self
    }
    fn shell(mut self, script: &str) -> Self {
        self.shell = Some(script.to_owned());
        self
    }
}

impl Task for Fake {
    fn name(&self) -> &str {
        &self.name
    }
    fn shape(&self) -> Shape {
        self.shape
    }
    fn resources(&self) -> Resources {
        self.resources
    }
    fn cost(&self) -> Cost {
        self.cost
    }
    fn key(&self) -> KeyBuilder {
        KeyBuilder::new("fake").config(format!("{}:{}", self.key_id, self.config))
    }
    fn hash_output(&self) -> bool {
        self.hash
    }
    fn chunk_frames(&self) -> Option<u32> {
        self.chunk_frames
    }
    fn run(&self, ctx: &TaskContext<'_>) -> Result<(), TaskError> {
        struct Guard<'a>(&'a Probe, Resources);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.exit(self.1);
            }
        }
        self.probe.enter(&self.name, ctx.chunk(), self.resources);
        let _guard = Guard(&self.probe, self.resources);
        let until = Instant::now() + self.sleep;
        while Instant::now() < until {
            ctx.check_cancelled()?;
            std::thread::sleep(Duration::from_millis(2));
        }
        match ctx.frames() {
            Some(iv) => {
                for f in iv.start.0..iv.end.0 {
                    std::fs::write(ctx.out_dir().join(format!("{f:04}.txt")), &self.content)?;
                    ctx.progress(u64::try_from(f - iv.start.0 + 1).unwrap(), 0);
                }
            }
            None => std::fs::write(ctx.out_dir().join("out.txt"), &self.content)?,
        }
        if let Some(p) = self.peak {
            ctx.note_peak_memory(p);
        }
        let start = ctx.frames().map_or(0, |f| f.start.0);
        if let Some(script) = &self.shell
            && start >= self.shell_from
        {
            ctx.command(Command::new("sh").arg("-c").arg(script))?;
        }
        Ok(())
    }
}

fn store(dir: &Path) -> Store {
    Store::open(dir.join("store")).unwrap().0
}

fn machine() -> Machine {
    Machine {
        name: "test".to_owned(),
        cores: 8,
        memory_bytes: 8 << 30,
    }
}

fn profile(cores: u32, memory: u64) -> Profile {
    Profile::new("test", cores, memory, 0)
}

fn options() -> SchedulerOptions {
    SchedulerOptions {
        poll: Duration::from_millis(5),
        kill_grace: Duration::from_millis(200),
        load_probe: Some(Arc::new(|| Some(0.0))),
        contention_threshold_cores: Some(1.0),
        ..SchedulerOptions::default()
    }
}

fn scheduler(store: &Store, p: Profile, events: &Arc<Collect>) -> Scheduler {
    Scheduler::new(store.clone(), p)
        .with_machine(machine())
        .with_options(options())
        .with_sink(events.clone())
}

fn add(g: &mut Graph, t: Fake) -> NodeId {
    g.add(Node::new(t)).unwrap()
}

fn tagged<'a>(events: &'a [Event], tag: &str, name: &str) -> Vec<&'a EventKind> {
    events
        .iter()
        .filter(|e| e.kind.tag() == tag && e.kind.task().is_some_and(|t| t.name == name))
        .map(|e| &e.kind)
        .collect()
}

fn position(events: &[Event], tag: &str, name: &str) -> Option<usize> {
    events
        .iter()
        .position(|e| e.kind.tag() == tag && e.kind.task().is_some_and(|t| t.name == name))
}

fn started_names(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Started { task, .. } => Some(task.name.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn budget_packing_never_exceeds_cores_or_memory() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let probe = Arc::new(Probe::default());
    let mut g = Graph::new();
    for i in 0..12u32 {
        let t = Fake::new(&format!("t{i}"), &probe)
            .res(1 + i % 3, u64::from(200 + 100 * (i % 4)) * MB)
            .sleep_ms(25);
        add(&mut g, t);
    }
    add(
        &mut g,
        Fake::new("big", &probe).res(16, 64 << 30).sleep_ms(25),
    );
    let events = Arc::new(Collect::new());
    let report = scheduler(&store, profile(4, 1024 * MB), &events)
        .run(&g, &[])
        .unwrap();
    assert_eq!(report.outcome, Outcome::Complete);
    let starts = probe.starts();
    assert_eq!(starts.len(), 13);
    let mut packed = false;
    for (name, _, cores, memory, running) in &starts {
        if name == "big" {
            assert_eq!(*running, 1, "an oversized task runs alone");
        } else {
            assert!(*cores <= 4, "{name} started with {cores} cores in use");
            assert!(
                *memory <= 1024 * MB,
                "{name} started with {memory} bytes in use"
            );
            packed |= *running > 1;
        }
    }
    assert!(packed, "small tasks should run side by side");
    let ev = events.events();
    assert_eq!(tagged(&ev, "warning", "big").len(), 1);
    let granted = ev.iter().find_map(|e| match &e.kind {
        EventKind::Started {
            task, cores, alone, ..
        } if task.name == "big" => Some((*cores, *alone)),
        _ => None,
    });
    assert_eq!(granted, Some((4, true)));
}

#[test]
fn critical_path_goes_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let probe = Arc::new(Probe::default());
    let mut g = Graph::new();
    let a1 = add(&mut g, Fake::new("a1", &probe));
    let a2 = g
        .add(Node::new(Fake::new("a2", &probe)).input("in", a1))
        .unwrap();
    let a3 = g
        .add(Node::new(Fake::new("a3", &probe)).input("in", a2))
        .unwrap();
    let bs: Vec<NodeId> = (1..=3)
        .map(|i| {
            add(
                &mut g,
                Fake::new(&format!("b{i}"), &probe).cost(Cost::Seconds(1.5)),
            )
        })
        .collect();
    let mut targets = vec![Target::all(a3)];
    targets.extend(bs.iter().map(|&b| Target::all(b)));
    let events = Arc::new(Collect::new());
    scheduler(&store, profile(1, 1 << 30), &events)
        .run(&g, &targets)
        .unwrap();
    assert_eq!(
        started_names(&events.events()),
        ["a1", "a2", "b1", "b2", "b3", "a3"]
    );
}

#[test]
fn a_preview_builds_only_what_it_needs_and_jumps_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let probe = Arc::new(Probe::default());

    // A run whose target is a subset builds only that subset's needs.
    let mut g = Graph::new();
    let x = add(&mut g, Fake::new("x", &probe));
    let y = g
        .add(Node::new(Fake::new("y", &probe)).input("x", x))
        .unwrap();
    add(&mut g, Fake::new("z", &probe));
    let seq = add(&mut g, Fake::new("seq", &probe).frames(0, 40, Some(10)));
    let events = Arc::new(Collect::new());
    let s = scheduler(&store, profile(2, 1 << 30), &events);
    let report = s
        .run(
            &g,
            &[
                Target::all(y),
                Target::frames(seq, Interval::new(Frame(12), Frame(18))),
            ],
        )
        .unwrap();
    assert_eq!(report.task("x").unwrap().state, TaskState::Built);
    assert_eq!(report.task("y").unwrap().state, TaskState::Built);
    assert_eq!(report.task("z").unwrap().state, TaskState::NotRequested);
    assert_eq!(report.task("seq").unwrap().state, TaskState::Partial);
    let seq_key = report.task("seq").unwrap().key.unwrap();
    let ids: Vec<ChunkId> = store
        .committed(seq_key)
        .unwrap()
        .iter()
        .map(videbimus_store::ChunkRecord::id)
        .collect();
    assert_eq!(ids, [videbimus_store::frames(10, 20)]);

    // Mid-run, a preview jumps ahead of equal-ranked work.
    let mut g = Graph::new();
    let ps: Vec<NodeId> = (1..=5)
        .map(|i| add(&mut g, Fake::new(&format!("p{i}"), &probe).sleep_ms(10)))
        .collect();
    let events = Arc::new(Collect::new());
    let s = Scheduler::new(store.clone(), profile(1, 1 << 30))
        .with_machine(machine())
        .with_options(options());
    let control = s.control();
    let sink_events = events.clone();
    let p5 = ps[4];
    let asked = AtomicUsize::new(0);
    let s = s.with_sink(Arc::new(move |e: &Event| {
        sink_events.event(e);
        if e.kind.tag() == "started" && asked.fetch_add(1, Ordering::SeqCst) == 0 {
            control.preview([Target::all(p5)]);
        }
    }));
    s.run(&g, &[]).unwrap();
    assert_eq!(
        started_names(&events.events()),
        ["p1", "p5", "p2", "p3", "p4"]
    );
}

#[test]
fn cache_hits_skip_and_resolve_keys_lazily_with_early_cutoff() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let probe = Arc::new(Probe::default());
    let graph = |a_config: &str, a_content: &str| {
        let mut g = Graph::new();
        let a = add(
            &mut g,
            Fake::new("a", &probe)
                .frames(0, 6, Some(2))
                .config(a_config)
                .content(a_content)
                .hashed(),
        );
        g.add(Node::new(Fake::new("b", &probe)).input("a", a))
            .unwrap();
        let c = add(&mut g, Fake::new("c", &probe));
        g.add(Node::new(Fake::new("d", &probe)).input("c", c))
            .unwrap();
        g
    };

    let events = Arc::new(Collect::new());
    let s = scheduler(&store, profile(1, 1 << 30), &events);
    let report = s.run(&graph("v1", "same"), &[]).unwrap();
    assert_eq!(report.outcome, Outcome::Complete);
    let ev = events.events();
    // b's key needs a's output hash: resolved only after a finishes.
    assert!(position(&ev, "key-resolved", "b") > position(&ev, "finished", "a"));
    // d's input isn't hashed: its key is known before c is built.
    assert!(position(&ev, "key-resolved", "d") < position(&ev, "finished", "c"));
    assert_eq!(probe.runs(), 3 + 3);

    // Nothing changed: everything is a cache hit and nothing runs.
    events.clear();
    let report = s.run(&graph("v1", "same"), &[]).unwrap();
    assert!(report.tasks.iter().all(|t| t.state == TaskState::Cached));
    assert_eq!(probe.runs(), 6);
    assert_eq!(tagged(&events.events(), "cached", "a").len(), 1);

    // a's config changes but its output doesn't: a rebuilds, b stays
    // cached (early cutoff).
    events.clear();
    let report = s.run(&graph("v2", "same"), &[]).unwrap();
    assert_eq!(report.task("a").unwrap().state, TaskState::Built);
    assert_eq!(report.task("b").unwrap().state, TaskState::Cached);
    assert_eq!(report.task("d").unwrap().state, TaskState::Cached);
    let ev = events.events();
    assert!(matches!(
        tagged(&ev, "finished", "a")[0],
        EventKind::Finished {
            cutoff: EarlyCutoff::Same,
            ..
        }
    ));
    let why = tagged(&ev, "begun", "a");
    let EventKind::Begun { why, .. } = why[0] else {
        unreachable!()
    };
    assert_eq!(why.to_string(), "config changed");

    // Now its output changes too: b rebuilds.
    let report = s.run(&graph("v3", "different"), &[]).unwrap();
    assert_eq!(report.task("b").unwrap().state, TaskState::Built);
}

#[test]
fn an_input_whose_dependants_are_cached_is_not_built() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let probe = Arc::new(Probe::default());
    let graph = || {
        let mut g = Graph::new();
        let c = add(&mut g, Fake::new("c", &probe));
        g.add(Node::new(Fake::new("d", &probe)).input("c", c))
            .unwrap();
        g
    };
    let events = Arc::new(Collect::new());
    let s = scheduler(&store, profile(1, 1 << 30), &events);
    s.run(&graph(), &[]).unwrap();
    // Collect c's output (keep d's), as GC might after c's slot moved on.
    let c_key = store.latest("c").unwrap().unwrap();
    std::fs::remove_dir_all(store.key_dir(c_key)).unwrap();
    let report = s.run(&graph(), &[]).unwrap();
    assert_eq!(report.task("d").unwrap().state, TaskState::Cached);
    assert_eq!(report.task("c").unwrap().state, TaskState::NotNeeded);
    assert_eq!(probe.runs(), 2);
}

#[cfg(unix)]
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait_for(path: &Path, timeout: Duration) -> bool {
    let until = Instant::now() + timeout;
    while Instant::now() < until {
        if std::fs::read_to_string(path).is_ok_and(|s| !s.trim().is_empty()) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn read_pid(path: &Path) -> i32 {
    std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn stop_kills_the_subprocess_group_and_a_second_run_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let pids: PathBuf = dir.path().join("pids");
    std::fs::create_dir_all(&pids).unwrap();
    let probe = Arc::new(Probe::default());
    let script = format!(
        "sleep 30 & echo $! > {p}/bg.tmp; mv {p}/bg.tmp {p}/bg; echo $$ > {p}/sh; wait",
        p = pids.display()
    );
    let mut slow = Fake::new("shot", &probe)
        .frames(0, 60, Some(10))
        .shell(&script);
    slow.shell_from = 20;
    let mut g = Graph::new();
    add(&mut g, slow.clone());

    let events = Arc::new(Collect::new());
    let s = scheduler(&store, profile(1, 1 << 30), &events);
    let control = s.control();
    let bg = pids.join("bg");
    let stopper = std::thread::spawn(move || {
        assert!(
            wait_for(&bg, Duration::from_secs(10)),
            "the subprocess never started"
        );
        std::thread::sleep(Duration::from_millis(50));
        control.stop();
    });
    let t0 = Instant::now();
    let report = s.run(&g, &[]).unwrap();
    stopper.join().unwrap();
    assert!(t0.elapsed() < Duration::from_secs(10));
    assert_eq!(report.outcome, Outcome::Stopped);
    let shot = report.task("shot").unwrap();
    assert_eq!(shot.state, TaskState::Stopped);
    assert_eq!(shot.chunks_committed, 2);
    assert_eq!(shot.chunks_cancelled, 1);

    // The whole group is gone: the shell and its backgrounded sleep.
    let (sh, bg) = (read_pid(&pids.join("sh")), read_pid(&pids.join("bg")));
    let until = Instant::now() + Duration::from_secs(2);
    while (alive(sh) || alive(bg)) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !alive(sh) && !alive(bg),
        "the subprocess group survived a stop"
    );
    // The in-flight chunk was discarded.
    assert_eq!(
        std::fs::read_dir(store.root().join("tmp")).unwrap().count(),
        0
    );
    let key = shot.key.unwrap();
    assert_eq!(
        store.manifest(key).unwrap().unwrap().status,
        Status::Building
    );
    let ev = events.events();
    assert_eq!(tagged(&ev, "cancelled", "shot").len(), 1);
    let started_first = tagged(&ev, "started", "shot").len();
    assert!(
        started_first - shot.chunks_committed <= 1,
        "lost more than one chunk"
    );

    // Resume: the same key, only the missing chunks run.
    events.clear();
    let mut fast = slow;
    fast.shell = None;
    let mut g = Graph::new();
    add(&mut g, fast);
    let report = s.run(&g, &[]).unwrap();
    assert_eq!(report.outcome, Outcome::Complete);
    assert_eq!(report.task("shot").unwrap().key, Some(key));
    let ev = events.events();
    let chunks: Vec<String> = ev
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Started { chunk, .. } => Some(chunk.dir_name()),
            _ => None,
        })
        .collect();
    assert_eq!(chunks, ["f20_30", "f30_40", "f40_50", "f50_60"]);
    let EventKind::Begun {
        why, done_frames, ..
    } = tagged(&ev, "begun", "shot")[0]
    else {
        unreachable!()
    };
    assert_eq!(*done_frames, 20);
    assert_eq!(why.to_string(), "first build");
    assert_eq!(
        store.manifest(key).unwrap().unwrap().status,
        Status::Complete
    );
}

#[test]
fn measured_history_replaces_estimates_unless_contended() {
    for contended in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let probe = Arc::new(Probe::default());
        let task = |config: &str| {
            let mut t = Fake::new("seq", &probe)
                .frames(0, 10, None)
                .cost(Cost::PerFrame(100.0))
                .res(1, 64 * MB)
                .config(config);
            t.peak = Some(123_456);
            t
        };
        let events = Arc::new(Collect::new());
        let load = if contended { 50.0 } else { 0.5 };
        let opts = SchedulerOptions {
            load_probe: Some(Arc::new(move || Some(load))),
            ..options()
        };
        let s = scheduler(&store, profile(1, 1 << 30), &events).with_options(opts);
        let mut g = Graph::new();
        add(&mut g, task("v1"));
        let report = s.run(&g, &[]).unwrap();
        // 120 s chunks of 100 s frames: 2 frames each.
        assert_eq!(tagged(&events.events(), "queued", "seq").len(), 5);
        let m = store
            .manifest(report.task("seq").unwrap().key.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(m.peak_memory_bytes, Some(123_456));
        assert_eq!(m.external_load_pct, Some(if contended { 5000 } else { 50 }));

        events.clear();
        let mut g = Graph::new();
        add(&mut g, task("v2"));
        s.run(&g, &[]).unwrap();
        let ev = events.events();
        let EventKind::Planned {
            from_history,
            estimate_s,
            resources,
            ..
        } = tagged(&ev, "planned", "seq")[0]
        else {
            unreachable!()
        };
        let queued = tagged(&ev, "queued", "seq").len();
        if contended {
            assert!(!from_history);
            assert!((estimate_s - 1000.0).abs() < 1e-9);
            assert_eq!(resources.memory_bytes, 64 * MB);
            assert_eq!(queued, 5);
        } else {
            assert!(from_history);
            assert!(*estimate_s < 10.0, "measured {estimate_s} s");
            assert_eq!(resources.memory_bytes, 123_456);
            assert_eq!(queued, 1);
        }
    }
}

#[cfg(unix)]
#[test]
fn subprocesses_get_niceness_thread_caps_and_a_full_log() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let probe = Arc::new(Probe::default());
    let mut g = Graph::new();
    let ok = add(
        &mut g,
        Fake::new("ok", &probe).res(3, 0).shell(
            "echo nice=$(ps -o nice= -p $$ | tr -d ' '); echo threads=$OMP_NUM_THREADS; echo to-stderr >&2",
        ),
    );
    let bad = add(
        &mut g,
        Fake::new("bad", &probe).shell("echo first; echo boom >&2; exit 3"),
    );
    g.add(Node::new(Fake::new("after", &probe)).input("bad", bad))
        .unwrap();
    let events = Arc::new(Collect::new());
    let s = scheduler(&store, Profile::new("test", 4, 1 << 30, 7), &events);
    let report = s.run(&g, &[]).unwrap();
    assert_eq!(report.outcome, Outcome::Failed);

    let key = report.task("ok").unwrap().key.unwrap();
    let log = std::fs::read_to_string(store.log_path(key)).unwrap();
    assert!(log.contains("why: first build"), "{log}");
    assert!(log.contains("$ \"sh\" \"-c\""), "{log}");
    // A process can't lower its niceness, so under `nice cargo test` the
    // child keeps the higher of the profile's 7 and the test's own.
    let own = std::process::Command::new("ps")
        .args(["-o", "nice=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(0);
    let want = format!("| nice={}", own.max(7));
    assert!(log.contains(&want), "{want} in {log}");
    assert!(log.contains("| threads=3"), "{log}");
    assert!(log.contains("! to-stderr"), "{log}");
    assert!(log.contains("exit: exit status: 0"), "{log}");
    let m = store.manifest(key).unwrap().unwrap();
    assert!(m.peak_memory_bytes.is_some_and(|p| p > 0));
    let _ = ok;

    assert!(matches!(
        report.task("bad").unwrap().state,
        TaskState::Failed(_)
    ));
    assert_eq!(
        report.task("after").unwrap().state,
        TaskState::Blocked("bad".to_owned())
    );
    let ev = events.events();
    let failed = tagged(&ev, "failed", "bad");
    let EventKind::Failed {
        last_lines, log, ..
    } = failed[0]
    else {
        unreachable!()
    };
    // stdout and stderr are read separately, so their order may vary.
    let mut lines = last_lines.clone();
    lines.sort();
    assert_eq!(lines, ["boom", "first"]);
    assert!(log.is_some());
    let bad_key = report.task("bad").unwrap().key.unwrap();
    assert!(matches!(
        store.manifest(bad_key).unwrap().unwrap().status,
        Status::Failed { .. }
    ));
}

#[test]
fn switching_profile_applies_to_tasks_started_afterwards() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let probe = Arc::new(Probe::default());
    let mut g = Graph::new();
    for i in 0..6 {
        add(&mut g, Fake::new(&format!("t{i}"), &probe).sleep_ms(20));
    }
    let events = Arc::new(Collect::new());
    let s = Scheduler::new(store.clone(), profile(2, 1 << 30))
        .with_machine(machine())
        .with_options(options());
    let control = s.control();
    let sink_events = events.clone();
    let switched = AtomicUsize::new(0);
    let s = s.with_sink(Arc::new(move |e: &Event| {
        sink_events.event(e);
        if e.kind.tag() == "started" && switched.fetch_add(1, Ordering::SeqCst) == 1 {
            control.set_profile(Profile::new("narrow", 1, 1 << 30, 3));
        }
    }));
    s.run(&g, &[]).unwrap();
    let starts = probe.starts();
    assert_eq!(starts[0].4 + starts[1].4, 1 + 2, "two run at first");
    for (name, _, _, _, running) in &starts[2..] {
        assert_eq!(
            *running, 1,
            "{name} started beside another after the switch"
        );
    }
    let ev = events.events();
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind.tag() == "profile-changed")
            .count(),
        1
    );
    let nices: Vec<i32> = ev
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Started { niceness, .. } => Some(*niceness),
            _ => None,
        })
        .collect();
    assert_eq!(nices, [0, 0, 3, 3, 3, 3]);
}

#[test]
fn events_arrive_in_a_valid_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let probe = Arc::new(Probe::default());
    let build = |fail: bool| {
        let mut g = Graph::new();
        let a = g
            .add(
                Node::new(Fake::new("a", &probe).frames(0, 30, Some(10)).hashed())
                    .group("scenes/s01"),
            )
            .unwrap();
        g.add(
            Node::new(Fake::new("b", &probe).frames(0, 8, Some(3)))
                .input("a", a)
                .group("scenes/s01"),
        )
        .unwrap();
        let mut f = Fake::new("f", &probe);
        if fail {
            f.shell = Some("exit 1".to_owned());
        }
        let f = add(&mut g, f);
        g.add(Node::new(Fake::new("g", &probe)).input("f", f))
            .unwrap();
        g
    };
    let events = Arc::new(Collect::new());
    let s = scheduler(&store, profile(3, 1 << 30), &events);
    s.run(&build(true), &[]).unwrap();
    let first = events.events();
    events.clear();
    s.run(&build(false), &[]).unwrap();
    let second = events.events();

    for ev in [&first, &second] {
        assert_eq!(ev.first().unwrap().kind.tag(), "run-started");
        assert_eq!(ev.last().unwrap().kind.tag(), "run-finished");
        let mut per_task: HashMap<String, Vec<&Event>> = HashMap::new();
        for e in ev {
            let line = serde_json::to_string(e).unwrap();
            assert!(
                line.contains(&format!("\"event\":\"{}\"", e.kind.tag())),
                "{line}"
            );
            if let Some(t) = e.kind.task() {
                per_task.entry(t.name.clone()).or_default().push(e);
            }
        }
        for (name, list) in &per_task {
            check_task_events(name, list);
        }
    }
    assert!(first.iter().any(|e| matches!(&e.kind,
        EventKind::Planned { task, .. } if task.group.as_deref() == Some("scenes/s01"))));
    assert_eq!(tagged(&first, "blocked", "g").len(), 1);
    assert_eq!(tagged(&second, "cached", "a").len(), 1);
    assert_eq!(tagged(&second, "finished", "g").len(), 1);
}

/// Checks one task's events against the order documented on `event`.
fn check_task_events(name: &str, list: &[&Event]) {
    let tags: Vec<&str> = list.iter().map(|e| e.kind.tag()).collect();
    assert_eq!(tags[0], "planned", "{name}: {tags:?}");
    let mut state = "planned";
    let mut open: HashMap<String, &str> = HashMap::new();
    for e in &list[1..] {
        let tag = e.kind.tag();
        let chunk = match &e.kind {
            EventKind::Queued { chunk, .. }
            | EventKind::Started { chunk, .. }
            | EventKind::Progress { chunk, .. }
            | EventKind::ChunkCommitted { chunk, .. }
            | EventKind::Cancelled { chunk, .. }
            | EventKind::Failed {
                chunk: Some(chunk), ..
            } => Some(chunk.dir_name()),
            _ => None,
        };
        if let Some(c) = chunk {
            assert_eq!(
                state, "begun",
                "{name}: chunk event {tag} outside a build: {tags:?}"
            );
            let prev = open.get(&c).copied();
            let ok = match tag {
                "queued" => prev.is_none(),
                "started" => prev == Some("queued"),
                "progress" => matches!(prev, Some("started" | "progress")),
                "chunk-committed" | "failed" | "cancelled" => {
                    matches!(prev, Some("started" | "progress"))
                }
                _ => false,
            };
            assert!(ok, "{name}: {tag} after {prev:?} for {c}: {tags:?}");
            open.insert(c, tag);
            continue;
        }
        let ok = matches!(
            (state, tag),
            (
                "planned",
                "key-resolved" | "blocked" | "not-needed" | "failed"
            ) | (
                "key-resolved",
                "cached" | "begun" | "blocked" | "not-needed" | "failed"
            ) | ("begun", "finished" | "failed" | "partial")
                | (_, "warning")
        );
        assert!(ok, "{name}: {tag} after {state}: {tags:?}");
        if tag != "warning" {
            state = tag;
        }
        if tag == "finished" {
            assert!(
                open.values().all(|s| *s == "chunk-committed"),
                "{name}: finished with chunks open: {open:?}"
            );
        }
    }
}

#[test]
fn tasks_sharing_a_key_build_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let probe = Arc::new(Probe::default());
    let mut g = Graph::new();
    for name in ["s01/backdrop", "s02/backdrop"] {
        let mut t = Fake::new(name, &probe).frames(0, 6, Some(2)).sleep_ms(10);
        t.key_id = "backdrop".to_owned();
        add(&mut g, t);
    }
    let events = Arc::new(Collect::new());
    let report = scheduler(&store, profile(4, 1 << 30), &events)
        .run(&g, &[])
        .unwrap();
    assert_eq!(report.outcome, Outcome::Complete);
    let states: Vec<&TaskState> = report.tasks.iter().map(|t| &t.state).collect();
    assert_eq!(states, [&TaskState::Built, &TaskState::Cached]);
    assert_eq!(probe.runs(), 3);
    assert_eq!(store.latest("s02/backdrop").unwrap(), report.tasks[0].key);
}

#[test]
fn graphs_reject_bad_nodes_with_the_fix() {
    let probe = Arc::new(Probe::default());
    let mut g = Graph::new();
    let a = add(&mut g, Fake::new("a", &probe));
    let err = g.add(Node::new(Fake::new("a", &probe))).unwrap_err();
    assert!(err.to_string().contains("two tasks are named `a`"), "{err}");
    let err = g
        .add(
            Node::new(Fake::new("b", &probe))
                .input("x", a)
                .input("x", a),
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("two inputs are labelled `x`"),
        "{err}"
    );
    let mut other = Graph::new();
    add(&mut other, Fake::new("p", &probe));
    let p2 = add(&mut other, Fake::new("q", &probe));
    let err = g
        .add(Node::new(Fake::new("c", &probe)).input("in", p2))
        .unwrap_err();
    assert!(
        err.to_string().contains("add the input's task first"),
        "{err}"
    );
    assert_eq!(g.sinks(), [a]);
}
