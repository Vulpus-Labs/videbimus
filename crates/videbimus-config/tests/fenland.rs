//! Fenland's shot list, written in KDL, resolves to the frame intervals and
//! camera keys that its Python original (`shots.py`) computes.
//!
//! The expected values are computed here from the Python's formulas, not
//! from the resolver:
//!
//! - a shot covers frames `[round(bar_s(b0)·30), round(bar_s(b1)·30))`,
//!   with `bar_s(b) = b·4·60/104`: each boundary from its own position, as
//!   the compositor placed them;
//! - camera key times are rescaled as in `export()`:
//!   `t' = t·(main − 1) / max(1, frames − 1)` with
//!   `main = round((b1 − b0)·4·60/104·30) + 1`. `frames` is the exported
//!   frame count; here it is `main + round(TAIL·30)`, the shot plus the
//!   0.5 s tail rendered past it;
//! - `k()`'s defaults: `elev=90 azim=0 lens=50 z=0.1`, and
//!   `fstop = 11 if elev >= 85 else 2.8` unless given.

use std::path::Path;

use videbimus_config::{Config, Node};
use videbimus_core::{Frame, Interval};

const BPM: f64 = 104.0;
const FPS: f64 = 30.0;
const TAIL: f64 = 0.5;

const V1_L1: Option<(&str, &str)> = Some(("0:0", "720,61"));
const V1_FX: Option<(&str, &str)> = Some(("0:2", "908,61"));
const KR_DELAY: Option<(&str, &str)> = Some(("0:1", "528,278"));

#[derive(Clone, Copy, Debug)]
struct K {
    t: f64,
    x: f64,
    y: f64,
    view: f64,
    elev: f64,
    azim: f64,
    lens: f64,
    fstop: f64,
    z: f64,
}

/// shots.py's `k(t, x, y, view, elev=90, azim=0, lens=50, fstop=None,
/// z=0.1)`, with the optional arguments in a slice.
fn k(t: f64, x: f64, y: f64, view: f64, rest: &[f64]) -> K {
    let elev = rest.first().copied().unwrap_or(90.0);
    let azim = rest.get(1).copied().unwrap_or(0.0);
    let lens = rest.get(2).copied().unwrap_or(50.0);
    let fstop = rest
        .get(3)
        .copied()
        .unwrap_or(if elev >= 85.0 { 11.0 } else { 2.8 });
    K {
        t,
        x,
        y,
        view,
        elev,
        azim,
        lens,
        fstop,
        z: 0.1,
    }
}

type Shot = (
    &'static str,
    f64,
    f64,
    &'static str,
    Option<(&'static str, &'static str)>,
    [K; 2],
    &'static [&'static str],
);

