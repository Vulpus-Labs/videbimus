//! Units and unit-checked arithmetic.

mod common;

use common::{VIDEO, err, node, num, ok};
use videbimus_config::{Time, Unit, Value};
use videbimus_core::{Frame, Rate};

fn time(config: &videbimus_config::Config, name: &str, key: &str) -> Time {
    config
        .resolver()
        .get(node(config, name), key)
        .unwrap_or_else(|e| panic!("{e}"))
}

fn frame(config: &videbimus_config::Config, name: &str, key: &str) -> Frame {
    config
        .resolver()
        .get(node(config, name), key)
        .unwrap_or_else(|e| panic!("{e}"))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn literals_carry_units_and_convert_to_frames() {
    let c = ok(&format!(
        "{VIDEO}x \"a\" bar=(bar)4 beat=(beat)16 s=(s)0.5 f=(f)15 n=4"
    ));
    assert_eq!(time(&c, "a", "bar"), Time::new(4.0, Unit::Bar));
    assert_eq!(frame(&c, "a", "bar"), Frame(277));
    assert_eq!(frame(&c, "a", "beat"), Frame(277));
    assert_eq!(frame(&c, "a", "s"), Frame(15));
    assert_eq!(frame(&c, "a", "f"), Frame(15));
    assert_eq!(node(&c, "a").prop("n").unwrap().value, Value::Number(4.0));
    let tb = c.timebase().unwrap();
    assert_eq!(tb.rate, Rate::new(30, 1));
    assert_eq!(tb.tempo.unwrap().beats_per_bar, 4);
}

#[test]
fn fractional_rates() {
    let c = ok("video rate=\"30000/1001\"\nx \"a\" t=(s)1001");
    assert_eq!(c.timebase().unwrap().rate, Rate::new(30_000, 1001));
    assert_eq!(frame(&c, "a", "t"), Frame(30_000));
}

#[test]
fn arithmetic_converts_to_the_left_operands_unit() {
    let c = ok(&format!(
        "{VIDEO}x \"a\" from=(bar)4 \\
            end=(expr)\"$self.from + 0.5s\" \\
            mixed=(expr)\"4bar - 2beat\" \\
            ratio=(expr)\"1bar / 1beat\" \\
            scaled=(expr)\"2 * $self.from / 4\" \\
            neg=(expr)\"-(1bar)\" \\
            ops=(expr)\"2 × 3 − 1 ÷ 2\" \\
            fr=(expr)\"frames(1bar)\""
    ));
    let end = time(&c, "a", "end");
    assert_eq!(end.unit, Unit::Bar);
    assert!(close(end.amount, 4.0 + 0.5 * 104.0 / 240.0));
    // rounded once, from the exact position: 276.92 + 15
    assert_eq!(frame(&c, "a", "end"), Frame(292));
    assert_eq!(time(&c, "a", "mixed"), Time::new(3.5, Unit::Bar));
    assert!(close(num(&c, node(&c, "a"), "ratio"), 4.0));
    assert_eq!(time(&c, "a", "scaled"), Time::new(2.0, Unit::Bar));
    assert_eq!(time(&c, "a", "neg"), Time::new(-1.0, Unit::Bar));
    assert!(close(num(&c, node(&c, "a"), "ops"), 5.5));
    assert_eq!(time(&c, "a", "fr"), Time::new(69.0, Unit::Frame));
}

#[test]
fn min_max_clamp() {
    let c = ok(&format!(
        "{VIDEO}x \"a\" \\
            min=(expr)\"min(4bar, 10s)\" \\
            max=(expr)\"max(4bar, 10s)\" \\
            n=(expr)\"clamp(5, 0, 3)\" \\
            t=(expr)\"clamp(1s, 2f, 10f)\" \\
            one=(expr)\"max(2beat)\""
    ));
    assert_eq!(time(&c, "a", "min"), Time::new(4.0, Unit::Bar));
    let max = time(&c, "a", "max");
    assert_eq!(max.unit, Unit::Bar);
    assert!(close(max.amount, 10.0 * 104.0 / 240.0));
    assert!(close(num(&c, node(&c, "a"), "n"), 3.0));
    let t = time(&c, "a", "t");
    assert_eq!(t.unit, Unit::Second);
    assert!(close(t.amount, 1.0 / 3.0));
    assert_eq!(time(&c, "a", "one"), Time::new(2.0, Unit::Beat));
}

#[test]
fn same_units_need_no_timebase() {
    let c = ok("x \"a\" t=(expr)\"1bar + 3bar\" n=(expr)\"(1 + 2) * 4\"");
    assert_eq!(time(&c, "a", "t"), Time::new(4.0, Unit::Bar));
    assert!(close(num(&c, node(&c, "a"), "n"), 12.0));
    assert!(c.timebase().is_none());
}

#[test]
fn unit_errors() {
    let e = err(&format!(
        "{VIDEO}x \"a\" from=(bar)4 t=(expr)\"$self.from + 1\""
    ));
    assert!(
        e.contains("can't add a time and a plain number (4bar + 1)"),
        "{e}"
    );
    assert!(e.contains("help: give the number a unit: 1bar"), "{e}");

    let e = err(&format!("{VIDEO}x \"a\" t=(expr)\"1bar * 2s\""));
    assert!(e.contains("can't multiply two times"), "{e}");

    let e = err(&format!("{VIDEO}x \"a\" t=(expr)\"2 / 1s\""));
    assert!(e.contains("can't divide a plain number by a time"), "{e}");

    let e = err(&format!("{VIDEO}x \"a\" t=(expr)\"min(1bar, 2)\""));
    assert!(
        e.contains("min() mixes times and plain numbers: 1bar, 2"),
        "{e}"
    );

    let e = err(&format!("{VIDEO}x \"a\" t=(expr)\"clamp(1, 3, 2)\""));
    assert!(
        e.contains("low bound (3) is above its high bound (2)"),
        "{e}"
    );

    let e = err(&format!("{VIDEO}x \"a\" t=(expr)\"frames(3)\""));
    assert!(e.contains("frames() takes one time, and got 3"), "{e}");

    let e = err(&format!("{VIDEO}x \"a\" t=(expr)\"1 / (1 - 1)\""));
    assert!(e.contains("division by zero"), "{e}");
}

#[test]
fn missing_tempo_or_rate() {
    let e = err("video rate=30\nx \"a\" t=(expr)\"1bar + 1s\"");
    assert!(e.contains("bars and beats need the song's tempo"), "{e}");
    assert!(
        e.contains("help: add `tempo bpm=… beats-per-bar=…` inside `video`"),
        "{e}"
    );

    let e = err("x \"a\" t=(expr)\"1bar + 1s\"");
    assert!(
        e.contains("converting s to bar needs the video's frame rate"),
        "{e}"
    );
    assert!(e.contains("help: add `video rate=30"), "{e}");

    let c = ok("x \"a\" t=(bar)1");
    let e = c
        .resolver()
        .get::<Frame>(node(&c, "a"), "t")
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("1bar can't become a frame: the config has no frame rate"),
        "{e}"
    );
    assert!(e.contains("main.kdl:1:7"), "{e}");
}

