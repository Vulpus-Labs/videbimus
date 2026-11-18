//! The progress tree from scripted events, its rendering, and recording a
//! run: event log, status file, plain and live views.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use videbimus_core::{AssetKey, Frame, Interval};
use videbimus_progress::{
    Recorder, RecorderOptions, RunOutcome, Snapshot, State, Tree, View, latest_run, read_status,
    render, status_path,
};
use videbimus_scheduler::{Event, EventKind, Outcome, Profile, Resources, TaskRef};
use videbimus_store::{ChunkId, Shape, Why};

const GIB: u64 = 1 << 30;

fn tref(id: usize, name: &str, group: Option<&str>) -> TaskRef {
    TaskRef {
        id,
        name: name.to_owned(),
        group: group.map(str::to_owned),
    }
}

fn frames(a: i64, b: i64) -> Interval {
    Interval::new(Frame(a), Frame(b))
}

fn chunk(a: i64, b: i64) -> ChunkId {
    ChunkId::Frames(frames(a, b))
}

fn ev(at_ms: u64, kind: EventKind) -> Event {
    Event { at_ms, kind }
}

fn key(n: u8) -> AssetKey {
    AssetKey([n; 32])
}

fn planned(t: &TaskRef, inputs: &[&str], shape: Shape, est: f64, cores: u32, mem: u64) -> Event {
    ev(
        0,
        EventKind::Planned {
            task: t.clone(),
            inputs: inputs.iter().map(|s| (*s).to_owned()).collect(),
            shape,
            estimate_s: est,
            resources: Resources::new(cores, mem),
            from_history: false,
        },
    )
}

struct Script {
    panel: TaskRef,
    blender: TaskRef,
    plate: TaskRef,
    grade: TaskRef,
    comp: TaskRef,
}

impl Script {
    fn new() -> Self {
        Self {
            panel: tref(0, "s07/panel", Some("s07")),
            blender: tref(1, "s07/blender", Some("s07")),
            plate: tref(2, "s08/plate", None),
            grade: tref(3, "s08/grade", None),
            comp: tref(4, "s09/comp", None),
        }
    }