/// shots.py's FACE, transcribed.
#[rustfmt::skip]
fn face() -> Vec<Shot> {
    vec![
        ("s02", 4.0, 8.0, "pad", V1_L1, [k(0.0, 530.0, 300.0, 1180.0, &[]), k(1.0, 845.0, 165.0, 400.0, &[])], &["L1 Cutoff"]),
        ("s03", 8.0, 12.0, "keys", None, [k(0.0, 110.0, 490.0, 400.0, &[34.0, -28.0, 60.0]), k(1.0, 320.0, 488.0, 380.0, &[40.0, 18.0, 60.0])], &["Filter Cutoff"]),
        ("s04", 12.0, 16.0, "bass", None, [k(0.0, 300.0, 150.0, 430.0, &[]), k(1.0, 430.0, 158.0, 360.0, &[])], &["Low Pass"]),
        ("s05", 16.0, 20.0, "bass", None, [k(0.0, 368.0, 150.0, 250.0, &[36.0, -38.0, 70.0, 2.0]), k(1.0, 395.0, 158.0, 230.0, &[42.0, 12.0, 70.0, 2.0])], &["Low Pass"]),
        ("s07", 24.0, 28.0, "arp", V1_L1, [k(0.0, 745.0, 505.0, 470.0, &[42.0, 18.0, 55.0]), k(1.0, 845.0, 170.0, 420.0, &[50.0, -12.0, 55.0])], &["L1 Cutoff"]),
        ("s09", 32.0, 36.0, "keys", None, [k(0.0, 560.0, 275.0, 1200.0, &[]), k(1.0, 225.0, 490.0, 470.0, &[])], &["Filter Cutoff"]),
        ("s10", 36.0, 40.0, "lead", None, [k(0.0, 612.0, 150.0, 300.0, &[36.0, 25.0, 60.0]), k(1.0, 372.0, 150.0, 300.0, &[34.0, -16.0, 60.0])], &["Vibrato Depth", "Low Pass"]),
        ("s11", 40.0, 44.0, "keys", None, [k(0.0, 215.0, 185.0, 520.0, &[50.0, -30.0, 50.0]), k(1.0, 330.0, 190.0, 450.0, &[40.0, 20.0, 50.0])], &["Filter Cutoff"]),
        ("s12", 44.0, 48.0, "pad", V1_L1, [k(0.0, 845.0, 160.0, 420.0, &[]), k(1.0, 760.0, 440.0, 420.0, &[])], &["L1 Cutoff", "L1 Slot 2 Depth"]),
        ("s13", 48.0, 52.0, "pad", V1_L1, [k(0.0, 800.0, 168.0, 300.0, &[32.0, -30.0, 70.0, 2.0]), k(1.0, 880.0, 168.0, 260.0, &[38.0, 22.0, 70.0, 2.0])], &["L1 Cutoff"]),
        ("s14", 52.0, 56.0, "lead", KR_DELAY, [k(0.0, 498.0, 340.0, 380.0, &[]), k(1.0, 560.0, 350.0, 300.0, &[45.0, 15.0, 55.0])], &["Delay Mix", "Vibrato Depth"]),
        ("s16", 60.0, 64.0, "arp", V1_L1, [k(0.0, 530.0, 300.0, 1180.0, &[]), k(1.0, 880.0, 165.0, 330.0, &[])], &["L1 Cutoff", "L1 Resonance"]),
        ("s18", 68.0, 72.0, "glass", None, [k(0.0, 115.0, 180.0, 330.0, &[45.0, -22.0, 55.0]), k(1.0, 225.0, 490.0, 400.0, &[40.0, 15.0, 55.0])], &["Filter Cutoff"]),
        ("s19", 72.0, 76.0, "keys", None, [k(0.0, 225.0, 490.0, 420.0, &[]), k(1.0, 560.0, 273.0, 1240.0, &[])], &["Filter Cutoff", "Delay FB"]),
        ("s20", 76.0, 80.0, "pad", V1_FX, [k(0.0, 530.0, 330.0, 1180.0, &[]), k(1.0, 916.0, 458.0, 330.0, &[])], &["Reverb Mix"]),
    ]
}

/// Python's `round`, for values that aren't halfway between integers
/// (asserted: a bar at 104 BPM and 30 fps is 900/13 frames, never a half).
#[allow(clippy::cast_possible_truncation)]
fn py_round(x: f64) -> i64 {
    assert!((x.fract().abs() - 0.5).abs() > 1e-9, "{x} is a tie");
    x.round() as i64
}

fn bar_frame(b: f64) -> i64 {
    py_round(b * 4.0 * 60.0 / BPM * FPS)
}

