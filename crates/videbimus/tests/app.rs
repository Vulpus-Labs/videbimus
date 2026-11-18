//! `App`, projects and the command line, with fake recipe kinds against
//! temporary stores.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use videbimus::config::{Error, Node, Resolver};
use videbimus::runtime::color::{read_png, write_png};
use videbimus::runtime::kinds::Stack;
use videbimus::scheduler::Target;
use videbimus::{
    App, Builder, CodeIdentity, Component, ComponentKind, Console, Context, Cost, Frame, Image,
    Interval, KeyBuilder, KeyStart, RecipeContext, RecipeKind, Resources, Shape, Task, TaskContext,
    TaskError,
};

/// Writes frames of one grey, `from`..`to`, `w`×`h` (4×2 by default).
struct FakeFrames {
    key: KeyStart,
    shape: Shape,
    grey: f32,
    size: (u32, u32),
}

impl Task for FakeFrames {
    fn name(&self) -> &'static str {
        "ignored: the project names it"
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
            let mut img = Image::new(self.size.0, self.size.1);
            img.fill([self.grey, self.grey, self.grey, 1.0]);
            write_png(&img, &cx.out_dir().join(format!("{f:04}.png")))?;
        }
        Ok(())
    }
}

impl RecipeKind for FakeFrames {
    const KIND: &'static str = "fake-frames";
    fn from_config(node: &Node, r: &Resolver, cx: &RecipeContext) -> Result<Box<dyn Task>, Error> {
        cx.check_props(node, r, &["from", "to", "grey", "w", "h"])?;
        #[allow(clippy::cast_possible_truncation)]
        let grey = r.opt::<f64>(node, "grey")?.unwrap_or(0.5) as f32;
        Ok(Box::new(FakeFrames {
            key: cx.key(node, r),
            shape: Shape::Frames(r.interval(node, "from", "to")?),
            grey,
            size: (
                r.opt(node, "w")?.unwrap_or(4),
                r.opt(node, "h")?.unwrap_or(2),
            ),
        }))
    }
}

/// A single output, for graph tests.
struct FakeSingle(KeyStart);

impl Task for FakeSingle {
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

impl RecipeKind for FakeSingle {
    const KIND: &'static str = "fake-single";
    fn from_config(node: &Node, r: &Resolver, cx: &RecipeContext) -> Result<Box<dyn Task>, Error> {
        Ok(Box::new(FakeSingle(cx.key(node, r))))
    }
}

/// A flat colour component.
struct Flat(Interval);

impl Component for Flat {
    fn interval(&self) -> Interval {
        self.0
    }
    fn reset(&mut self, _: &Context) {}
    fn render(&mut self, _: Frame, _: &Context, out: &mut Image) {
        out.fill([1.0, 0.0, 0.0, 1.0]);
    }
}

impl ComponentKind for Flat {
    const KIND: &'static str = "flat";
    fn from_config(node: &Node, r: &Resolver, b: &Builder) -> Result<Box<dyn Component>, Error> {
        b.check_props(node, r, &[])?;
        Ok(Box::new(Flat(b.interval(node, r)?)))
    }
}

fn app() -> App {
    App::new("videbimus")
        .recipe::<FakeFrames>()
        .recipe::<FakeSingle>()
}

/// A temporary project directory with `video.kdl`.
struct Dir {
    dir: tempfile::TempDir,
}

impl Dir {
    fn new(config: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("video.kdl"), config).unwrap();
        Self { dir }
    }
    fn config(&self) -> PathBuf {
        self.dir.path().join("video.kdl")
    }
    fn path(&self) -> &Path {
        self.dir.path()
    }
    fn run(&self, app: &App, args: &[&str]) -> (ExitCode, Console) {
        let console = Console::memory();
        let mut all = vec!["--config".to_owned(), self.config().display().to_string()];
        all.extend(args.iter().map(|&s| s.to_owned()));
        let code = app.run_from(all, &console);
        (code, console)
    }
    fn error(&self, app: &App) -> String {
        let e = app.load(&self.config(), None).unwrap_err().to_string();
        e.replace(&self.config().display().to_string(), "video.kdl")
    }
}

