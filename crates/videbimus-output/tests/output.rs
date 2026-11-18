//! The output end to end: segments through the scheduler and ffmpeg,
//! keys, stop and resume, the join and audio, review, and state persisted
//! across segments. Tiny frames (64×36) and short videos keep it fast.
//! Every test that runs ffmpeg is skipped, with a message, without it.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::missing_panics_doc
)]

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use videbimus_config::{Config, Error, Node as ConfigNode, Resolver};
use videbimus_core::{Component, Context, Frame, Horizon, Image, Interval};
use videbimus_runtime::kinds::{Caption, FramePlayer, Sequence};
use videbimus_runtime::{Builder, CodeIdentity, ComponentKind, ComponentRegistry, Scene};
use videbimus_scheduler::{
    Cost, Event, EventKind, Graph, Node, NodeId, Outcome, Profile, Resources, RunReport, Scheduler,
    Target, Task, TaskContext, TaskError, TaskState,
};
use videbimus_store::{KeyBuilder, Shape, Store};

use videbimus_output::encode::rgb24;
use videbimus_output::{Output, PlanContext, SceneSource};

const W: u32 = 64;
const H: u32 = 36;

// ---------------------------------------------------------------- tools

fn have_ffmpeg() -> bool {
    let ok = |tool: &str| {
        Command::new(tool)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if ok("ffmpeg") && ok("ffprobe") {
        true
    } else {
        eprintln!("skipping: ffmpeg and ffprobe aren't both on the path");
        false
    }
}

/// `(video frames counted, video duration, audio duration)` of `file`.
fn probe(file: &Path) -> (u64, f64, Option<f64>) {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames"])
        .args(["-show_entries", "stream=codec_type,nb_read_frames,duration"])
        .args(["-of", "csv=p=0"])
        .arg(file)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (mut frames, mut vdur, mut adur) = (0, 0.0, None);
    for line in text.lines() {
        let f: Vec<&str> = line.split(',').collect();
        match f.first().copied() {
            Some("video") => {
                vdur = f[1].parse().unwrap();
                frames = f[2].parse().unwrap();
            }
            Some("audio") => adur = Some(f[1].parse().unwrap()),
            _ => {}
        }
    }
    (frames, vdur, adur)
}

/// Every frame of `file`, decoded to `rgb24`.
fn decode(file: &Path) -> Vec<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(file)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .chunks((W * H * 3) as usize)
        .map(<[u8]>::to_vec)
        .collect()
}

fn mean_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(x.abs_diff(*y)))
        .sum::<f64>()
        / a.len() as f64
}

/// A 16-bit mono WAV of a sine, `seconds` long at `rate`.
fn wav(seconds: f64, rate: u32) -> Vec<u8> {
    let n = (seconds * f64::from(rate)) as u32;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + n * 2).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(n * 2).to_le_bytes());
    for i in 0..n {
        let t = f64::from(i) / f64::from(rate);
        let s = ((t * 440.0 * std::f64::consts::TAU).sin() * 8000.0) as i16;
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

// ---------------------------------------------------------------- assets

/// A frame-sequence asset of flat colour, as PNG files named by frame.
struct Solid {
    name: String,
    frames: i64,
    grey: f32,
    version: u32,
}

impl Task for Solid {
    fn name(&self) -> &str {
        &self.name
    }
    fn shape(&self) -> Shape {
        Shape::Frames(Interval::new(Frame(0), Frame(self.frames)))
    }
    fn resources(&self) -> Resources {
        Resources::new(1, 1 << 20)
    }
    fn cost(&self) -> Cost {
        Cost::PerFrame(0.001)
    }
    fn key(&self) -> KeyBuilder {
        KeyBuilder::new("solid").config(format!("{} v{}", self.grey, self.version))
    }
    fn run(&self, ctx: &TaskContext<'_>) -> Result<(), TaskError> {
        let mut img = Image::new(W, H);
        img.fill([self.grey, self.grey * 0.5, 0.1, 1.0]);
        for f in ctx.frames().unwrap().start.0..ctx.frames().unwrap().end.0 {
            videbimus_runtime::color::write_png(&img, &ctx.out_dir().join(format!("{f:04}.png")))?;
        }
        Ok(())
    }
}

/// A single-output asset holding one WAV.
struct Wav {
    seconds: f64,
}

impl Task for Wav {
    fn name(&self) -> &'static str {
        "mix"
    }
    fn shape(&self) -> Shape {
        Shape::Single
    }
    fn resources(&self) -> Resources {
        Resources::new(1, 1 << 20)
    }
    fn cost(&self) -> Cost {
        Cost::Seconds(0.01)
    }
    fn key(&self) -> KeyBuilder {
        KeyBuilder::new("wav").config(format!("{}", self.seconds))
    }
    fn run(&self, ctx: &TaskContext<'_>) -> Result<(), TaskError> {
        std::fs::write(ctx.out_dir().join("mix.wav"), wav(self.seconds, 44_100))?;
        Ok(())
    }
}