fn config() -> Config {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fenland-shots.kdl");
    Config::load(&path).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn shots_match_shots_py() {
    let config = config();
    let r = config.resolver();
    let shots: Vec<&Node> = config.nodes_of("shot").collect();
    let face = face();
    assert_eq!(shots.len(), face.len());

    for (node, (name, b0, b1, inst, tab, keys, caps)) in shots.into_iter().zip(face) {
        assert_eq!(node.name(), Some(name));
        let ctx = format!("shot {name}");

        // the interval, each end from its own bar
        let interval = r.interval(node, "from", "to").unwrap();
        assert_eq!(
            interval,
            Interval::new(Frame(bar_frame(b0)), Frame(bar_frame(b1))),
            "{ctx}"
        );

        assert_eq!(r.get::<String>(node, "inst").unwrap(), inst, "{ctx}");
        assert_eq!(r.get::<i64>(node, "width").unwrap(), 1920, "{ctx}");
        assert_eq!(r.get::<i64>(node, "height").unwrap(), 1080, "{ctx}");
        let got_tab: Vec<(String, String)> = node
            .children_of("tab")
            .map(|t| (r.get(t, "page").unwrap(), r.get(t, "click").unwrap()))
            .collect();
        let want_tab: Vec<(String, String)> = tab
            .map(|(p, c)| (p.to_owned(), c.to_owned()))
            .into_iter()
            .collect();
        assert_eq!(got_tab, want_tab, "{ctx}");
        let captions = node.children_of("captions").next().unwrap();
        assert_eq!(r.args::<String>(captions).unwrap(), caps, "{ctx}");

        // camera keys, rescaled as export() does
        let main = py_round((b1 - b0) * 4.0 * 60.0 / BPM * FPS) + 1;
        let frames = main + py_round(TAIL * FPS);
        let got: Vec<&Node> = node.children_of("key").collect();
        assert_eq!(got.len(), 2, "{ctx}");
        for (j, (g, want)) in got.into_iter().zip(keys).enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let t = want.t * (main - 1) as f64 / (frames - 1).max(1) as f64;
            let ctx = format!("{ctx} key {j}");
            let field = |key: &str| r.get::<f64>(g, key).unwrap();
            assert!(
                (field("t") - t).abs() < 1e-12,
                "{ctx}: t {} vs {t}",
                field("t")
            );
            for (key, v) in [
                ("x", want.x),
                ("y", want.y),
                ("view", want.view),
                ("elev", want.elev),
                ("azim", want.azim),
                ("lens", want.lens),
                ("fstop", want.fstop),
                ("z", want.z),
            ] {
                assert!(
                    (field(key) - v).abs() < 1e-12,
                    "{ctx}: {key} {} vs {v}",
                    field(key)
                );
            }
        }
    }
}

/// The two ways of rounding differ: s07's shot length rounded as one
/// duration is 277 frames (what shots.py times its camera over), but its
/// boundaries, each rounded from its own bar, are 276 frames apart (what
/// the compositor showed). The config reproduces both.
#[test]
fn durations_and_positions_round_differently_where_python_did() {
    let config = config();
    let r = config.resolver();
    let s07 = config
        .nodes_of("shot")
        .find(|n| n.name() == Some("s07"))
        .unwrap();
    assert_eq!(r.interval(s07, "from", "to").unwrap().len(), 276);
    let shot_frames: videbimus_config::Time = r.get(s07, "shot-frames").unwrap();
    assert!((shot_frames.amount - 277.0).abs() < f64::EPSILON);
}

#[test]
fn values_remember_their_template() {
    let config = config();
    let s02 = config.nodes_of("shot").next().unwrap();
    let key = s02.children_of("key").next().unwrap();
    let fstop = key.prop("fstop").unwrap();
    assert_eq!(
        fstop.origin.at.line(),
        23,
        "fstop is written in template \"top\""
    );
    let names: Vec<String> = fstop
        .origin
        .via
        .iter()
        .map(|v| match v {
            videbimus_config::Via::Template { name, .. } => name.clone(),
            other => panic!("{other}"),
        })
        .collect();
    assert_eq!(names, ["top"]);
    let t = key.prop("t").unwrap();
    let names: Vec<String> = t.origin.via.iter().map(ToString::to_string).collect();
    assert!(names[0].starts_with("from template \"key\""), "{names:?}");
    assert!(names[1].starts_with("from template \"top\""), "{names:?}");
}