const SCENE: &str = r#"
video rate=30 width=4 height=2
asset "s01/frames" recipe="fake-frames" from=(f)0 to=(f)10 grey=0.5
asset "unused" recipe="fake-frames" from=(f)0 to=(f)4 grey=0.1
sequence "main" to=(f)10 {
    frame-player asset="s01/frames" start-index=2
}
"#;

// ---- registration

#[test]
fn registers_kinds() {
    let app = App::new("fenland")
        .component::<Flat>()
        .recipe::<FakeFrames>();
    assert_eq!(app.name(), "fenland");
    assert!(app.components().contains("flat"));
    assert!(app.components().contains("frame-player"), "built-ins too");
    assert!(app.recipes().contains("fake-frames"));
    assert!(!app.components().contains("fake-frames"));
}

#[test]
#[should_panic(expected = "component kind `stack` is registered twice")]
fn duplicate_component_kind() {
    let _ = App::new("x").component::<Stack>();
}

#[test]
#[should_panic(expected = "recipe kind `fake-frames` is registered twice")]
fn duplicate_recipe_kind() {
    let _ = App::new("x").recipe::<FakeFrames>().recipe::<FakeFrames>();
}

#[test]
fn builtin_kinds_have_code_identities() {
    let app = App::new("x");
    for kind in app.components().kinds() {
        let code = app.components().code(kind).unwrap();
        assert_eq!(code.code.len(), 64, "{kind}");
    }
    assert_ne!(
        app.components().code("stack"),
        app.components().code("sequence")
    );
}

// ---- requires and binary

#[test]
fn missing_kind_names_the_binary() {
    let d = Dir::new(
        r#"requires "fen" "fake-frames"
binary "fenland" run="cargo run -p fenland --"
video rate=30 width=4 height=2
fen "backdrop" to=(f)10
"#,
    );
    assert_eq!(
        d.error(&app()),
        "error: kind `fen` isn't registered; this config needs the `fenland` binary (`cargo run -p fenland --`)
 --> video.kdl:1:10
  |
1 | requires \"fen\" \"fake-frames\"
  |          ^^^^^
  = in: requires
  = note: this binary is `videbimus`
"
    );
    // and nothing ran: the store wasn't even created
    let (code, console) = d.run(&app(), &["build"]);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(
        console
            .err()
            .starts_with("error: kind `fen` isn't registered")
    );
    assert!(!d.path().join(".videbimus").exists());
}

#[test]
fn missing_kinds_without_a_binary_hint() {
    let d = Dir::new("requires \"fen\" \"song-render\"\nvideo rate=30\n");
    let e = d.error(&app());
    assert!(
        e.starts_with("error: kinds `fen` and `song-render` aren't registered in `videbimus`, and the config names no binary that has them\n"),
        "{e}"
    );
    assert!(
        e.contains("= help: add `binary \"<name>\" run=\"cargo run -p <name> --\"`"),
        "{e}"
    );
}

#[test]
fn requires_is_met_by_either_registry() {
    let d = Dir::new(
        "requires \"flat\" \"fake-frames\"\nvideo rate=30 width=4 height=2\nflat to=(f)3\n",
    );
    let app = app().component::<Flat>();
    assert!(app.load(&d.config(), None).is_ok());
}

// ---- root selection

#[test]
fn the_only_component_is_the_root() {
    let d = Dir::new(SCENE);
    let p = app().load(&d.config(), None).unwrap();
    assert_eq!(p.root().unwrap().describe(), "sequence \"main\"");
}