    /// The run up to `until_ms`, in order.
    #[allow(clippy::too_many_lines)]
    fn events(&self, until_ms: u64) -> Vec<Event> {
        let s = self;
        let log = PathBuf::from("/store/b1/log.txt");
        let all = vec![
            ev(
                0,
                EventKind::RunStarted {
                    machine: "m1".to_owned(),
                    profile: Profile::new("background", 4, 8 * GIB, 15),
                    targets: vec!["s09/comp".to_owned()],
                },
            ),
            planned(&s.panel, &[], Shape::Single, 10.0, 1, GIB),
            planned(
                &s.blender,
                &["s07/panel"],
                Shape::Frames(frames(0, 100)),
                200.0,
                2,
                2 * GIB,
            ),
            planned(&s.plate, &[], Shape::Single, 3.0, 1, 0),
            planned(
                &s.grade,
                &["s08/plate"],
                Shape::Frames(frames(0, 10)),
                5.0,
                1,
                0,
            ),
            planned(
                &s.comp,
                &["s07/blender", "s08/grade"],
                Shape::Single,
                5.0,
                1,
                0,
            ),
            ev(
                0,
                EventKind::Cached {
                    task: s.plate.clone(),
                    key: key(2),
                },
            ),
            ev(
                0,
                EventKind::Cached {
                    task: s.grade.clone(),
                    key: key(3),
                },
            ),
            ev(
                0,
                EventKind::KeyResolved {
                    task: s.panel.clone(),
                    key: key(0),
                },
            ),
            ev(
                0,
                EventKind::Begun {
                    task: s.panel.clone(),
                    key: key(0),
                    why: Why::New,
                    chunks: 1,
                    done_frames: 0,
                    total_frames: 0,
                    log: PathBuf::from("/store/p0/log.txt"),
                },
            ),
            ev(
                0,
                EventKind::Queued {
                    task: s.panel.clone(),
                    chunk: ChunkId::Whole,
                    estimate_s: 10.0,
                },
            ),
            ev(
                0,
                EventKind::Started {
                    task: s.panel.clone(),
                    chunk: ChunkId::Whole,
                    machine: "m1".to_owned(),
                    cores: 1,
                    niceness: 15,
                    alone: false,
                },
            ),
            ev(
                10_000,
                EventKind::ChunkCommitted {
                    task: s.panel.clone(),
                    chunk: ChunkId::Whole,
                    ms: 10_000,
                    peak_memory_bytes: None,
                },
            ),
            ev(
                10_000,
                EventKind::Finished {
                    task: s.panel.clone(),
                    key: key(0),
                    build_ms: Some(10_000),
                    peak_memory_bytes: None,
                    cutoff: videbimus_scheduler::EarlyCutoff::NotHashed,
                },
            ),
            ev(
                10_000,
                EventKind::KeyResolved {
                    task: s.blender.clone(),
                    key: key(1),
                },
            ),
            ev(
                10_000,
                EventKind::Begun {
                    task: s.blender.clone(),
                    key: key(1),
                    why: Why::Resume,
                    chunks: 2,
                    done_frames: 20,
                    total_frames: 100,
                    log: log.clone(),
                },
            ),
            ev(
                10_000,
                EventKind::Queued {
                    task: s.blender.clone(),
                    chunk: chunk(20, 60),
                    estimate_s: 80.0,
                },
            ),
            ev(
                10_000,
                EventKind::Queued {
                    task: s.blender.clone(),
                    chunk: chunk(60, 100),
                    estimate_s: 80.0,
                },
            ),
            ev(
                11_000,
                EventKind::Started {
                    task: s.blender.clone(),
                    chunk: chunk(20, 60),
                    machine: "m1".to_owned(),
                    cores: 2,
                    niceness: 15,
                    alone: false,
                },
            ),
            ev(
                11_000,
                EventKind::Started {
                    task: s.blender.clone(),
                    chunk: chunk(60, 100),
                    machine: "m1".to_owned(),
                    cores: 2,
                    niceness: 15,
                    alone: false,
                },
            ),
            ev(
                30_000,
                EventKind::Progress {
                    task: s.blender.clone(),
                    chunk: chunk(20, 60),
                    done: 20,
                    total: 40,
                },
            ),
            ev(
                50_000,
                EventKind::ChunkCommitted {
                    task: s.blender.clone(),
                    chunk: chunk(20, 60),
                    ms: 39_000,
                    peak_memory_bytes: Some(GIB),
                },
            ),
            ev(
                55_000,
                EventKind::Failed {
                    task: s.blender.clone(),
                    chunk: Some(chunk(60, 100)),
                    message: "blender exited with status 1".to_owned(),
                    last_lines: vec!["Error: out of memory".to_owned(), "Blender quit".to_owned()],
                    log: Some(log.clone()),
                },
            ),
            ev(
                55_000,
                EventKind::Failed {
                    task: s.blender.clone(),
                    chunk: None,
                    message: "a chunk failed".to_owned(),
                    last_lines: Vec::new(),
                    log: Some(log),
                },
            ),
            ev(
                55_000,
                EventKind::Blocked {
                    task: s.comp.clone(),
                    by: "s07/blender".to_owned(),
                },
            ),
            ev(
                60_000,
                EventKind::RunFinished {
                    outcome: Outcome::Failed,
                    built: 1,
                    cached: 2,
                    failed: 1,
                    chunks: 2,
                    ms: 60_000,
                },
            ),
        ];
        all.into_iter().filter(|e| e.at_ms <= until_ms).collect()
    }

    fn tree(&self, until_ms: u64) -> Tree {
        let mut t = Tree::new();
        for e in self.events(until_ms) {
            t.apply(&e);
        }
        t
    }
}

fn node<'a>(s: &'a Snapshot, path: &str) -> &'a videbimus_progress::NodeView {
    s.find(path)
        .unwrap_or_else(|| panic!("no node {path} in {s:#?}"))
}

