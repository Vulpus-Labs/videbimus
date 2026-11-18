//! `$name`, `$self.*`, `$parent.*`, `$prev.*`, and cycles.

mod common;

use common::{VIDEO, err, node, num, ok};
use videbimus_config::{Time, Unit, Value};

#[test]
fn lets_resolve_lazily_and_scope_by_block() {
    let c = ok(
        "let a=$b b=2\nlet tail=(s)0.5\nx \"top\" a=$a t=$tail {\n    let a=10\n    y \"inner\" a=$a b=$b\n}",
    );
    let top = node(&c, "top");
    assert!((num(&c, top, "a") - 2.0).abs() < f64::EPSILON);
    let t: Time = c.resolver().get(top, "t").unwrap();
    assert_eq!(t, Time::new(0.5, Unit::Second));
    let inner = &top.children()[0];
    assert!(
        (num(&c, inner, "a") - 10.0).abs() < f64::EPSILON,
        "shadowed"
    );
    assert!((num(&c, inner, "b") - 2.0).abs() < f64::EPSILON, "outer");

    let e = err("x \"a\" {\n    let inner=1\n}\ny \"b\" v=$inner");
    assert!(e.contains("nothing called `$inner` is defined here"), "{e}");
    assert!(e.contains("main.kdl:4:7"), "{e}");
}

#[test]
fn self_refers_forwards_and_backwards() {
    let c = ok(&format!(
        "{VIDEO}shot \"a\" end=(expr)\"$self.start + $self.length\" start=(bar)2 length=(bar)1 n=$self.name"
    ));
    let a = node(&c, "a");
    let end: Time = c.resolver().get(a, "end").unwrap();
    assert_eq!(end, Time::new(3.0, Unit::Bar));
    assert_eq!(a.prop("n").unwrap().value, Value::String("a".into()));
}

#[test]
fn parent_and_prev() {
    let c = ok(&format!(
        "{VIDEO}\
sequence \"seq\" zoom=2 {{
    shot \"a\" from=(bar)0 to=(bar)4 {{
        camera end=7
    }}
    shot \"b\" from=$prev.to to=(expr)\"$self.from + 4bar\" z=(expr)\"$parent.zoom * 3\" {{
        camera start=$parent.prev.camera.end end=(expr)\"$parent.parent.zoom + 1\"
    }}
    shot \"c\" first=$prev.prev.name cam=$prev.camera.start
}}"
    ));
    let seq = node(&c, "seq");
    let [a, b, s3] = seq.children() else { panic!() };
    let r = c.resolver();
    let ia = r.interval(a, "from", "to").unwrap();
    let ib = r.interval(b, "from", "to").unwrap();
    assert_eq!(ia.end, ib.start);
    assert_eq!(ib.len(), 277);
    assert!((num(&c, b, "z") - 6.0).abs() < f64::EPSILON);
    let cam = &b.children()[0];
    assert!((num(&c, cam, "start") - 7.0).abs() < f64::EPSILON);
    assert!((num(&c, cam, "end") - 3.0).abs() < f64::EPSILON);
    assert_eq!(s3.prop("first").unwrap().value, Value::String("a".into()));
    assert!((num(&c, s3, "cam") - 7.0).abs() < f64::EPSILON);
}

#[test]
fn literal_dollar() {
    let c = ok("x \"a\" price=\"$$5\"");
    assert_eq!(
        node(&c, "a").prop("price").unwrap().value,
        Value::String("$5".into())
    );
}

#[test]
fn cycles_are_errors() {
    let e = err("shot \"x\" a=$self.b b=(expr)\"$self.c + 1\" c=$self.a");
    assert!(
        e.contains("these values refer to each other in a cycle: shot \"x\".a → shot \"x\".b → shot \"x\".c → shot \"x\".a"),
        "{e}"
    );
    assert!(
        e.contains("help: give one of them a value that doesn't depend on the others"),
        "{e}"
    );

    let e = err("let a=$b b=(expr)\"$a * 2\"\nx \"y\" v=$a");
    assert!(e.contains("cycle: $a → $b → $a"), "{e}");

    let e = err("shot \"x\" a=$self.a");
    assert!(e.contains("cycle: shot \"x\".a → shot \"x\".a"), "{e}");
}

#[test]
fn reference_errors() {
    let e = err("shot \"x\" length=4 l=$self.lenght");
    assert!(e.contains("shot \"x\" has no property `lenght`"), "{e}");
    assert!(e.contains("did you mean `length`?"), "{e}");
    assert!(e.contains("main.kdl:1:19"), "{e}");

    let e = err("shot \"x\" a=$prev.to");
    assert!(
        e.contains("shot \"x\" is the first in its block, so `prev` has nothing to refer to"),
        "{e}"
    );

    let e = err("shot \"x\" a=$parent.to");
    assert!(
        e.contains("shot \"x\" is at the top level, so it has no parent"),
        "{e}"
    );

    let e = err("let a=$self.x\nshot \"x\" v=$a");
    assert!(e.contains("`$self` only works in a node's values"), "{e}");
    assert!(
        e.contains("note: needed by the reference at main.kdl:2:10"),
        "{e}"
    );

    let e = err("shot \"x\" a=$self.camera.end {\n    camera end=1\n}");
    assert!(
        e.contains("shot \"x\" isn't resolved yet, so `.camera` can't look into its children"),
        "{e}"
    );

    let e = err("shot \"a\" { camera; }\nshot \"b\" v=$prev.camra.end");
    assert!(e.contains("shot \"a\" has no child `camra`"), "{e}");
    assert!(e.contains("did you mean `camera`?"), "{e}");

    let e = err("shot \"a\" { key; key; }\nshot \"b\" v=$prev.key.t");
    assert!(
        e.contains("shot \"a\" has 2 children `key`, so `.key` is ambiguous"),
        "{e}"
    );

    let e = err("shot \"a\" v=$self");
    assert!(
        e.contains("`$self` is a node; name one of its properties"),
        "{e}"
    );

    let e = err("let a=1\nshot \"a\" v=$a.b");
    assert!(e.contains("`$a` is a value, not a node"), "{e}");

    let e = err("shot \"a\" v=\"$self.\"");
    assert!(e.contains("`$self.` isn't a valid reference"), "{e}");
}