#[test]
fn root_names_one_of_several() {
    let two = "video rate=30 width=4 height=2 ROOT\nstack \"a\" to=(f)2\nstack \"b\" to=(f)2\n";
    let d = Dir::new(&two.replace("ROOT", "root=\"b\""));
    let p = app().load(&d.config(), None).unwrap();
    assert_eq!(p.root().unwrap().name(), Some("b"));

    let d = Dir::new(&two.replace("ROOT", ""));
    let e = d.error(&app());
    assert!(
        e.starts_with(
            "error: the config has 2 top-level components, and nothing says which is the video\n"
        ),
        "{e}"
    );
    assert!(
        e.contains("= note: they are `stack \"a\"`, `stack \"b\"`"),
        "{e}"
    );
    assert!(e.contains("= help: add root=\"<name>\" to `video`"), "{e}");

    let d = Dir::new(&two.replace("ROOT", "root=\"c\""));
    let e = d.error(&app());
    assert!(
        e.starts_with("error: there is no top-level component named `c`\n"),
        "{e}"
    );
}

#[test]
fn assets_only_has_no_root() {
    let d = Dir::new("asset \"a\" recipe=\"fake-single\"\n");
    let p = app().load(&d.config(), None).unwrap();
    assert!(p.root().is_none());
    assert_eq!(p.assets().len(), 1);
}

// ---- asset names and the graph

#[test]
fn names_come_from_groups_and_roles() {
    let d = Dir::new(
        r#"
video rate=30
asset "song" recipe="fake-single"
group "s07" {
    asset recipe="fake-single" { input "song" "song"; }
    asset "panel" recipe="fake-single"
    group "close" { asset recipe="fake-frames" from=(f)0 to=(f)2 { input "panel" "s07/panel"; } }
}
"#,
    );
    let p = app().load(&d.config(), None).unwrap();
    let names: Vec<&str> = p.assets().iter().map(|a| a.name.as_str()).collect();
    // inputs come first
    assert_eq!(
        names,
        [
            "song",
            "s07/fake-single",
            "s07/panel",
            "s07/close/fake-frames"
        ]
    );
    let shot = p.asset("s07/close/fake-frames").unwrap();
    assert_eq!(shot.group.as_deref(), Some("s07/close"));
    assert_eq!(shot.inputs, [("panel".to_owned(), "s07/panel".to_owned())]);
    // the task carries the assigned name, not its own
    assert_eq!(p.graph().task(shot.node).name(), "s07/close/fake-frames");
    // a group path builds everything under it
    let t = p.named_targets(&["s07".to_owned()]).unwrap();
    assert_eq!(t.len(), 3);
    assert_eq!(
        p.named_targets(&["s07/panl".to_owned()]).unwrap_err(),
        "there is no asset or group `s07/panl`; did you mean `s07/panel`?"
    );
}

#[test]
fn asset_declaration_errors() {
    let e = Dir::new(
        "asset \"a\" recipe=\"fake-single\"\ngroup \"\" { }\nasset \"a\" recipe=\"fake-single\"\n",
    )
    .error(&app());
    assert!(e.starts_with("error: `` isn't an asset name"), "{e}");

    let e = Dir::new("asset \"a\" recipe=\"fake-single\"\nasset \"a\" recipe=\"fake-single\"\n")
        .error(&app());
    assert!(e.starts_with("error: a second asset is named `a`\n"), "{e}");
    assert!(e.contains("= note: the first is at video.kdl:1:1"), "{e}");

    let e = Dir::new("asset \"a\" recipe=\"fake-single\" { input \"x\" \"panl\"; }\nasset \"panel\" recipe=\"fake-single\"\n")
        .error(&app());
    assert!(
        e.starts_with("error: there is no asset `panl` in this config\n"),
        "{e}"
    );
    assert!(e.contains("did you mean `panel`?"), "{e}");

    let e = Dir::new(
        "asset \"a\" recipe=\"fake-single\" { input \"x\" \"b\"; }\nasset \"b\" recipe=\"fake-single\" { input \"y\" \"a\"; }\n",
    )
    .error(&app());
    assert!(
        e.starts_with("error: the assets' inputs form a cycle\n"),
        "{e}"
    );
    assert!(e.contains("= note: cycle: a → b → a"), "{e}");

    let e = Dir::new("asset \"a\" recipe=\"fake-singel\"\n").error(&app());
    assert!(
        e.starts_with("error: there is no recipe kind `fake-singel`\n"),
        "{e}"
    );
    assert!(e.contains("did you mean `fake-single`?"), "{e}");

    let e = Dir::new("asset \"a\"\n").error(&app());
    assert!(
        e.starts_with("error: an asset needs a recipe kind\n"),
        "{e}"
    );
}

