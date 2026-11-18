//! Errors name the file, the line, the element, the chain and the fix
//! (ADR 0005 §5). These pin the full text of a few.

mod common;

use common::{err, load, node, ok};
use videbimus_core::Frame;

#[test]
fn a_bad_value_from_a_template() {
    let e = err("\
video rate=30 { tempo bpm=104 beats-per-bar=4; }
template \"face\" length=(expr)\"$self.to - $self.from\"
shot \"s02\" use=\"face\" from=(bar)4 to=\"8\"");
    assert_eq!(
        e,
        "\
error: `-` works on numbers and times, not the string \"8\"
 --> main.kdl:2:31
  |
2 | template \"face\" length=(expr)\"$self.to - $self.from\"
  |                               ^^^^^^^^^^^^^^^^^^^^^
  = in: shot \"s02\", property `length`
  = from template \"face\" (defined at main.kdl:2:1), applied by `use` at main.kdl:3:12
  = help: \"8\" is a string; write it without quotes, with a unit if it is a time: (bar)8
"
    );
}

#[test]
fn a_kind_reading_the_wrong_type() {
    let c = ok("\
video rate=30 { tempo bpm=104 beats-per-bar=4; }
shot \"s02\" from=(bar)4 to=8");
    let e = c
        .resolver()
        .interval(node(&c, "s02"), "from", "to")
        .unwrap_err()
        .to_string();
    assert_eq!(
        e,
        "\
error: expected a time, found the number 8
 --> main.kdl:2:24
  |
2 | shot \"s02\" from=(bar)4 to=8
  |                        ^^^^
  = in: shot \"s02\", property `to`
  = help: give it a unit: (bar)8, (beat)8, (s)8 or (f)8
"
    );
}

#[test]
fn a_missing_property_and_a_near_miss() {
    let c = ok("shot \"s02\" form=(bar)4 to=(bar)8");
    let e = c
        .resolver()
        .get::<Frame>(node(&c, "s02"), "from")
        .unwrap_err()
        .to_string();
    assert!(e.contains("error: shot \"s02\" has no `from`"), "{e}");
    assert!(e.contains("--> main.kdl:1:1"), "{e}");
    assert!(
        e.contains("help: is `form` a misspelling of `from`?"),
        "{e}"
    );
}

#[test]
fn unknown_properties_suggest_the_known_ones() {
    let c = ok("shot \"s02\" from=(bar)4 too=(bar)8");
    let e = c
        .resolver()
        .check_props(node(&c, "s02"), &["from", "to"])
        .unwrap_err()
        .to_string();
    assert!(e.contains("error: shot has no property `too`"), "{e}");
    assert!(e.contains("--> main.kdl:1:24"), "{e}");
    assert!(e.contains("did you mean `to`?"), "{e}");
}

#[test]
fn an_interval_backwards() {
    let c =
        ok("video rate=30 { tempo bpm=104 beats-per-bar=4; }\nshot \"s\" from=(bar)8 to=(bar)4");
    let e = c
        .resolver()
        .interval(node(&c, "s"), "from", "to")
        .unwrap_err()
        .to_string();
    assert!(e.contains("`to` (f277) is before `from` (f554)"), "{e}");
}

#[test]
fn an_error_in_an_included_file_inside_an_each() {
    let e = load(&[
        ("main.kdl", "each \"n\" 1 2 {\n    include \"parts.kdl\"\n}"),
        ("parts.kdl", "part v=(expr)\"$n / 0\""),
    ])
    .unwrap_err()
    .to_string();
    assert_eq!(
        e,
        "\
error: division by zero
 --> parts.kdl:1:15
  |
1 | part v=(expr)\"$n / 0\"
  |               ^^^^^^
  = in: part, property `v`
  = in a file included at main.kdl:2:5
  = in `each \"n\"` at main.kdl:1:1, where $n = 1
"
    );
}

#[test]
fn kdl_syntax_errors_are_located() {
    let e = err("shot \"s\" {\n    key u=\n}");
    assert!(e.starts_with("error: "), "{e}");
    assert!(e.contains("--> main.kdl:"), "{e}");
}
