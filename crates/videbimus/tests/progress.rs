//! Logs and progress through the command line (ADR 0007): the run log and
//! status file a build leaves, `status` against the current config, and
//! the resolved config in each task log.

use std::path::PathBuf;
use std::process::ExitCode;

use videbimus::config::{Error, Node, Resolver};
use videbimus::store::Store;
use videbimus::{
    App, Console, Cost, KeyBuilder, KeyStart, RecipeContext, RecipeKind, Resources, Shape, Task,
    TaskContext, TaskError,
};
use videbimus_progress::{RunOutcome, Tree, latest_run, read_status, replace_atomic, status_path};

/// Frames `from`..`to`, one small file each.
struct Frames {
    key: KeyStart,
    shape: Shape,
}

impl Task for Frames {
    fn name(&self) -> &'static str {
        ""
    }
    fn shape(&self) -> Shape {
        self.shape
    }
    fn resources(&self) -> Resources {
        Resources::new(1, 0)
    }
    fn cost(&self) -> Cost {
        Cost::PerFrame(0.01)
    }
    fn key(&self) -> KeyBuilder {
        self.key.builder()
    }
    fn run(&self, cx: &TaskContext<'_>) -> Result<(), TaskError> {
        let frames = cx.frames().ok_or("a frame sequence")?;
        for f in frames.start.0..frames.end.0 {
            std::fs::write(cx.out_dir().join(format!("{f:04}.txt")), "x")?;
        }
        Ok(())
    }
}

impl RecipeKind for Frames {
    const KIND: &'static str = "frames";
    fn from_config(node: &Node, r: &Resolver, cx: &RecipeContext) -> Result<Box<dyn Task>, Error> {
        cx.check_props(node, r, &["from", "to", "grey"])?;
        Ok(Box::new(Frames {
            key: cx.key(node, r),
            shape: Shape::Frames(r.interval(node, "from", "to")?),
        }))
    }
}

/// A single output that hashes it (early cutoff): its dependants' keys
/// wait for it to be built.
struct Hashed(KeyStart);

impl Task for Hashed {
    fn name(&self) -> &'static str {
        ""
    }
    fn shape(&self) -> Shape {
        Shape::Single
    }
    fn resources(&self) -> Resources {
        Resources::new(1, 0)
    }
    fn cost(&self) -> Cost {
        Cost::Seconds(0.01)
    }
    fn key(&self) -> KeyBuilder {
        self.0.builder()
    }
    fn hash_output(&self) -> bool {
        true
    }
    fn run(&self, cx: &TaskContext<'_>) -> Result<(), TaskError> {
        std::fs::write(cx.out_dir().join("out.txt"), "same")?;
        Ok(())
    }
}

impl RecipeKind for Hashed {
    const KIND: &'static str = "hashed";
    fn from_config(node: &Node, r: &Resolver, cx: &RecipeContext) -> Result<Box<dyn Task>, Error> {
        Ok(Box::new(Hashed(cx.key(node, r))))
    }
}

/// A single output.
struct Single(KeyStart);

impl Task for Single {
    fn name(&self) -> &'static str {
        ""
    }
    fn shape(&self) -> Shape {
        Shape::Single
    }
    fn resources(&self) -> Resources {
        Resources::new(1, 0)
    }
    fn cost(&self) -> Cost {
        Cost::Seconds(0.01)
    }
    fn key(&self) -> KeyBuilder {
        self.0.builder()
    }
    fn run(&self, cx: &TaskContext<'_>) -> Result<(), TaskError> {
        std::fs::write(cx.out_dir().join("out.txt"), "x")?;
        Ok(())
    }
}

impl RecipeKind for Single {
    const KIND: &'static str = "single";
    fn from_config(node: &Node, r: &Resolver, cx: &RecipeContext) -> Result<Box<dyn Task>, Error> {
        Ok(Box::new(Single(cx.key(node, r))))
    }
}