// ---- what the scene reads

#[test]
fn the_scene_builds_what_it_reads() {
    let d = Dir::new(SCENE);
    let p = app().load(&d.config(), None).unwrap();
    let frames = p.asset("s01/frames").unwrap().node;
    assert_eq!(p.scene_targets(None), [Target::all(frames)]);
    // video frames 3..5 show asset frames 5..7 (start-index=2)
    assert_eq!(
        p.scene_targets(Some(Interval::new(Frame(3), Frame(5)))),
        [Target::frames(frames, Interval::new(Frame(5), Frame(7)))]
    );
    // frames the asset doesn't cover need nothing
    assert!(
        p.scene_targets(Some(Interval::new(Frame(20), Frame(22))))
            .is_empty()
    );
}

#[test]
fn reading_an_undeclared_asset_is_an_error() {
    let d = Dir::new(
        "video rate=30 width=4 height=2\nasset \"s01/frames\" recipe=\"fake-frames\" from=(f)0 to=(f)2\nframe-player asset=\"s01/frame\" to=(f)2\n",
    );
    let e = d.error(&app());
    assert!(
        e.starts_with("error: there is no asset `s01/frame` in this config\n"),
        "{e}"
    );
    assert!(e.contains("did you mean `s01/frames`?"), "{e}");
}

#[test]
fn reads_by_name_in_the_project_store_are_wired() {
    let d = Dir::new(
        "video rate=30 width=4 height=2\nasset \"a\" recipe=\"fake-frames\" from=(f)0 to=(f)2\nstack to=(f)2 {\n frame-player store=\".videbimus/store\" name=\"a\"\n frame-player store=\"elsewhere\" name=\"a\"\n frame-player dir=\"frames\"\n}\n",
    );
    let p = app().load(&d.config(), None).unwrap();
    assert_eq!(p.reads().len(), 2, "dir reads aren't recorded");
    assert_eq!(p.wired().len(), 1, "only the project's store is built");
}

// ---- code identity

fn identity_app(ka: &str, kb: &str, version: &'static str) -> App {
    let mut app = App::new("x");
    let code = |c: &str| CodeIdentity {
        code: Cow::Owned(c.to_owned()),
        version: Cow::Borrowed(version),
    };
    app.recipes_mut()
        .register_with_code("ka", code(ka), FakeSingle::from_config)
        .register_with_code("kb", code(kb), FakeSingle::from_config);
    app
}

fn keys(app: &App, config: &Path) -> Vec<(String, videbimus::core::AssetKey)> {
    let p = app.load(config, None).unwrap();
    p.assets()
        .iter()
        .map(|a| {
            let key = p.graph().task(a.node).key().build().unwrap().key();
            (a.name.clone(), key)
        })
        .collect()
}