#[test]
fn states_and_hierarchy() {
    let sc = Script::new();
    let s = sc.tree(5_000).snapshot(5_000);
    let tops: Vec<&str> = s.nodes.iter().map(|n| n.label.as_str()).collect();
    assert_eq!(tops, ["s07", "s08", "s09"]);
    let s07 = node(&s, "s07");
    assert!(!s07.task);
    assert_eq!(s07.state, State::Running);
    let labels: Vec<&str> = s07.children.iter().map(|n| n.label.as_str()).collect();
    assert_eq!(labels, ["panel", "blender"]);
    assert_eq!(node(&s, "s07/panel").state, State::Running);
    assert_eq!(node(&s, "s07/panel").machine.as_deref(), Some("m1"));
    assert_eq!(node(&s, "s07/blender").state, State::Waiting);
    assert_eq!(node(&s, "s09/comp").state, State::Waiting);
    // the budget: the panel's one core and its memory
    assert_eq!(s.budget.cores_in_use, 1);
    assert_eq!(s.budget.cores_limit, 4);
    assert_eq!(s.budget.memory_in_use, GIB);
    assert_eq!(s.budget.memory_limit, 8 * GIB);
    assert_eq!(s.run.profile, "background");
    assert_eq!(node(&s, "s07/panel").elapsed_ms, Some(5_000));

    // a cached subtree collapses to one line
    let s08 = node(&s, "s08");
    assert_eq!(s08.state, State::Cached);
    assert!(s08.collapsed);
    assert!(s08.children.is_empty());
    assert_eq!((s08.done_assets, s08.total_assets), (2, 2));
    assert_eq!((s08.done_frames, s08.total_frames), (10, 10));

    // both blender chunks running: progress counts committed frames only
    let s = sc.tree(30_000).snapshot(30_000);
    let b = node(&s, "s07/blender");
    assert_eq!(b.state, State::Running);
    assert_eq!(
        (b.done_frames, b.total_frames),
        (20, 100),
        "resumed with 20 committed; in-flight frames don't count"
    );
    assert_eq!(b.why.as_deref(), Some("resuming"));
    assert_eq!(s.budget.cores_in_use, 4);
    assert_eq!(s.budget.memory_in_use, 4 * GIB);
    let s07 = node(&s, "s07");
    assert_eq!((s07.done_assets, s07.total_assets), (1, 2));
    assert_eq!(s07.elapsed_ms, Some(30_000));

    let s = sc.tree(50_000).snapshot(50_000);
    assert_eq!(node(&s, "s07/blender").done_frames, 60);
}

#[test]
fn failure_blocks_dependants() {
    let sc = Script::new();
    let s = sc.tree(u64::MAX).snapshot(70_000);
    assert_eq!(s.outcome, Some(RunOutcome::Failed));
    assert_eq!(s.eta_s, None);
    assert_eq!(s.elapsed_ms, 60_000, "a finished run's clock stops");
    let b = node(&s, "s07/blender");
    assert_eq!(b.state, State::Failed);
    assert_eq!(b.done_frames, 60);
    assert_eq!(b.elapsed_ms, Some(44_000));
    let c = node(&s, "s09/comp");
    assert_eq!(c.state, State::Blocked);
    assert_eq!(c.blocked_by.as_deref(), Some("s07/blender"));
    assert_eq!(node(&s, "s07").state, State::Failed);
    assert_eq!(s.failures.len(), 1);
    let f = &s.failures[0];
    assert_eq!(f.name, "s07/blender");
    assert_eq!(
        f.message, "blender exited with status 1",
        "the failure with output wins"
    );
    assert_eq!(f.last_lines, ["Error: out of memory", "Blender quit"]);
    assert_eq!(
        f.log.as_deref(),
        Some(std::path::Path::new("/store/b1/log.txt"))
    );
    assert_eq!(s.budget.cores_in_use, 0);
}