#[test]
fn annotation_errors() {
    let e = err("x \"a\" t=(bars)4");
    assert!(e.contains("unknown type annotation (bars)"), "{e}");
    assert!(e.contains("did you mean (bar)?"), "{e}");

    let e = err("x \"a\" t=(f)1.5");
    assert!(e.contains("(f)1.5 isn't a whole number of frames"), "{e}");

    let e = err("x \"a\" t=(s)\"0.5\"");
    assert!(
        e.contains("(s) takes a number, not the string \"0.5\""),
        "{e}"
    );

    let e = err("x \"a\" t=(expr)4");
    assert!(e.contains("(expr) takes a string"), "{e}");

    // ADR 0005 first wrote expressions as (=)"…", which KDL v2 rejects
    let e = err("x \"a\" t=(=)\"1 + 1\"");
    assert!(
        e.contains("write (expr)") || e.contains("written (expr)"),
        "{e}"
    );
    assert!(e.contains("not (=)"), "{e}");
}

#[test]
fn expression_syntax_errors_point_inside_the_string() {
    let e = err("x \"a\" t=(expr)\"1 + 4 bar\"");
    assert!(
        e.contains("a space separates the unit `bar` from its number"),
        "{e}"
    );
    // column of `bar` inside the string: x "a" t=(expr)"1 + 4 bar"
    //                                     1234567890123456789012
    assert!(e.contains("main.kdl:1:22"), "{e}");
    assert!(e.contains("^^^"), "{e}");

    let e = err("x \"a\" t=(expr)\"sqrt(4)\"");
    assert!(e.contains("unknown name `sqrt`"), "{e}");
    assert!(e.contains("no user-defined functions"), "{e}");
}

#[test]
fn video_errors() {
    let e = err("video { tempo bpm=104 beats-per-bar=4; }");
    assert!(e.contains("`video` has no frame rate"), "{e}");
    let e = err("video rate=29.97");
    assert!(e.contains("expected a whole number"), "{e}");
    let e = err("video rate=\"30/0\"");
    assert!(e.contains("`30/0` isn't a frame rate"), "{e}");
    let e = err("video rate=30 { tempo bpm=0 beats-per-bar=4; }");
    assert!(e.contains("a tempo of 0 BPM can't be played"), "{e}");
    let e = err("video rate=30 { tempo bpm=104 beats-per-bra=4; }");
    assert!(e.contains("tempo has no property `beats-per-bra`"), "{e}");
    assert!(e.contains("did you mean `beats-per-bar`?"), "{e}");
    let e = err("video rate=30\nvideo rate=24");
    assert!(e.contains("a config has one `video` node"), "{e}");
    assert!(e.contains("main.kdl:2:1"), "{e}");
}
