//! Component trees from KDL, through the registry: random access equals
//! sequential rendering, and each built-in kind draws what it should.

#![allow(
    clippy::float_cmp,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::needless_raw_string_hashes
)]

use std::cell::Cell;
use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::path::Path;

use videbimus_config::{Config, Error, MemLoader, Node, Resolver};
use videbimus_core::{Checkpoint, Component, Context, Frame, Horizon, Image, Interval};
use videbimus_runtime::color::{read_png, write_png};
use videbimus_runtime::{
    Builder, ComponentKind, ComponentRegistry, DriveOptions, Scene, scene_from_config,
};

thread_local! {
    /// Frames stepped by every walker on this thread.
    static STEPS: Cell<u64> = const { Cell::new(0) };
}

fn steps() -> u64 {
    STEPS.with(Cell::get)
}

/// A stateful test kind. With `window=H` its state is the sum of its last
/// H + 1 frame numbers (horizon H); without, a running hash of every frame
/// (unbounded). It draws its state into red and green.
struct Walker {
    interval: Interval,
    horizon: Horizon,
    state: u64,
    window: VecDeque<u64>,
}

impl Component for Walker {
    fn interval(&self) -> Interval {
        self.interval
    }
    fn horizon(&self) -> Horizon {
        self.horizon
    }
    fn reset(&mut self, _: &Context) {
        self.state = 0;
        self.window.clear();
    }
    #[allow(clippy::cast_sign_loss)]
    fn step(&mut self, n: Frame, _: &Context) {
        STEPS.with(|s| s.set(s.get() + 1));
        match self.horizon {
            Horizon::Frames(h) => {
                self.window.push_back(n.0 as u64);
                while self.window.len() > h as usize + 1 {
                    self.window.pop_front();
                }
                self.state = self.window.iter().sum();
            }
            _ => {
                self.state = (self.state ^ n.0 as u64)
                    .wrapping_mul(0x0100_0000_01b3)
                    .rotate_left(7);
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    fn render(&mut self, _: Frame, _: &Context, out: &mut Image) {
        let r = (self.state % 4093) as f32 / 4093.0;
        let g = (self.state / 4093 % 4093) as f32 / 4093.0;
        out.fill([r, g, 0.5, 1.0]);
    }
    fn checkpoint(&self) -> Option<Checkpoint> {
        Some(Box::new((self.state, self.window.clone())))
    }
    fn restore(&mut self, cp: &Checkpoint) -> bool {
        cp.downcast_ref::<(u64, VecDeque<u64>)>()
            .map(|(s, w)| {
                self.state = *s;
                self.window.clone_from(w);
            })
            .is_some()
    }
}

impl ComponentKind for Walker {
    const KIND: &'static str = "walker";
    fn from_config(node: &Node, r: &Resolver, b: &Builder) -> Result<Box<dyn Component>, Error> {
        b.check_props(node, r, &["window"])?;
        Ok(Box::new(Walker {
            interval: b.interval(node, r)?,
            horizon: r
                .opt::<u32>(node, "window")?
                .map_or(Horizon::Unbounded, Horizon::Frames),
            state: 0,
            window: VecDeque::new(),
        }))
    }
}

/// A flat grey, `value` in every colour channel, opaque.
struct Flat {
    interval: Interval,
    value: f32,
}

impl Component for Flat {
    fn interval(&self) -> Interval {
        self.interval
    }
    fn reset(&mut self, _: &Context) {}
    fn render(&mut self, _: Frame, _: &Context, out: &mut Image) {
        out.fill([self.value, self.value, self.value, 1.0]);
    }
}

impl ComponentKind for Flat {
    const KIND: &'static str = "flat";
    #[allow(clippy::cast_possible_truncation)]
    fn from_config(node: &Node, r: &Resolver, b: &Builder) -> Result<Box<dyn Component>, Error> {
        b.check_props(node, r, &["value"])?;
        Ok(Box::new(Flat {
            interval: b.interval(node, r)?,
            value: r.get::<f64>(node, "value")? as f32,
        }))
    }
}

fn registry() -> ComponentRegistry {
    ComponentRegistry::with_builtins()
        .with::<Walker>()
        .with::<Flat>()
}

const VIDEO: &str = "video rate=30 width=4 height=2\n";

fn load(body: &str) -> Result<Config, Error> {
    let loader = MemLoader::new().with("v.kdl", format!("{VIDEO}{body}"));
    Config::load_with("v.kdl", &loader)
}

/// The scene of the first non-`video` node of `body`.
fn try_scene(body: &str, options: DriveOptions) -> Result<Scene, Error> {
    let config = load(body)?;
    let root = config
        .nodes()
        .iter()
        .find(|n| n.kind() != "video")
        .expect("a root");
    scene_from_config(&config, root, &registry(), options, 0)
}

fn scene(body: &str) -> Scene {
    try_scene(body, DriveOptions::default()).unwrap_or_else(|e| panic!("{e}"))
}

fn error(body: &str) -> Error {
    try_scene(body, DriveOptions::default()).expect_err("a config error")
}

fn px(s: &mut Scene, n: i64) -> [f32; 4] {
    s.render(Frame(n)).pixels()[0]
}

fn close(a: [f32; 4], b: [f32; 4], tol: f32) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= tol)
}

const TREE: &str = r#"
sequence "root" to=(f)150 {
    walker "a" length=(f)30
    stack length=(f)60 {
        walker "b" fade-in=(f)10
        walker "c" from=(f)50 to=(f)80 window=5 blend="add" opacity=0.5
    }
    filter {
        grain amount=0.2 seed=3
        vignette strength=0.6
        fade out=(f)20
        crossfade overlap=(f)8 {
            walker "d" length=(f)30
            walker "e" window=12
        }
    }
}
"#;

#[test]
fn random_access_through_containers_equals_sequential_rendering() {
    let options = DriveOptions {
        checkpoint_every: NonZeroU32::new(16),
    };
    let mut seq = try_scene(TREE, options).unwrap();
    let frames: Vec<Image> = (0..150).map(|n| seq.render(Frame(n))).collect();
    let mut random = try_scene(TREE, options).unwrap();
    // back, forward, back again, into hidden gaps and out of them
    let order = [
        149, 0, 75, 74, 31, 140, 10, 89, 90, 91, 45, 46, 120, 29, 30, 112, 113, 60, 0, 149,
    ];
    for n in order {
        assert_eq!(random.render(Frame(n)), frames[n as usize], "frame {n}");
    }
    // and with no checkpoints at all
    let mut bare = try_scene(
        TREE,
        DriveOptions {
            checkpoint_every: None,
        },
    )
    .unwrap();
    for n in order {
        assert_eq!(bare.render(Frame(n)), frames[n as usize], "frame {n}");
    }
}

#[test]
fn a_finite_horizon_bounds_catch_up_inside_containers() {
    let mut s = scene(
        r#"
        sequence to=(f)1000 {
            stack { walker "w" window=5 }
        }
        "#,
    );
    for n in 0..10 {
        px(&mut s, n);
    }
    let before = steps();
    px(&mut s, 500);
    assert_eq!(steps() - before, 6, "frames 495 to 500, not 490 frames");
}

#[test]
fn checkpoints_are_used_inside_containers() {
    let body = r#"filter to=(f)1000 { grain; walker "w" }"#;
    let options = DriveOptions {
        checkpoint_every: NonZeroU32::new(50),
    };
    let mut s = try_scene(body, options).unwrap();
    for n in 0..=200 {
        px(&mut s, n);
    }
    let before = steps();
    let rewound = s.render(Frame(120));
    assert_eq!(
        steps() - before,
        20,
        "restored frame 100, stepped 101 to 120"
    );
    let mut fresh = try_scene(body, options).unwrap();
    assert_eq!(fresh.render(Frame(120)), rewound);
}

#[test]
fn hidden_layers_are_not_ticked() {
    // the walker is invisible until frame 20: never stepped before it shows
    let mut s = scene(r#"stack to=(f)100 { walker "w" window=3 opacity=0 }"#);
    let before = steps();
    for n in 0..50 {
        px(&mut s, n);
    }
    assert_eq!(steps(), before);
}

#[test]
fn sequences_dispatch_at_boundaries() {
    let mut s = scene(
        r#"
        sequence to=(f)30 {
            flat value=0.25 length=(f)10
            flat value=0.5 length=(f)10
            flat value=1 from=(f)25
        }
        "#,
    );
    let grey = |v: f32| [v, v, v, 1.0];
    assert_eq!(px(&mut s, 0), grey(0.25));
    assert_eq!(px(&mut s, 9), grey(0.25));
    assert_eq!(px(&mut s, 10), grey(0.5));
    assert_eq!(px(&mut s, 19), grey(0.5));
    assert_eq!(px(&mut s, 20), [0.0; 4], "a gap");
    assert_eq!(px(&mut s, 24), [0.0; 4]);
    assert_eq!(px(&mut s, 25), grey(1.0));
    assert_eq!(px(&mut s, 29), grey(1.0));
    assert_eq!(px(&mut s, 30), [0.0; 4], "after the end");
}

#[test]
fn stacks_layer_by_z_with_opacity_and_blend() {
    // the first child has the higher z, so it is on top
    let mut s = scene(
        r#"
        stack to=(f)10 {
            flat value=1 z=1 opacity=0.25
            flat value=0.2
        }
        "#,
    );
    assert!(close(px(&mut s, 0), [0.4, 0.4, 0.4, 1.0], 1e-6));
    let mut s = scene(
        r#"
        stack to=(f)10 {
            flat value=0.2
            flat value=0.5 blend="add"
            flat value=0.5 blend="multiply"
            flat value=1 from=(f)4 fade-in=(f)4 fade-out=(f)2 ease="smooth"
        }
        "#,
    );
    assert!(close(px(&mut s, 0), [0.35, 0.35, 0.35, 1.0], 1e-6));
    assert!(
        close(px(&mut s, 4), [0.35, 0.35, 0.35, 1.0], 1e-6),
        "faded in from 0"
    );
    assert!(
        close(px(&mut s, 6), [0.675, 0.675, 0.675, 1.0], 1e-6),
        "halfway"
    );
    assert!(
        close(px(&mut s, 9), [0.35, 0.35, 0.35, 1.0], 1e-6),
        "faded out to 0"
    );
}

#[test]
fn a_crossfade_mixes_evenly_at_the_middle_of_its_overlap() {
    let mut s = scene(
        r#"
        crossfade to=(f)40 overlap=(f)10 {
            flat value=1 to=(f)20
            flat value=0
        }
        "#,
    );
    assert_eq!(px(&mut s, 9), [1.0, 1.0, 1.0, 1.0], "before the overlap");
    assert_eq!(px(&mut s, 10), [1.0, 1.0, 1.0, 1.0], "where it starts");
    assert!(
        close(px(&mut s, 15), [0.5, 0.5, 0.5, 1.0], 1e-6),
        "the middle"
    );
    assert!(px(&mut s, 19)[0] < 0.15);
    assert_eq!(px(&mut s, 20), [0.0, 0.0, 0.0, 1.0], "after it");
}

#[test]
fn fades_hit_black_exactly_at_their_ends() {
    let mut s = scene(
        r#"
        filter to=(f)10 {
            fade in=(f)4 out=(f)4
            flat value=0.8
        }
        "#,
    );
    let black = [0.0, 0.0, 0.0, 1.0];
    assert_eq!(px(&mut s, 0), black);
    assert!(close(px(&mut s, 2), [0.4, 0.4, 0.4, 1.0], 1e-6));
    assert_eq!(px(&mut s, 5), [0.8, 0.8, 0.8, 1.0]);
    assert_eq!(px(&mut s, 9), black);
}

#[test]
fn grain_is_the_same_however_a_frame_is_reached() {
    let body = r#"filter to=(f)20 { grain amount=0.1 seed=9; flat value=0.5 }"#;
    let mut a = scene(body);
    let seq: Vec<Image> = (0..20).map(|n| a.render(Frame(n))).collect();
    let mut b = scene(body);
    for n in [13, 2, 19, 7] {
        assert_eq!(b.render(Frame(n)), seq[n as usize]);
    }
    assert_ne!(seq[3], seq[4], "it changes every frame");
    let other =
        scene(r#"filter to=(f)20 { grain amount=0.1 seed=10; flat value=0.5 }"#).render(Frame(3));
    assert_ne!(other, seq[3], "and with the seed");
}

/// Writes a 2×1 PNG of one sRGB grey.
fn grey_png(path: &Path, v: f32) {
    let mut img = Image::new(2, 1);
    let lin = videbimus_runtime::color::srgb_to_linear(v);
    img.fill([lin, lin, lin, 1.0]);
    write_png(&img, path).unwrap();
}

fn scene_file(dir: &Path, body: &str) -> Result<Scene, Error> {
    let path = dir.join("v.kdl");
    std::fs::write(&path, format!("{VIDEO}{body}")).unwrap();
    let config = Config::load(&path)?;
    let root = config.nodes().iter().find(|n| n.kind() != "video").unwrap();
    scene_from_config(&config, root, &registry(), DriveOptions::default(), 0)
}

#[test]
fn frame_players_show_file_n_and_placeholders_for_missing_frames() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("frames")).unwrap();
    grey_png(&dir.path().join("frames/f_001.png"), 0.2);
    grey_png(&dir.path().join("frames/f_002.png"), 0.6);
    grey_png(&dir.path().join("frames/f_004.png"), 1.0);
    let mut s = scene_file(
        dir.path(),
        r#"frame-player to=(f)4 dir="frames" pattern="f_###.png" start-index=1 fit="fill""#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let want = |v: f32| {
        let l = videbimus_runtime::color::srgb_to_linear(v);
        [l, l, l, 1.0]
    };
    assert!(close(px(&mut s, 0), want(0.2), 1e-3));
    assert!(close(px(&mut s, 1), want(0.6), 1e-3));
    assert!(close(px(&mut s, 3), want(1.0), 1e-3));
    // f_003.png is missing: a placeholder, not a failure
    let missing = s.render(Frame(2));
    assert!(missing.pixels().iter().all(|p| p[3] == 1.0));
    let mut plain = Image::new(4, 2);
    videbimus_runtime::raster::placeholder(&mut plain, "MISSING 3");
    assert_eq!(missing, plain);
}

#[test]
fn frame_players_read_committed_chunks_from_the_store() {
    use videbimus_store::{CommitOptions, KeyBuilder, Shape, Store, frames, key_hex};
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = Store::open(dir.path().join("store")).unwrap();
    let inputs = KeyBuilder::new("test-frames")
        .config(b"{}")
        .build()
        .unwrap();
    let asset = store
        .begin(
            "s01/frames",
            &inputs,
            Shape::Frames(Interval::new(Frame(0), Frame(4))),
        )
        .unwrap();
    let chunk = asset.chunk(frames(0, 2)).unwrap();
    grey_png(&chunk.out_dir().join("0000.png"), 0.3);
    grey_png(&chunk.out_dir().join("0001.png"), 0.7);
    chunk.commit(CommitOptions::default()).unwrap();
    let key = key_hex(&asset.key());
    drop(asset);
    drop(store);
    for source in [format!("key=\"{key}\""), "name=\"s01/frames\"".to_owned()] {
        let mut s = scene_file(
            dir.path(),
            &format!(r#"frame-player to=(f)4 store="store" {source} fit="fill""#),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let l = videbimus_runtime::color::srgb_to_linear(0.7);
        assert!(close(px(&mut s, 1), [l, l, l, 1.0], 4e-3), "{source}");
        // frames 2 and 3 aren't built yet
        let mut plain = Image::new(4, 2);
        videbimus_runtime::raster::placeholder(&mut plain, "MISSING 2");
        assert_eq!(s.render(Frame(2)), plain, "{source}");
    }
}

#[test]
fn a_small_scene_renders_expected_pixels() {
    let dir = tempfile::tempdir().unwrap();
    grey_png(&dir.path().join("card.png"), 0.5);
    let font = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fonts/jost/Jost-500-Medium.ttf");
    let body = format!(
        r##"
        sequence to=(f)20 {{
            image "card" length=(f)10 path="card.png" fit="fill"
            stack {{
                flat value=0
                caption "title" text="I" font="{}" size=40 in=(f)5 color="#ff0000"
            }}
        }}
        "##,
        font.display()
    );
    let mut s = scene_file(dir.path(), &body).unwrap_or_else(|e| panic!("{e}"));
    let l = videbimus_runtime::color::srgb_to_linear(0.5);
    assert!(
        s.render(Frame(3))
            .pixels()
            .iter()
            .all(|p| close(*p, [l, l, l, 1.0], 4e-3))
    );
    // the caption's first frame is fully faded: black
    assert!(
        s.render(Frame(10))
            .pixels()
            .iter()
            .all(|p| *p == [0.0, 0.0, 0.0, 1.0])
    );
    // then red text on black, nothing green or blue
    let img = s.render(Frame(19));
    assert!(img.pixels().iter().any(|p| p[0] > 0.5), "some text");
    assert!(img.pixels().iter().all(|p| p[1] == 0.0 && p[2] == 0.0));
    // big enough to see: write a debug PNG and read it back
    let out = dir.path().join("out.png");
    write_png(&img, &out).unwrap();
    assert_eq!(read_png(&out).unwrap().width(), 4);
}

#[test]
fn captions_render_text_where_asked() {
    let font = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fonts/jost/Jost-500-Medium.ttf");
    let loader = MemLoader::new().with(
        "v.kdl",
        format!(
            "video rate=30 width=64 height=32\ncaption to=(f)1 text=\"HH\" font=\"{}\" size=16 x=0.25 align=\"left\"",
            font.display()
        ),
    );
    let config = Config::load_with("v.kdl", &loader).unwrap();
    let root = config.nodes_of("caption").next().unwrap();
    let mut s = scene_from_config(&config, root, &registry(), DriveOptions::default(), 0)
        .unwrap_or_else(|e| panic!("{e}"));
    let img = s.render(Frame(0));
    let inked: Vec<usize> = (0..img.pixels().len())
        .filter(|&i| img.pixels()[i][3] > 0.5)
        .map(|i| i % 64)
        .collect();
    assert!(!inked.is_empty());
    let (lo, hi) = (*inked.iter().min().unwrap(), *inked.iter().max().unwrap());
    assert!((15..=18).contains(&lo), "starts at x = 16: {lo}");
    assert!(hi < 40, "two letters at 16 px: {hi}");
}

#[test]
fn unknown_kinds_are_located_with_a_suggestion() {
    let e = error("sequence to=(f)10 {\n    stak { flat value=1 }\n}");
    assert_eq!(e.message(), "there is no component kind `stak`");
    let at = &e.origin().unwrap().at;
    assert_eq!((at.line(), at.column()), (3, 5));
    assert_eq!(e.help_text(), Some("did you mean `stack`?"));
}

#[test]
fn bad_properties_are_located() {
    let e = error("stack to=(f)10 {\n    flat value=1 opacty=0.5\n}");
    assert_eq!(e.message(), "flat has no property `opacty`");
    assert_eq!(e.origin().unwrap().at.line(), 3);
    assert!(e.help_text().unwrap().contains("opacity"), "{e}");
    // stack properties aren't allowed outside a stack
    let e = error("sequence to=(f)10 { flat value=1 blend=\"add\" }");
    assert_eq!(e.message(), "flat has no property `blend`");
    let e = error("stack to=(f)10 { flat value=1 blend=\"screen\" }");
    assert!(e.message().contains("can't be \"screen\""), "{e}");
    let e = error("stack to=(f)10 { flat value=1 opacity=2 }");
    assert!(e.message().contains("outside 0 to 1"), "{e}");
}

#[test]
fn structural_mistakes_are_config_errors() {
    let e = error("sequence to=(f)20 { flat value=1 length=(f)10; flat value=1 from=(f)5 }");
    assert!(
        e.message().contains("before the previous child ends"),
        "{e}"
    );
    let e = error("crossfade to=(f)20 overlap=(f)2 { flat value=1 }");
    assert!(e.message().contains("has two children"), "{e}");
    let e = error("crossfade to=(f)20 { flat value=1 to=(f)10; flat value=0 }");
    assert!(e.message().contains("how long the overlap is"), "{e}");
    let e = error("filter to=(f)20 { grain }");
    assert!(e.message().contains("nothing to filter"), "{e}");
    let e = error("filter to=(f)20 { flat value=1; flat value=0 }");
    assert!(e.message().contains("a second"), "{e}");
    let e = error("stack { flat value=1 }");
    assert_eq!(e.message(), "it has no end");
    let e = error("stack from=(f)5 to=(f)10 { flat value=1 from=(f)2 }");
    assert!(e.message().contains("before its parent"), "{e}");
    let e = error("frame-player to=(f)2");
    assert!(e.message().contains("nothing to play"), "{e}");
    let e = error("image to=(f)2 path=\"nope.png\"");
    assert!(e.message().contains("no file at"), "{e}");
    let e = error("caption to=(f)2 text=\"x\" font=\"nope.ttf\"");
    assert!(e.message().contains("can't read the font"), "{e}");
}