#[test]
fn eta_falls_as_work_is_done() {
    let sc = Script::new();
    let mut last = f64::INFINITY;
    for at in (0..=50_000).step_by(2_500) {
        let s = sc.tree(at).snapshot(at);
        let eta = s.eta_s.expect("an eta while running");
        assert!(eta <= last + 1e-6, "eta rose at {at}: {last} -> {eta}");
        last = eta;
    }
    // at the start: panel 10 s, then blender's 80 frames at 2 s/frame on
    // two lanes, then comp's 5 s
    let s = sc.tree(0).snapshot(0);
    assert!((s.eta_s.unwrap() - 215.0).abs() < 1e-6, "{:?}", s.eta_s);
    // once a chunk commits, the measured rate replaces the estimate
    let s = sc.tree(50_000).snapshot(50_000);
    assert!(s.eta_s.unwrap() < 10.0, "{:?}", s.eta_s);
    // a node's eta includes its inputs'
    let s = sc.tree(11_000).snapshot(11_000);
    let b = node(&s, "s07/blender").eta_s.unwrap();
    let c = node(&s, "s09/comp").eta_s.unwrap();
    assert!((c - b - 5.0).abs() < 1e-6, "{b} {c}");
}

#[test]
fn stopping_cancels_what_was_running() {
    let sc = Script::new();
    let mut t = sc.tree(30_000);
    t.apply(&ev(31_000, EventKind::StopRequested));
    assert!(t.snapshot(31_000).stopping);
    for c in [chunk(20, 60), chunk(60, 100)] {
        t.apply(&ev(
            32_000,
            EventKind::Cancelled {
                task: sc.blender.clone(),
                chunk: c,
            },
        ));
    }
    t.apply(&ev(
        32_000,
        EventKind::RunFinished {
            outcome: Outcome::Stopped,
            built: 1,
            cached: 2,
            failed: 0,
            chunks: 1,
            ms: 32_000,
        },
    ));
    let s = t.snapshot(40_000);
    assert_eq!(node(&s, "s07/blender").state, State::Cancelled);
    assert_eq!(node(&s, "s09/comp").state, State::Cancelled);
    assert_eq!(node(&s, "s07/blender").done_frames, 20);
    assert!(render(&s, 80, false).starts_with("stopped in 32s"));
}

const RENDER_30S: &str = "\
building · profile background (nice 15) · m1 · s09/comp
cores 4/4 · memory 4.0G/8.0G · elapsed 30s · eta 55s
s07         running        20/100 f     30s    eta 50s  m1
  panel     done                        10s             m1
  blender   running        20/100 f     19s    eta 50s  m1
s08         cached    (2 assets)
s09         waiting             0/1            eta 55s
  comp      waiting                            eta 55s
";

#[test]
fn renders_at_a_fixed_width() {
    let sc = Script::new();
    let s = sc.tree(30_000).snapshot(30_000);
    assert_eq!(render(&s, 80, false), RENDER_30S);
    // narrower: every line fits
    for l in render(&s, 40, false).lines() {
        assert!(l.chars().count() <= 40, "{l}");
    }
    // colour wraps only the state words
    let c = render(&s, 80, true);
    assert!(c.contains("\x1b[33mrunning\x1b[0m"), "{c}");
    assert_eq!(strip_ansi(&c), RENDER_30S);
}

const RENDER_END: &str = "\
failed in 1m00s · profile background (nice 15) · m1 · s09/comp
cores 0/4 · memory 0M/8.0G
s07         failed         60/100 f     55s             m1
  panel     done                        10s             m1
  blender   failed         60/100 f     44s             m1
    | blender exited with status 1
    | Error: out of memory
    | Blender quit
    log: /store/b1/log.txt
s08         cached    (2 assets)
s09         blocked             0/1
  comp      blocked   by s07/blender
";