#[test]
fn editing_one_kind_changes_only_its_keys() {
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("ka.rs"), "fn a() {}").unwrap();
    std::fs::write(src.path().join("kb.rs"), "fn b() {}").unwrap();
    let kinds = videbimus_build::Kinds::new()
        .kind("ka", &["ka.rs"])
        .kind("kb", &["kb.rs"]);
    let d = Dir::new(
        "asset \"one\" recipe=\"ka\"\nasset \"two\" recipe=\"ka\" n=2\nasset \"three\" recipe=\"kb\"\n",
    );
    let ids = kinds.identities(src.path()).unwrap();
    let before = keys(&identity_app(&ids[0].1, &ids[1].1, ""), &d.config());

    std::fs::write(src.path().join("ka.rs"), "fn a() { /* edited */ }").unwrap();
    let ids = kinds.identities(src.path()).unwrap();
    let after = keys(&identity_app(&ids[0].1, &ids[1].1, ""), &d.config());
    assert_ne!(after[0], before[0], "ka's assets rebuild");
    assert_ne!(after[1], before[1]);
    assert_eq!(after[2], before[2], "kb's don't");

    // VERSION forces a rebuild too
    let bumped = keys(&identity_app(&ids[0].1, &ids[1].1, "2"), &d.config());
    assert!(bumped.iter().zip(&after).all(|(b, a)| b != a));
}

#[test]
fn canonical_config_ignores_order_and_units() {
    let one =
        Dir::new("video rate=30\nasset \"a\" recipe=\"fake-frames\" from=(f)0 to=(s)1 grey=0.5\n");
    let two = Dir::new(
        "video rate=30\nasset \"b\" recipe=\"fake-frames\" grey=0.5 to=(f)30 from=(f)0 { input \"x\" \"c\"; }\nasset \"c\" recipe=\"fake-single\"\n",
    );
    let three =
        Dir::new("video rate=30\nasset \"a\" recipe=\"fake-frames\" from=(f)0 to=(s)1 grey=0.6\n");
    let k = |d: &Dir| {
        let p = app().load(&d.config(), None).unwrap();
        let a = p
            .assets()
            .iter()
            .find(|a| a.recipe == "fake-frames")
            .unwrap();
        videbimus::canonical_config(
            p.config()
                .nodes()
                .iter()
                .find(|n| n.kind() == "asset" && n.name() == Some(a.name.as_str()))
                .unwrap(),
            p.config().resolver(),
        )
    };
    assert_eq!(k(&one), k(&two), "name, inputs, order and units aside");
    assert_ne!(k(&one), k(&three));
}

// ---- the command line

#[test]
fn preview_writes_frames() {
    let d = Dir::new(SCENE);
    let out = d.path().join("out");
    let (code, console) = d.run(
        &app(),
        &["preview", "3..5", "--out", &out.display().to_string()],
    );
    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "{}{}",
        console.out(),
        console.err()
    );
    assert!(
        console.out().contains("wrote 2 frames to"),
        "{}",
        console.out()
    );
    for f in [3, 4] {
        let img = read_png(&out.join(format!("{f:06}.png"))).unwrap();
        assert_eq!((img.width(), img.height()), (4, 2));
        for p in img.pixels() {
            assert!(
                (p[0] - 0.5).abs() < 0.01 && (p[3] - 1.0).abs() < 1e-6,
                "{p:?}"
            );
        }
    }
    // the unused asset wasn't built
    let (_, status) = d.run(&app(), &["status"]);
    let s = status.out();
    assert!(
        s.contains("scene sequence \"main\" reads: s01/frames"),
        "{s}"
    );
    assert!(s.contains("unused      fake-frames  not built"), "{s}");
}