// ------------------------------------------------------- a stateful kind

thread_local! {
    static STEPS: Cell<u64> = const { Cell::new(0) };
}

/// A stateful test kind: its state is a running hash of every frame it
/// has stepped, drawn as a colour. It persists its state.
struct Walker {
    interval: Interval,
    state: u64,
}

impl Component for Walker {
    fn interval(&self) -> Interval {
        self.interval
    }
    fn horizon(&self) -> Horizon {
        Horizon::Unbounded
    }
    fn reset(&mut self, _: &Context) {
        self.state = 0x9e37_79b9;
    }
    fn step(&mut self, n: Frame, _: &Context) {
        STEPS.with(|s| s.set(s.get() + 1));
        self.state = (self.state ^ n.0 as u64)
            .wrapping_mul(0x0100_0000_01b3)
            .rotate_left(7);
    }
    fn render(&mut self, _: Frame, _: &Context, out: &mut Image) {
        let c = |shift: u32| ((self.state >> shift) & 0xff) as f32 / 255.0;
        out.fill([c(0), c(8), c(16), 1.0]);
    }
    fn save_state(&self) -> Option<Vec<u8>> {
        Some(self.state.to_le_bytes().to_vec())
    }
    fn load_state(&mut self, bytes: &[u8]) -> bool {
        bytes
            .try_into()
            .map(|b| self.state = u64::from_le_bytes(b))
            .is_ok()
    }
}

impl ComponentKind for Walker {
    const KIND: &'static str = "walker";
    const CODE: &'static str = "walker v1";
    fn from_config(
        node: &ConfigNode,
        r: &Resolver,
        b: &Builder,
    ) -> Result<Box<dyn Component>, Error> {
        b.check_props(node, r, &[])?;
        Ok(Box::new(Walker {
            interval: b.interval(node, r)?,
            state: 0,
        }))
    }
}

fn registry() -> ComponentRegistry {
    ComponentRegistry::with_builtins().with::<Walker>()
}

// ---------------------------------------------------------------- fixture

struct Fx {
    dir: tempfile::TempDir,
}

/// What a fixture's graph holds.
struct Planned {
    graph: Graph,
    output: Output,
}

enum Asset {
    Solid {
        name: &'static str,
        frames: i64,
        grey: f32,
        version: u32,
    },
    Wav(f64),
}

impl Fx {
    fn new(kdl: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("video.kdl"), kdl).unwrap();
        Self { dir }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    fn store(&self) -> PathBuf {
        self.path("store")
    }

    /// The config's assets and output, planned into a graph.
    fn plan(&self, registry: &ComponentRegistry, assets: &[Asset]) -> Planned {
        let config = Config::load(self.path("video.kdl")).unwrap();
        let mut graph = Graph::new();
        let mut ids: HashMap<String, NodeId> = HashMap::new();
        for a in assets {
            let (name, id) = match a {
                Asset::Solid {
                    name,
                    frames,
                    grey,
                    version,
                } => (
                    (*name).to_owned(),
                    graph
                        .add(Node::new(Solid {
                            name: (*name).to_owned(),
                            frames: *frames,
                            grey: *grey,
                            version: *version,
                        }))
                        .unwrap(),
                ),
                Asset::Wav(s) => (
                    "mix".to_owned(),
                    graph.add(Node::new(Wav { seconds: *s })).unwrap(),
                ),
            };
            ids.insert(name, id);
        }
        let root = config
            .nodes()
            .iter()
            .position(|n| n.name() == Some("main"))
            .unwrap();
        let source = SceneSource::new(config.clone(), root, registry.clone(), self.store());
        let (_, reads) = source.build_recording().unwrap();
        let wired: Vec<_> = reads
            .take()
            .into_iter()
            .filter_map(|r| ids.get(&r.name).map(|id| (*id, r)))
            .collect();
        let lookup = |n: &str| ids.get(n).copied();
        let cx = PlanContext {
            config: &config,
            root: Some(root),
            registry,
            store: &self.store(),
            scene_identity: std::fs::read(self.path("video.kdl")).unwrap(),
            wired: &wired,
            asset: &lookup,
            asset_names: ids.keys().map(String::as_str).collect(),
        };
        let output = Output::plan(&cx, &mut graph).unwrap().unwrap();
        Planned { graph, output }
    }

