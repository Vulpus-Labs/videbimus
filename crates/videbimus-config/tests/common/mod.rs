//! Helpers shared by the integration tests.

#![allow(dead_code)]

use videbimus_config::{Config, Error, MemLoader, Node};

/// The video header most tests share: Fenland's 104 BPM 4/4 at 30 fps.
pub const VIDEO: &str = "video rate=30 { tempo bpm=104 beats-per-bar=4; }\n";

/// Resolves `files`, the first being the root.
pub fn load(files: &[(&str, &str)]) -> Result<Config, Error> {
    let loader = files
        .iter()
        .fold(MemLoader::new(), |l, (path, text)| l.with(path, *text));
    Config::load_with(files[0].0, &loader)
}

/// Resolves `text` as `main.kdl`, which must succeed.
pub fn ok(text: &str) -> Config {
    load(&[("main.kdl", text)]).unwrap_or_else(|e| panic!("{e}"))
}

/// Resolves `text` as `main.kdl`, which must fail; the error as displayed.
pub fn err(text: &str) -> String {
    match load(&[("main.kdl", text)]) {
        Ok(_) => panic!("expected an error"),
        Err(e) => e.to_string(),
    }
}

/// The top-level node named `name`.
pub fn node<'a>(config: &'a Config, name: &str) -> &'a Node {
    config
        .nodes()
        .iter()
        .find(|n| n.name() == Some(name))
        .unwrap_or_else(|| panic!("no node {name:?}"))
}

/// Property `key` of `node`, as a number.
pub fn num(config: &Config, node: &Node, key: &str) -> f64 {
    config
        .resolver()
        .get(node, key)
        .unwrap_or_else(|e| panic!("{e}"))
}