fn app() -> App {
    App::new("videbimus")
        .recipe::<Frames>()
        .recipe::<Hashed>()
        .recipe::<Single>()
}

fn config(grey: &str) -> String {
    format!(
        r#"
video rate=30 width=4 height=2
let base=0.25
asset "s01/plate" recipe="frames" from=(f)0 to=(f)4 grey={grey}
asset "s01/grade" recipe="hashed" {{
    input "plate" "s01/plate"
}}
asset "s01/comp" recipe="single" {{
    input "graded" "s01/grade"
}}
asset "other" recipe="frames" from=(f)0 to=(f)2
"#
    )
}

struct Dir(tempfile::TempDir);

impl Dir {
    fn new(config: &str) -> Self {
        let d = Self(tempfile::tempdir().unwrap());
        d.write(config);
        d
    }
    fn write(&self, config: &str) {
        std::fs::write(self.config(), config).unwrap();
    }
    fn config(&self) -> PathBuf {
        self.0.path().join("video.kdl")
    }
    fn store(&self) -> PathBuf {
        self.0.path().join(".videbimus/store")
    }
    fn run(&self, args: &[&str]) -> (ExitCode, Console) {
        let console = Console::memory();
        let mut all = vec!["--config".to_owned(), self.config().display().to_string()];
        all.extend(args.iter().map(|&s| s.to_owned()));
        let code = app().run_from(all, &console);
        (code, console)
    }
    fn status(&self) -> String {
        let (code, c) = self.run(&["status"]);
        assert_eq!(code, ExitCode::SUCCESS, "{}", c.err());
        c.out()
    }
}

