//! Templates, includes and `each`.

mod common;

use common::{VIDEO, err, load, node, num, ok};
use videbimus_config::{Value, Via};

fn keys(n: &videbimus_config::Node) -> Vec<&str> {
    n.props().iter().map(|p| p.key.as_str()).collect()
}

fn via_names(v: &[Via]) -> Vec<String> {
    v.iter()
        .map(|v| match v {
            Via::Template { name, .. } => format!("template {name}"),
            Via::Include { at } => format!("include {}", at.path().display()),
            Via::Each { binding, item, .. } => format!("each {binding}={item}"),
        })
        .collect()
}

#[test]
fn templates_supply_defaults_that_nodes_override() {
    let c = ok("template \"t\" a=1 b=2\nshot \"s\" use=\"t\" b=3 c=4");
    let s = node(&c, "s");
    assert_eq!(keys(s), ["a", "b", "c"]);
    assert!((num(&c, s, "b") - 3.0).abs() < f64::EPSILON);
    assert_eq!(via_names(&s.prop("a").unwrap().origin.via), ["template t"]);
    assert!(
        s.prop("b").unwrap().origin.via.is_empty(),
        "b is the node's own"
    );
    assert_eq!(s.prop("b").unwrap().origin.at.line(), 2);
}

#[test]
fn templates_nest() {
    let c = ok(
        "template \"base\" a=1 c=5\ntemplate \"mid\" use=\"base\" a=10 b=2\nshot \"s\" use=\"mid\"",
    );
    let s = node(&c, "s");
    assert_eq!(keys(s), ["a", "c", "b"]);
    assert!((num(&c, s, "a") - 10.0).abs() < f64::EPSILON);
    assert_eq!(
        via_names(&s.prop("a").unwrap().origin.via),
        ["template mid"]
    );
    assert_eq!(
        via_names(&s.prop("c").unwrap().origin.via),
        ["template base", "template mid"]
    );
}

#[test]
fn template_children_merge_by_kind_and_name() {
    let c = ok("\
template \"t\" {
    camera \"main\" lens=50 {
        key u=0
    }
    label \"x\"
}
shot \"s\" use=\"t\" {
    camera \"main\" lens=35
    extra
}");
    let s = node(&c, "s");
    let kinds: Vec<&str> = s
        .children()
        .iter()
        .map(videbimus_config::Node::kind)
        .collect();
    assert_eq!(kinds, ["camera", "label", "extra"]);
    let cam = &s.children()[0];
    assert!((num(&c, cam, "lens") - 35.0).abs() < f64::EPSILON);
    assert_eq!(cam.children().len(), 1, "the template's key survives");
}

#[test]
fn template_values_resolve_where_used() {
    let c = ok(&format!(
        "{VIDEO}let tail=(s)0.5\n\
template \"t\" end=(expr)\"$self.to + $tail\" {{\n    marker at=$parent.to\n}}\n\
shot \"s\" use=\"t\" to=(bar)4"
    ));
    let s = node(&c, "s");
    let r = c.resolver();
    let end: videbimus_core::Frame = r.get(s, "end").unwrap();
    assert_eq!(end.0, 277 + 15);
    let at: videbimus_core::Frame = r.get(&s.children()[0], "at").unwrap();
    assert_eq!(at.0, 277);
}

#[test]
fn template_errors() {
    let e = err("template \"a\" use=\"b\"\ntemplate \"b\" use=\"a\"\nshot \"s\" use=\"a\"");
    assert!(
        e.contains("templates use each other in a cycle: a → b → a"),
        "{e}"
    );

    let e = err("template \"close-up\" a=1\nshot \"s\" use=\"closeup\"");
    assert!(e.contains("there is no template \"closeup\""), "{e}");
    assert!(e.contains("did you mean `close-up`?"), "{e}");
    assert!(e.contains("main.kdl:2:10"), "{e}");

    let e = err("shot \"s\" {\n    template \"t\"\n}");
    assert!(e.contains("templates are defined at the top level"), "{e}");

    let e = err("template \"t\"\ntemplate \"t\"");
    assert!(e.contains("template \"t\" is defined twice"), "{e}");
    assert!(e.contains("note: first defined at main.kdl:1:1"), "{e}");

    let e = err("template \"t\" 4");
    assert!(e.contains("has an argument after its name"), "{e}");

    let e = err("template a=1");
    assert!(e.contains("a template needs a name"), "{e}");
}