    fn scheduler(&self) -> Scheduler {
        let (store, _) = Store::open(self.store()).unwrap();
        Scheduler::new(store, Profile::new("test", 4, 8 << 30, 0))
    }

    fn build(&self, p: &Planned, targets: &[Target]) -> RunReport {
        self.scheduler().run(&p.graph, targets).unwrap()
    }

    /// The store directory of `name`'s latest build.
    fn built_dir(&self, name: &str) -> PathBuf {
        let store = Store::open_read_only(self.store()).unwrap();
        let key = store.latest(name).unwrap().unwrap();
        store.chunk_dir(key, videbimus_store::ChunkId::Whole)
    }

    fn log(&self, report: &RunReport, name: &str) -> String {
        let store = Store::open_read_only(self.store()).unwrap();
        std::fs::read_to_string(store.log_path(report.task(name).unwrap().key.unwrap())).unwrap()
    }
}

fn state(r: &RunReport, name: &str) -> TaskState {
    r.task(name).unwrap().state.clone()
}

fn seg(i: usize) -> String {
    format!("output/segment-{i:05}")
}

const TWO_SHOTS: &str = r#"
video rate=30 width=64 height=36
output "film.mp4" segment=(f)20 preset="ultrafast"
sequence "main" to=(f)40 {
    frame-player asset="a" length=(f)20
    frame-player asset="b" length=(f)20
}
"#;

fn two_shots(b_version: u32) -> Vec<Asset> {
    vec![
        Asset::Solid {
            name: "a",
            frames: 20,
            grey: 0.2,
            version: 1,
        },
        Asset::Solid {
            name: "b",
            frames: 20,
            grey: 0.7,
            version: b_version,
        },
    ]
}

// ---------------------------------------------------------------- tests

#[test]
fn segments_read_only_the_assets_they_show() {
    let fx = Fx::new(TWO_SHOTS);
    let p = fx.plan(&registry(), &two_shots(1));
    let segs = p.output.segments();
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].frames, Interval::new(Frame(0), Frame(20)));
    assert_eq!(segs[0].reads, ["a"]);
    assert_eq!(segs[1].reads, ["b"]);
    assert_eq!(p.output.frames(), Interval::new(Frame(0), Frame(40)));
}