/// The status line of asset `name`, its padding squeezed.
fn line(status: &str, name: &str) -> String {
    let l = status
        .lines()
        .find(|l| l.trim_start().starts_with(&format!("{name} ")))
        .unwrap_or_else(|| panic!("no line for {name} in\n{status}"));
    l.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn status_says_whether_each_asset_is_current() {
    let d = Dir::new(&config("0.5"));
    let s = d.status();
    assert!(s.contains("no build is running in"), "{s}");
    assert!(s.contains("no run recorded yet"), "{s}");
    assert_eq!(line(&s, "s01/plate"), "s01/plate missing never built");
    assert_eq!(line(&s, "s01/grade"), "s01/grade missing never built");
    assert_eq!(
        line(&s, "s01/comp"),
        "s01/comp pending input `s01/grade`: its key is known once that is built"
    );

    // build the plate only
    let (code, c) = d.run(&["build", "s01/plate"]);
    assert_eq!(code, ExitCode::SUCCESS, "{}{}", c.out(), c.err());
    let s = d.status();
    assert!(s.contains("last run: complete"), "{s}");
    assert_eq!(line(&s, "s01/plate"), "s01/plate current 4/4 frames, built");
    assert_eq!(line(&s, "s01/grade"), "s01/grade missing never built");
    assert!(line(&s, "s01/comp").starts_with("s01/comp pending input"));

    // build everything
    let (code, c) = d.run(&["build"]);
    assert_eq!(code, ExitCode::SUCCESS, "{}{}", c.out(), c.err());
    let s = d.status();
    for a in ["s01/plate", "s01/grade", "s01/comp", "other"] {
        assert!(line(&s, a).starts_with(&format!("{a} current")), "{a}: {s}");
    }

    // edit the plate: it and what reads it go stale; what reads the
    // hashed grade can't know its key until the grade is rebuilt
    d.write(&config("0.6"));
    let s = d.status();
    assert!(
        line(&s, "s01/plate").starts_with("s01/plate stale its latest build ("),
        "{s}"
    );
    assert!(line(&s, "s01/grade").starts_with("s01/grade stale"), "{s}");
    assert!(
        line(&s, "s01/comp").starts_with("s01/comp pending input `s01/grade`"),
        "{s}"
    );
    assert!(line(&s, "other").starts_with("other current"), "{s}");
}

#[test]
fn a_build_records_its_run() {
    let d = Dir::new(&config("0.5"));
    let (code, c) = d.run(&["build"]);
    assert_eq!(code, ExitCode::SUCCESS, "{}{}", c.out(), c.err());
    // not a terminal: a line per event, as before
    assert!(c.out().contains("s01/plate: finished"), "{}", c.out());

    let store = d.store();
    let status = read_status(&store).unwrap().expect("a status file");
    assert_eq!(status.outcome, Some(RunOutcome::Complete));
    let plate = status.find("s01/plate").expect("plate in the tree");
    assert_eq!((plate.done_frames, plate.total_frames), (4, 4));
    let s01 = status.find("s01").expect("the s01 group");
    assert_eq!((s01.done_assets, s01.total_assets), (3, 3));

    let log = latest_run(&store).expect("a run log");
    assert_eq!(status.run.run_log.as_deref(), Some(log.as_path()));
    let text = std::fs::read_to_string(&log).unwrap();
    let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(first["event"], "run-started");
    let last: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(last["event"], "run-finished");
    assert_eq!(last["outcome"], "complete");

    // a second, cached run: the tree collapses
    let (code, _) = d.run(&["build"]);
    assert_eq!(code, ExitCode::SUCCESS);
    let status = read_status(&store).unwrap().unwrap();
    let s01 = status.find("s01").unwrap();
    assert!(s01.collapsed, "{s01:?}");
    assert_ne!(latest_run(&store).unwrap(), log, "each run has its own log");
}

#[test]
fn task_logs_hold_the_resolved_config() {
    let d = Dir::new(&config("(expr)\"$base * 2\""));
    let (code, c) = d.run(&["build", "s01/grade"]);
    assert_eq!(code, ExitCode::SUCCESS, "{}{}", c.out(), c.err());
    let store = Store::open_read_only(d.store()).unwrap();
    let plate =
        std::fs::read_to_string(store.log_path(store.latest("s01/plate").unwrap().unwrap()))
            .unwrap();
    assert!(
        plate.contains(
            "config:\n    asset \"s01/plate\" recipe=\"frames\" from=(f)0 to=(f)4 grey=0.5\n"
        ),
        "{plate}"
    );
    let grade =
        std::fs::read_to_string(store.log_path(store.latest("s01/grade").unwrap().unwrap()))
            .unwrap();
    assert!(
        grade.contains(
            "config:\n    asset \"s01/grade\" recipe=\"hashed\" {\n        input \"plate\" \"s01/plate\"\n    }\n"
        ),
        "{grade}"
    );
    assert!(grade.contains("why: first build"), "{grade}");
}

#[test]
fn status_while_building_shows_the_tree() {
    let d = Dir::new(&config("0.5"));
    let store = d.store();
    std::fs::create_dir_all(&store).unwrap();
    // this process "holds" the lock, and has written a status file
    let holder = videbimus::store::LockHolder {
        pid: std::process::id(),
        host: videbimus::store::machine_name(),
        started_ms: videbimus_progress::now_ms(),
    };
    std::fs::write(store.join("lock"), serde_json::to_vec(&holder).unwrap()).unwrap();
    let mut tree = Tree::new();
    tree.apply(&videbimus::scheduler::Event {
        at_ms: videbimus_progress::now_ms(),
        kind: videbimus::scheduler::EventKind::RunStarted {
            machine: "m1".to_owned(),
            profile: videbimus::scheduler::Profile::new("overnight", 8, 1 << 30, 10),
            targets: Vec::new(),
        },
    });
    let snap = tree.snapshot(videbimus_progress::now_ms());
    replace_atomic(&status_path(&store), &serde_json::to_vec(&snap).unwrap()).unwrap();

    let s = d.status();
    assert!(s.contains("building in"), "{s}");
    assert!(
        s.contains("building · profile overnight (nice 10) · m1 · everything"),
        "{s}"
    );
    assert!(!s.contains("assets against"), "{s}");
    std::fs::remove_file(store.join("lock")).unwrap();
    assert!(d.status().contains("no build is running"));
}