#[test]
fn build_runs_then_caches() {
    let d = Dir::new(SCENE);
    let (code, first) = d.run(&app(), &["build"]);
    assert_eq!(code, ExitCode::SUCCESS, "{}{}", first.out(), first.err());
    let out = first.out();
    assert!(out.contains("s01/frames: building (first build)"), "{out}");
    assert!(out.contains("s01/frames: finished"), "{out}");
    assert!(!out.contains("unused"), "the scene doesn't read it: {out}");
    assert!(out.contains("run complete: 1 built, 0 cached"), "{out}");

    let (code, second) = d.run(&app(), &["build"]);
    assert_eq!(code, ExitCode::SUCCESS);
    let out = second.out();
    assert!(out.contains("s01/frames: cached"), "{out}");
    assert!(out.contains("run complete: 0 built, 1 cached"), "{out}");

    // by name
    let (code, named) = d.run(&app(), &["build", "unused"]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(named.out().contains("unused: finished"), "{}", named.out());
}

#[test]
fn usage_errors_and_a_review_without_output() {
    let d = Dir::new(SCENE);
    let (code, c) = d.run(&app(), &["frobnicate"]);
    assert_eq!(code, ExitCode::from(2));
    assert!(
        c.err()
            .starts_with("error: there is no command `frobnicate`")
    );

    let (code, c) = d.run(&app(), &["build", "--profile", "turbo"]);
    assert_eq!(code, ExitCode::from(2));
    assert!(c.err().contains("there is no profile `turbo`"));

    let (code, c) = d.run(&app(), &["review"]);
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(
        c.err(),
        "error: the config declares no output to review; add output \"film.mp4\" at the top\n"
    );

    let (code, c) = d.run(&app(), &["stop"]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(c.out().starts_with("nothing is building in"), "{}", c.out());

    let (code, c) = d.run(&app(), &["--help"]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(c.out().starts_with("usage: videbimus [--config FILE]"));
}

#[test]
fn the_stock_binary() {
    let d = Dir::new("requires \"fen\"\nbinary \"fenland\"\nvideo rate=30\n");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_videbimus"))
        .args(["--config", &d.config().display().to_string(), "build"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with(
            "error: kind `fen` isn't registered; this config needs the `fenland` binary\n"
        ),
        "{err}"
    );
    assert!(err.contains("= note: this binary is `videbimus`"), "{err}");
}

// ---- output (ticket 0007)

#[test]
fn build_encodes_the_output_and_review_assembles_it() {
    let ffmpeg = std::process::Command::new("ffmpeg")
        .arg("-version")
        .output();
    if !ffmpeg.is_ok_and(|o| o.status.success()) {
        eprintln!("skipping: ffmpeg isn't on the path");
        return;
    }
    let d = Dir::new(
        r#"
video rate=30 width=4 height=2
asset "s01/frames" recipe="fake-frames" from=(f)0 to=(f)10 grey=0.5
output "film.mp4" segment=(f)4 preset="ultrafast"
sequence "main" to=(f)10 {
    frame-player asset="s01/frames" start-index=2
}
"#,
    );
    let project = app().load(&d.config(), None).unwrap();
    let o = project.output().unwrap();
    assert_eq!(o.segments().len(), 3);
    assert_eq!(
        project.root().unwrap().name(),
        Some("main"),
        "output isn't a root"
    );

    // review before anything is built: all placeholders, nothing built
    let (code, c) = d.run(&app(), &["review"]);
    assert_eq!(code, ExitCode::SUCCESS, "{}", c.err());
    assert!(
        c.out()
            .contains("film.review.mp4: 0 of 3 segments built, 10 frames shown as placeholders"),
        "{}",
        c.out()
    );
    assert!(!d.path().join(".videbimus/store").exists());

    let (code, c) = d.run(&app(), &["build"]);
    assert_eq!(code, ExitCode::SUCCESS, "{}{}", c.out(), c.err());
    assert!(c.out().contains("output/join: finished"), "{}", c.out());
    assert!(
        c.out()
            .ends_with(&format!("wrote {}\n", d.path().join("film.mp4").display()))
    );
    assert!(d.path().join("film.mp4").is_file());

    let out = d.path().join("r.mp4");
    let (code, c) = d.run(&app(), &["review", "--out", &out.display().to_string()]);
    assert_eq!(code, ExitCode::SUCCESS, "{}", c.err());
    assert!(c.out().contains("3 of 3 segments built"), "{}", c.out());
    assert!(out.is_file());
}