#[test]
fn segment_keys_follow_code_identities_and_the_assets_read() {
    if !have_ffmpeg() {
        return;
    }
    let fx = Fx::new(TWO_SHOTS);
    let p = fx.plan(&registry(), &two_shots(1));
    let first = fx.build(&p, &[p.output.target()]);
    assert_eq!(first.outcome, Outcome::Complete);
    for n in [seg(0), seg(1), "output/join".to_owned()] {
        assert_eq!(state(&first, &n), TaskState::Built, "{n}");
    }

    // rebuilding b re-renders only the segment that shows it
    let p = fx.plan(&registry(), &two_shots(2));
    let r = fx.build(&p, &[p.output.target()]);
    assert_eq!(state(&r, &seg(0)), TaskState::Cached);
    assert_eq!(state(&r, &seg(1)), TaskState::Built);
    assert_eq!(state(&r, "output/join"), TaskState::Built);

    // a kind the scene doesn't build: nothing changes
    let mut other = ComponentRegistry::empty()
        .with::<Sequence>()
        .with::<FramePlayer>()
        .with::<Walker>();
    other.register_with_code(
        "caption",
        CodeIdentity::new("edited caption", ""),
        <Caption as ComponentKind>::from_config,
    );
    let p = fx.plan(&other, &two_shots(2));
    let r = fx.build(&p, &[p.output.target()]);
    assert_eq!(state(&r, &seg(0)), TaskState::Cached);
    assert_eq!(state(&r, &seg(1)), TaskState::Cached);

    // editing the frame player's source re-renders every segment
    let mut edited = ComponentRegistry::empty()
        .with::<Sequence>()
        .with::<Walker>();
    edited.register_with_code(
        "frame-player",
        CodeIdentity::new("edited frame player", ""),
        <FramePlayer as ComponentKind>::from_config,
    );
    let p = fx.plan(&edited, &two_shots(2));
    let r = fx.build(&p, &[p.output.target()]);
    assert_eq!(state(&r, &seg(0)), TaskState::Built);
    assert_eq!(state(&r, &seg(1)), TaskState::Built);
    let store = Store::open_read_only(fx.store()).unwrap();
    let m = store
        .manifest(r.task(&seg(0)).unwrap().key.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(m.why.to_string(), "source kind/frame-player changed");
}

#[test]
fn a_stopped_render_resumes_with_only_the_missing_segments() {
    if !have_ffmpeg() {
        return;
    }
    let fx = Fx::new(
        r#"
video rate=30 width=64 height=36
output "film.mp4" segment=(f)10 preset="ultrafast"
sequence "main" to=(f)40 { walker }
"#,
    );
    let p = fx.plan(&registry(), &[]);
    // two cores: segments (two each) run one at a time; stop after the
    // first commits
    let (store, _) = Store::open(fx.store()).unwrap();
    let s = Scheduler::new(store, Profile::new("test", 2, 8 << 30, 0));
    let control = s.control();
    let s = s.with_sink(Arc::new(move |e: &Event| {
        if matches!(e.kind, EventKind::ChunkCommitted { .. }) {
            control.stop();
        }
    }));
    let first = s.run(&p.graph, &[p.output.target()]).unwrap();
    drop(s);
    assert_eq!(first.outcome, Outcome::Stopped);
    let done: Vec<String> = (0..4)
        .map(seg)
        .filter(|n| state(&first, n) == TaskState::Built)
        .collect();
    assert!(!done.is_empty() && done.len() < 4, "{done:?}");

    let second = fx.build(&p, &[p.output.target()]);
    assert_eq!(second.outcome, Outcome::Complete);
    for n in (0..4).map(seg) {
        let want = if done.contains(&n) {
            TaskState::Cached
        } else {
            TaskState::Built
        };
        assert_eq!(state(&second, &n), want, "{n}");
    }
    let (frames, _, _) = probe(&fx.built_dir("output/join").join("film.mp4"));
    assert_eq!(frames, 40);
}

#[test]
fn the_join_has_every_frame_and_the_audio_asset_padded_to_length() {
    if !have_ffmpeg() {
        return;
    }
    let fx = Fx::new(
        r#"
video rate=30 width=64 height=36
output "out/film.mp4" segment=(f)20 preset="ultrafast" {
    audio asset="mix"
}
sequence "main" to=(f)50 { walker }
"#,
    );
    // 1 s of audio under 50 frames (1.667 s): padded with silence
    let p = fx.plan(&registry(), &[Asset::Wav(1.0)]);
    assert_eq!(p.output.segments().len(), 3, "20, 20 and 10 frames");
    let r = fx.build(&p, &[p.output.target()]);
    assert_eq!(r.outcome, Outcome::Complete);
    let key = r.task("output/join").unwrap().key.unwrap();
    let store = Store::open_read_only(fx.store()).unwrap();
    let file = p.output.export(&store, key).unwrap();
    assert_eq!(file, fx.path("out/film.mp4"));
    let (frames, video, audio) = probe(&file);
    assert_eq!(frames, 50);
    assert!((video - 50.0 / 30.0).abs() < 1e-3, "{video}");
    let audio = audio.expect("an audio stream");
    assert!(
        (audio - video).abs() < 1.0 / 30.0,
        "audio {audio} s, video {video} s"
    );
}

#[test]
fn an_audio_file_is_cut_to_the_video() {
    if !have_ffmpeg() {
        return;
    }
    let fx = Fx::new(
        r#"
video rate="30000/1001" width=64 height=36
output "film.mov" segment=(f)16 preset="ultrafast" { audio "tone.wav" sample-rate=44100; }
sequence "main" to=(f)45 { walker }
"#,
    );
    std::fs::write(fx.path("tone.wav"), wav(3.0, 48_000)).unwrap();
    let p = fx.plan(&registry(), &[]);
    let r = fx.build(&p, &[p.output.target()]);
    assert_eq!(r.outcome, Outcome::Complete);
    let (frames, video, audio) = probe(&fx.built_dir("output/join").join("film.mov"));
    assert_eq!(frames, 45);
    let want = 45.0 * 1001.0 / 30000.0;
    assert!((video - want).abs() < 1.0 / 30.0, "{video} vs {want}");
    let audio = audio.expect("an audio stream");
    assert!(
        (audio - want).abs() < 1.0 / 30.0,
        "audio {audio} s, want {want}"
    );

    // the audio file is an input of the join: editing it re-joins only
    std::fs::write(fx.path("tone.wav"), wav(2.5, 48_000)).unwrap();
    let p = fx.plan(&registry(), &[]);
    let r = fx.build(&p, &[p.output.target()]);
    assert_eq!(state(&r, &seg(0)), TaskState::Cached);
    assert_eq!(state(&r, "output/join"), TaskState::Built);
}

#[test]
fn review_shows_placeholders_for_missing_segments() {
    if !have_ffmpeg() {
        return;
    }
    let fx = Fx::new(TWO_SHOTS);
    let p = fx.plan(&registry(), &two_shots(1));
    // only the first segment
    let r = fx.build(&p, &[Target::all(p.output.segments()[0].node)]);
    assert_eq!(r.outcome, Outcome::Complete);
    assert_eq!(state(&r, &seg(1)), TaskState::NotRequested);

    let dest = fx.path("review/film.review.mp4");
    let review = p.output.review(&fx.store(), &dest).unwrap();
    assert_eq!(review.segments, 2);
    assert_eq!(review.missing, [Interval::new(Frame(20), Frame(40))]);
    let frames = decode(&dest);
    assert_eq!(frames.len(), 40);

    // segment 0 is the shot, flat; segment 1 the placeholder
    let mut shot = Image::new(W, H);
    shot.fill([0.2, 0.1, 0.1, 1.0]);
    let mut ph = Image::new(W, H);
    videbimus_runtime::raster::placeholder(&mut ph, "NOT RENDERED 30");
    assert!(mean_diff(&frames[5], &rgb24(&shot)) < 6.0);
    let d_ph = mean_diff(&frames[30], &rgb24(&ph));
    let d_shot = mean_diff(&frames[30], &rgb24(&shot));
    assert!(
        d_ph < 12.0 && d_shot > 2.0 * d_ph,
        "placeholder {d_ph}, shot {d_shot}"
    );
    // review built nothing
    let store = Store::open_read_only(fx.store()).unwrap();
    assert_eq!(store.latest(&seg(1)).unwrap(), None);

    // with no store at all, everything is a placeholder
    let empty = Fx::new(TWO_SHOTS);
    let p = empty.plan(&registry(), &two_shots(1));
    let review = p
        .output
        .review(&empty.store(), &empty.path("r.mp4"))
        .unwrap();
    assert_eq!(review.missing.len(), 2);
    assert!(!empty.store().exists());
}

/// Frames of `scene` in `frames`, with the steps taken.
fn render(scene: &mut Scene, frames: std::ops::Range<i64>) -> (Vec<Image>, u64) {
    let before = STEPS.with(Cell::get);
    let images = frames.map(|f| scene.render(Frame(f))).collect();
    (images, STEPS.with(Cell::get) - before)
}

#[test]
fn persisted_state_renders_what_sequential_rendering_does() {
    let fx = Fx::new(
        r#"
video rate=30 width=8 height=4
sequence "main" to=(f)60 {
    stack length=(f)30 { walker }
    filter { fade in=(f)5; walker }
}
"#,
    );
    let config = Config::load(fx.path("video.kdl")).unwrap();
    let source = SceneSource::new(config, 1, registry(), fx.store());
    let (all, _) = render(&mut source.build().unwrap(), 0..60);

    // two boundaries: inside the first child, and inside the second
    for boundary in [17, 41] {
        let mut a = source.build().unwrap();
        render(&mut a, 0..boundary);
        let saved = a.save_state();
        let mut b = source.build().unwrap();
        assert!(b.load_state(&saved));
        let (rest, steps) = render(&mut b, boundary..60);
        assert_eq!(rest, all[boundary as usize..], "from {boundary}");
        assert_eq!(steps, 60 - boundary as u64, "no catch-up from {boundary}");

        // without the state: the same frames, caught up from the start
        let (rest, steps) = render(&mut source.build().unwrap(), boundary..60);
        assert_eq!(rest, all[boundary as usize..]);
        assert!(steps > 60 - boundary as u64);
    }
    // bytes that aren't a state load nothing, and change nothing
    let mut c = source.build().unwrap();
    assert!(!c.load_state(b"junk"));
    assert_eq!(render(&mut c, 0..60).0, all);
}

#[test]
fn a_segment_starts_from_the_state_its_predecessor_saved() {
    if !have_ffmpeg() {
        return;
    }
    let kdl = r#"
video rate=30 width=64 height=36
output "film.mp4" segment=(f)20 preset="ultrafast"
sequence "main" to=(f)40 { walker }
"#;
    let fx = Fx::new(kdl);
    let p = fx.plan(&registry(), &[]);
    let segs = p.output.segments();
    fx.build(&p, &[Target::all(segs[0].node)]);
    let r = fx.build(&p, &[Target::all(segs[1].node)]);
    let log = fx.log(&r, &seg(1));
    assert!(
        log.contains("resumed from the state output/segment-00000 saved at frame 20"),
        "{log}"
    );

    // the same segment rendered with no predecessor: caught up, and the
    // same frames
    let alone = Fx::new(kdl);
    let p2 = alone.plan(&registry(), &[]);
    let r2 = alone.build(&p2, &[Target::all(p2.output.segments()[1].node)]);
    assert!(alone.log(&r2, &seg(1)).contains("catching up"));
    let with_state = decode(&fx.built_dir(&seg(1)).join("segment.mp4"));
    let caught_up = decode(&alone.built_dir(&seg(1)).join("segment.mp4"));
    assert_eq!(with_state.len(), 20);
    assert!(with_state == caught_up, "decoded frames differ");
}

#[test]
fn output_config_errors_are_located() {
    let fx = Fx::new(
        r#"
video rate=30 width=63 height=36
output "film.mp4"
sequence "main" to=(f)4 { walker }
"#,
    );
    let config = Config::load(fx.path("video.kdl")).unwrap();
    let e = videbimus_output::OutputSpec::from_config(&config)
        .unwrap_err()
        .to_string();
    assert!(e.contains("needs an even size, and this is 63×36"), "{e}");
    assert!(e.contains("video.kdl:2"), "{e}");

    std::fs::write(
        fx.path("video.kdl"),
        "video rate=30 width=64 height=36\noutput \"film.mp4\" preset=\"fastest\"\n",
    )
    .unwrap();
    let config = Config::load(fx.path("video.kdl")).unwrap();
    let e = videbimus_output::OutputSpec::from_config(&config)
        .unwrap_err()
        .to_string();
    assert!(e.contains("there is no x264 preset `fastest`"), "{e}");
    assert!(e.contains("did you mean `faster`?"), "{e}");

    std::fs::write(
        fx.path("video.kdl"),
        "video rate=30 width=64 height=36\noutput \"film.mp4\" { audio asset=\"mx\"; }\nsequence \"main\" to=(f)4 { walker }\n",
    )
    .unwrap();
    let config = Config::load(fx.path("video.kdl")).unwrap();
    let mut graph = Graph::new();
    graph.add(Node::new(Wav { seconds: 1.0 })).unwrap();
    let cx = PlanContext {
        config: &config,
        root: Some(2),
        registry: &registry(),
        store: &fx.store(),
        scene_identity: Vec::new(),
        wired: &[],
        asset: &|_| None,
        asset_names: vec!["mix"],
    };
    let e = Output::plan(&cx, &mut graph).unwrap_err().to_string();
    assert!(e.contains("there is no asset `mx` in this config"), "{e}");
    assert!(e.contains("did you mean `mix`?"), "{e}");
}