#[test]
fn includes_splice_relative_to_the_including_file() {
    let c = load(&[
        (
            "video/main.kdl",
            "include \"parts/defs.kdl\"\nshot \"s\" use=\"face\" n=$size {\n    include \"parts/keys.kdl\"\n}",
        ),
        (
            "video/parts/defs.kdl",
            "include \"../common.kdl\"\ntemplate \"face\" w=1920",
        ),
        ("video/common.kdl", "let size=3"),
        ("video/parts/keys.kdl", "key u=0\nkey u=1"),
    ])
    .unwrap_or_else(|e| panic!("{e}"));
    let s = node(&c, "s");
    assert!((num(&c, s, "w") - 1920.0).abs() < f64::EPSILON);
    assert!((num(&c, s, "n") - 3.0).abs() < f64::EPSILON);
    assert_eq!(s.children().len(), 2);
    let k = &s.children()[1];
    assert_eq!(k.origin().at.path().to_str(), Some("video/parts/keys.kdl"));
    assert_eq!(k.origin().at.line(), 2);
    assert_eq!(via_names(&k.origin().via), ["include video/main.kdl"]);
    let w = s.prop("w").unwrap();
    assert_eq!(w.origin.at.path().to_str(), Some("video/parts/defs.kdl"));
}

#[test]
fn include_errors() {
    let e = load(&[
        ("a.kdl", "include \"b.kdl\""),
        ("b.kdl", "include \"a.kdl\""),
    ])
    .unwrap_err()
    .to_string();
    assert!(e.contains("include cycle: a.kdl → b.kdl → a.kdl"), "{e}");
    assert!(e.contains("--> b.kdl:1:1"), "{e}");
    assert!(e.contains("in a file included at a.kdl:1:1"), "{e}");

    let e = load(&[("dir/a.kdl", "include \"missing.kdl\"")])
        .unwrap_err()
        .to_string();
    assert!(e.contains("can't read `dir/missing.kdl`"), "{e}");
    assert!(
        e.contains("relative to the including file's directory (`dir`)"),
        "{e}"
    );

    let e = err("include");
    assert!(e.contains("`include` takes one path, as a string"), "{e}");

    let e = load(&[
        ("a.kdl", "shot \"s\" {\n    include \"t.kdl\"\n}"),
        ("t.kdl", "template \"t\""),
    ])
    .unwrap_err()
    .to_string();
    assert!(e.contains("this file is included inside a node"), "{e}");
}

#[test]
fn each_expands_a_literal_list() {
    let c = ok(&format!(
        "{VIDEO}\
template \"band\" h=(expr)\"1080 / 3\"
stack \"bands\" {{
    each \"synth\" \"vxn1c\" \"vxn2b\" \"kritter\" index=\"i\" {{
        band $synth use=\"band\" y=(expr)\"$i * $self.h\" from=(expr)\"$i * 1bar\"
        gap
    }}
}}"
    ));
    let bands: Vec<_> = node(&c, "bands").children_of("band").collect();
    let names: Vec<_> = bands.iter().map(|b| b.name().unwrap()).collect();
    assert_eq!(names, ["vxn1c", "vxn2b", "kritter"]);
    assert!((num(&c, bands[2], "y") - 720.0).abs() < 1e-9);
    assert_eq!(node(&c, "bands").children().len(), 6);
    assert_eq!(via_names(&bands[1].origin().via), ["each synth=\"vxn2b\""]);
    let from = &bands[1].prop("from").unwrap();
    assert_eq!(
        from.value,
        Value::Time(videbimus_config::Time::new(
            1.0,
            videbimus_config::Unit::Bar
        ))
    );
}

#[test]
fn each_errors() {
    let e = err("let x=1\neach \"a\" $x { n; }");
    assert!(
        e.contains("`each` expands over a literal list; this item is computed"),
        "{e}"
    );
    let e = err("each \"a\" { n; }");
    assert!(e.contains("`each \"a\"` has no items"), "{e}");
    let e = err("each \"a\" 1 2");
    assert!(e.contains("`each \"a\"` has no body"), "{e}");
    let e = err("each 1 2 { n; }");
    assert!(e.contains("`each` starts with the name to bind"), "{e}");
    let e = err("each \"a\" 1 count=2 { n; }");
    assert!(e.contains("`each` has no property `count`"), "{e}");
    let e = err("each \"a\" 1 2 { n v=(expr)\"$a + 1bar\"; }");
    assert!(e.contains("can't add a time and a plain number"), "{e}");
    assert!(
        e.contains("in `each \"a\"` at main.kdl:1:1, where $a = 1"),
        "{e}"
    );
}

#[test]
fn let_errors() {
    let e = err("let 4");
    assert!(
        e.contains("`let` binds named values; this one has no name"),
        "{e}"
    );
    let e = err("let a=1\nlet a=2");
    assert!(e.contains("`a` is bound twice in the same block"), "{e}");
    assert!(e.contains("note: first bound at main.kdl:1:5"), "{e}");
}