#[test]
fn renders_failures_with_their_output() {
    let sc = Script::new();
    let s = sc.tree(u64::MAX).snapshot(70_000);
    assert_eq!(render(&s, 80, false), RENDER_END);
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            for d in it.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn fast() -> RecorderOptions {
    RecorderOptions {
        redraw: Duration::from_millis(5),
        status: Duration::from_millis(5),
        keep_runs: 50,
    }
}

#[test]
fn status_file_is_rewritten_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().to_owned();
    let rec = Recorder::with_options(&store, View::Quiet, fast()).unwrap();
    let sink = rec.sink();
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let (store, stop) = (store.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut seen = std::collections::BTreeSet::new();
            while !stop.load(Ordering::SeqCst) {
                match read_status(&store) {
                    Ok(Some(s)) => {
                        seen.insert(s.at_ms);
                    }
                    Ok(None) => {}
                    Err(e) => panic!("a torn status file: {e}"),
                }
            }
            seen.len()
        })
    };
    let events = Script::new().events(u64::MAX);
    for e in &events {
        sink.event(e);
        std::thread::sleep(Duration::from_millis(3));
    }
    let (last, err) = rec.finish();
    stop.store(true, Ordering::SeqCst);
    let versions = reader.join().unwrap();
    assert!(err.is_none(), "{err:?}");
    assert!(versions > 1, "the status file was rewritten during the run");

    // the final write holds the final tree
    let on_disk = read_status(&store).unwrap().unwrap();
    assert_eq!(on_disk.outcome, Some(RunOutcome::Failed));
    assert_eq!(on_disk.nodes, last.nodes);
    assert!(status_path(&store).is_file());
    let stray: Vec<_> = std::fs::read_dir(&store)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(stray.is_empty(), "{stray:?}");

    // the run log: one JSON object per event, in order
    let log = latest_run(&store).expect("runs/latest names the run log");
    assert_eq!(on_disk.run.run_log.as_deref(), Some(log.as_path()));
    let text = std::fs::read_to_string(&log).unwrap();
    let tags: Vec<String> = text
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["event"].as_str().unwrap().to_owned()
        })
        .collect();
    let want: Vec<&str> = events.iter().map(|e| e.kind.tag()).collect();
    assert_eq!(tags, want);
    let first: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
    assert_eq!(first["task"]["name"], "s07/panel");
    assert_eq!(first["at_ms"], 0);
}

#[test]
fn runs_are_kept_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let options = RecorderOptions {
        keep_runs: 2,
        ..fast()
    };
    let mut logs = Vec::new();
    for _ in 0..3 {
        let rec = Recorder::with_options(dir.path(), View::Quiet, options).unwrap();
        logs.push(rec.run_log().to_owned());
        let _ = rec.finish();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!logs[0].exists(), "the oldest is pruned");
    assert!(logs[1].exists() && logs[2].exists());
    assert_eq!(latest_run(dir.path()), Some(logs[2].clone()));
}

#[test]
fn plain_view_prints_a_line_per_event() {
    let dir = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let view = {
        let seen = seen.clone();
        View::Plain(Box::new(move |e: &Event| {
            seen.lock().unwrap().push(e.kind.tag());
        }))
    };
    let rec = Recorder::with_options(dir.path(), view, fast()).unwrap();
    let events = Script::new().events(u64::MAX);
    for e in &events {
        rec.sink().event(e);
    }
    let _ = rec.finish();
    let want: Vec<&str> = events.iter().map(|e| e.kind.tag()).collect();
    assert_eq!(*seen.lock().unwrap(), want);
}

#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn live_view_redraws_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let out = Shared::default();
    let view = View::Live {
        out: Box::new(out.clone()),
        size: Box::new(|| (80, 30)),
        color: false,
    };
    let rec = Recorder::with_options(dir.path(), view, fast()).unwrap();
    for e in Script::new().events(u64::MAX) {
        rec.sink().event(&e);
        std::thread::sleep(Duration::from_millis(3));
    }
    let _ = rec.finish();
    let text = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
    assert!(text.starts_with("\x1b[J"), "{text:?}");
    assert!(
        text.contains("F\x1b[J"),
        "later frames move up over the last: {text:?}"
    );
    let last = text.rsplit("\x1b[J").next().unwrap();
    assert!(last.starts_with("failed in 1m00s"), "{last}");
    assert!(last.contains("  blender   failed"), "{last}");
}

#[test]
fn live_view_fits_the_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let out = Shared::default();
    let view = View::Live {
        out: Box::new(out.clone()),
        size: Box::new(|| (30, 6)),
        color: false,
    };
    let rec = Recorder::with_options(dir.path(), view, fast()).unwrap();
    for e in Script::new().events(u64::MAX) {
        rec.sink().event(&e);
    }
    let _ = rec.finish();
    let text = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
    let last = text.rsplit("\x1b[J").next().unwrap();
    let lines: Vec<&str> = last.lines().collect();
    assert_eq!(lines.len(), 5, "{last}");
    assert!(lines[4].starts_with("… "), "{last}");
    assert!(lines.iter().all(|l| l.chars().count() <= 30), "{last}");
}
