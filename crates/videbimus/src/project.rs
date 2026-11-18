//! A loaded project: a config checked against a binary's kinds, its
//! asset graph, and its scene ([ADR 0009] §4, §6).
//!
//! # The config's top level
//!
//! ```kdl
//! requires "fen" "song-render"                     // kinds it needs
//! binary "fenland" run="cargo run -p fenland --"   // which binary has them
//! store "../fenland-store"                         // optional
//! video rate=30 width=1920 height=1080 root="main" // root: optional
//!
//! asset "song" recipe="song-render" { … }          // named "song"
//! group "s07" {
//!     asset recipe="blender-shot" {                // named "s07/blender-shot"
//!         input "song" "song"
//!     }
//!     asset "panel" recipe="panel-export"          // named "s07/panel"
//! }
//!
//! sequence "main" {
//!     frame-player asset="s07/blender-shot" length=(bar)4
//! }
//! ```
//!
//! - **`requires`** lists the kinds (component or recipe) the config
//!   needs. A binary that lacks one fails at load, before anything runs,
//!   naming the binary that has it from **`binary`**: its name, and
//!   optionally `run`, the command that runs it.
//! - **`store`**: the asset store's directory, relative to the config
//!   file. By default `.videbimus/store` beside the config file. The
//!   command line's `--store` overrides both.
//! - **The root** is the top-level component that is the video: the one
//!   `video`'s `root` names, or else the only top-level node that isn't
//!   one of `video`, `requires`, `binary`, `store`, `asset` and `group`.
//!   Several candidates and no `root` is an error naming them; none is a
//!   project of assets only.
//! - **`asset`** declares an asset and its recipe kind. Its children
//!   `input "<label>" "<asset name>"` are its inputs: graph edges, each
//!   keyed under its label. Everything else on the node is the recipe
//!   kind's.
//! - **`group "<name>"`** holds assets (and groups), and is part of their
//!   names. A group is the place in the scene its assets belong to, for
//!   the progress tree: name it after the scene (`s07`).
//!
//! # Asset names
//!
//! An asset's name is its stable slot in the store (`s07/blender`), the
//! name its builds are recorded under (ADR 0003 §6). It is the path of
//! groups it is declared in, then its own name: the `asset` node's name
//! argument, or by default its recipe kind, its role. So
//! `group "s07" { asset recipe="blender" }` and
//! `asset "s07/blender" recipe="blender"` both declare `s07/blender`.
//! Names are unique; a second declaration of one is an error.
//!
//! # What the scene reads
//!
//! Components that read assets by name record the reads while the scene
//! is built ([`Builder::read_asset`](videbimus_runtime::Builder::read_asset)):
//! a frame player's `asset="…"`, which must name a declared asset, and its
//! `store=… name=…` when that store is the project's and the name is
//! declared. Building the scene builds exactly those assets and their
//! inputs; a preview builds only the asset frames its video frames show.
//! A frame player's `dir`, or `key`, is read as it is: unmanaged, an
//! escape hatch.
//!
//! [ADR 0009]: ../../../adrs/0009-using-videbimus.md

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use videbimus_config::{Config, Error, Node, Origin, Resolver};
use videbimus_core::Interval;
use videbimus_runtime::registry::did_you_mean;
use videbimus_runtime::{AssetRead, AssetReads, DriveOptions, ReadBy, Scene};
use videbimus_scheduler::{Graph, NodeId, Target};
use videbimus_store::Shape;

use crate::App;
use crate::recipe::{INPUT, RecipeContext};

/// Top-level node kinds that aren't components.
pub const RESERVED: [&str; 6] = ["video", "requires", "binary", "store", "asset", "group"];

/// Where the store is, by default, relative to the config file's
/// directory.
pub const DEFAULT_STORE: &str = ".videbimus/store";

/// A declared asset.
#[derive(Clone, Debug)]
pub struct Asset {
    /// Its name (`s07/blender`).
    pub name: String,
    /// Its recipe kind.
    pub recipe: String,
    /// The group path it is declared in (`s07`), if any.
    pub group: Option<String>,
    /// Its inputs: label and asset name.
    pub inputs: Vec<(String, String)>,
    /// Its node in the graph.
    pub node: NodeId,
    /// Where it is declared.
    pub origin: Origin,
}

/// A config, loaded and checked against a binary's kinds.
pub struct Project {
    config: Config,
    path: PathBuf,
    store: PathBuf,
    assets: Vec<Asset>,
    graph: Graph,
    root: Option<usize>,
    reads: Vec<AssetRead>,
    wired: Vec<(NodeId, AssetRead)>,
}

impl std::fmt::Debug for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Project")
            .field("path", &self.path)
            .field("store", &self.store)
            .field("assets", &self.assets)
            .field("root", &self.root().map(Node::describe))
            .finish_non_exhaustive()
    }
}

impl Project {
    /// Loads the config at `path` for `app`: checks `requires`, declares
    /// the assets and builds their graph, selects the root and builds the
    /// scene once, to learn what it reads. Nothing runs. `store`
    /// overrides the config's store.
    ///
    /// # Errors
    ///
    /// The first problem, located: a config error, a missing kind, a bad
    /// asset declaration or reference, or a scene that won't build.
    pub fn load(app: &App, path: &Path, store: Option<&Path>) -> Result<Self, Error> {
        let config = Config::load(path)?;
        Self::from_config(app, config, path, store)
    }

    /// As [`load`](Self::load), from a config already read.
    ///
    /// # Errors
    ///
    /// As [`load`](Self::load).
    pub fn from_config(
        app: &App,
        config: Config,
        path: &Path,
        store: Option<&Path>,
    ) -> Result<Self, Error> {
        check_requires(&config, app)?;
        let store = store_path(&config, path, store)?;
        let r = config.resolver();
        let decls = declare_assets(&config)?;
        let (graph, assets) = build_graph(&decls, app, r)?;
        let root = select_root(&config)?;
        let mut project = Self {
            path: path.to_owned(),
            store,
            assets,
            graph,
            root,
            reads: Vec::new(),
            wired: Vec::new(),
            config,
        };
        if project.root.is_some() {
            let reads = AssetReads::new(Some(project.store.clone()));
            project.scene_recording(app, &reads)?;
            project.reads = reads.take();
            project.wired = project.wire()?;
        }
        Ok(project)
    }

    /// The resolved config.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The config file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The store's directory.
    #[must_use]
    pub fn store(&self) -> &Path {
        &self.store
    }

    /// The declared assets, in graph order (inputs first).
    #[must_use]
    pub fn assets(&self) -> &[Asset] {
        &self.assets
    }

    /// The asset named `name`.
    #[must_use]
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// The asset graph.
    #[must_use]
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// The root component's node, if the config has one.
    #[must_use]
    pub fn root(&self) -> Option<&Node> {
        self.root.map(|i| &self.config.nodes()[i])
    }

    /// Every asset read the scene records, managed or not.
    #[must_use]
    pub fn reads(&self) -> &[AssetRead] {
        &self.reads
    }

    /// The reads of declared assets in the project's store: the ones
    /// building the scene builds.
    #[must_use]
    pub fn wired(&self) -> &[(NodeId, AssetRead)] {
        &self.wired
    }

    /// A fresh scene of the root, reading assets from the project's
    /// store; `None` if there is no root.
    ///
    /// # Errors
    ///
    /// If the scene won't build.
    pub fn scene(&self, app: &App) -> Result<Option<Scene>, Error> {
        if self.root.is_none() {
            return Ok(None);
        }
        let reads = AssetReads::new(Some(self.store.clone()));
        self.scene_recording(app, &reads).map(Some)
    }

    fn scene_recording(&self, app: &App, reads: &AssetReads) -> Result<Scene, Error> {
        let root = self.root().expect("a root");
        let builder = app
            .components()
            .builder_reading(DriveOptions::default(), reads);
        videbimus_runtime::scene_from_config_with(&self.config, root, builder, 0)
    }

    /// What building the scene means: every asset it reads, whole; or
    /// with `frames`, only the asset frames those video frames show (a
    /// preview). Inputs are built whole by the scheduler.
    #[must_use]
    pub fn scene_targets(&self, frames: Option<Interval>) -> Vec<Target> {
        let mut out: Vec<Target> = Vec::new();
        for (id, read) in &self.wired {
            let target = match (frames, self.graph.task(*id).shape()) {
                (None, _) => Some(Target::all(*id)),
                (Some(f), Shape::Single) => read.video.overlap(f).map(|_| Target::all(*id)),
                (Some(f), Shape::Frames(whole)) => read
                    .asset_frames(f)
                    .and_then(|a| a.overlap(whole))
                    .map(|a| Target::frames(*id, a)),
            };
            if let Some(t) = target
                && !out.contains(&t)
            {
                out.push(t);
            }
        }
        out
    }

    /// Targets for asset names given on the command line: an asset's
    /// name, or a group path for every asset under it.
    ///
    /// # Errors
    ///
    /// A name that is neither, with the nearest asset name.
    pub fn named_targets(&self, names: &[String]) -> Result<Vec<Target>, String> {
        let mut out = Vec::new();
        for n in names {
            let prefix = format!("{}/", n.trim_end_matches('/'));
            let found: Vec<NodeId> = self
                .assets
                .iter()
                .filter(|a| a.name == *n || a.name.starts_with(&prefix))
                .map(|a| a.node)
                .collect();
            if found.is_empty() {
                let known: Vec<&str> = self.assets.iter().map(|a| a.name.as_str()).collect();
                let hint = match did_you_mean(n, &known) {
                    Some(k) => format!("did you mean `{k}`?"),
                    None if known.is_empty() => "the config declares no assets".to_owned(),
                    None => format!("the assets are {}", known.join(", ")),
                };
                return Err(format!("there is no asset or group `{n}`; {hint}"));
            }
            for id in found {
                let t = Target::all(id);
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
        Ok(out)
    }

    /// The reads that name declared assets in the project's store.
    fn wire(&self) -> Result<Vec<(NodeId, AssetRead)>, Error> {
        let by_name: HashMap<&str, NodeId> = self
            .assets
            .iter()
            .map(|a| (a.name.as_str(), a.node))
            .collect();
        let mut out = Vec::new();
        for read in &self.reads {
            let id = by_name.get(read.name.as_str()).copied();
            match (read.by, id) {
                (ReadBy::Asset, Some(id)) => out.push((id, read.clone())),
                (ReadBy::Asset, None) => {
                    let known: Vec<&str> = by_name.keys().copied().collect();
                    let mut known = known;
                    known.sort_unstable();
                    let help = match did_you_mean(&read.name, &known) {
                        Some(k) => format!("did you mean `{k}`?"),
                        None => format!("declare it: asset \"{}\" recipe=\"…\"", read.name),
                    };
                    return Err(Error::new(format!(
                        "there is no asset `{}` in this config",
                        read.name
                    ))
                    .at(read.origin.clone())
                    .element(format!("asset reference `{}`", read.name))
                    .help(help));
                }
                (ReadBy::Name, Some(id)) if same_dir(&read.store, &self.store) => {
                    out.push((id, read.clone()));
                }
                (ReadBy::Name, _) => {}
            }
        }
        Ok(out)
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        std::fs::canonicalize(p)
            .or_else(|_| std::path::absolute(p))
            .unwrap_or_else(|_| p.to_owned())
    };
    norm(a) == norm(b)
}

/// Checks the config's `requires` against `app`'s kinds.
fn check_requires(config: &Config, app: &App) -> Result<(), Error> {
    let r = config.resolver();
    let binary = match config.nodes_of("binary").collect::<Vec<_>>().as_slice() {
        [] => None,
        [b] => {
            r.check_props(b, &["run"])?;
            let name: String = r.arg(b, 0)?;
            let run: Option<String> = r.opt(b, "run")?;
            Some((name, run))
        }
        [_, second, ..] => {
            return Err(r
                .error(
                    second,
                    second.origin(),
                    "a config names one `binary`, and this is a second",
                )
                .help("keep the one whose binary registers every kind in `requires`"));
        }
    };
    let mut missing: Vec<(String, Origin)> = Vec::new();
    for node in config.nodes_of("requires") {
        r.check_props(node, &[])?;
        let kinds: Vec<String> = r.args(node)?;
        for (kind, arg) in kinds.into_iter().zip(node.args()) {
            if !app.components().contains(&kind) && !app.recipes().contains(&kind) {
                missing.push((kind, arg.origin.clone()));
            }
        }
    }
    let Some((first, at)) = missing.first() else {
        return Ok(());
    };
    let names: Vec<String> = missing.iter().map(|(k, _)| format!("`{k}`")).collect();
    let subject = match names.as_slice() {
        [one] => format!("kind {one} isn't registered"),
        [init @ .., last] => format!("kinds {} and {last} aren't registered", init.join(", ")),
        [] => unreachable!(),
    };
    let error = match &binary {
        Some((name, run)) => {
            let how = run
                .as_ref()
                .map(|r| format!(" (`{r}`)"))
                .unwrap_or_default();
            Error::new(format!("{subject}; this config needs the `{name}` binary{how}"))
                .note(format!("this binary is `{}`", app.name()))
        }
        None => Error::new(format!(
            "{subject} in `{}`, and the config names no binary that has {}",
            app.name(),
            if missing.len() == 1 { "it" } else { "them" }
        ))
        .help("add `binary \"<name>\" run=\"cargo run -p <name> --\"` at the top of the config, naming the project binary that registers it"),
    };
    let _ = first;
    Err(error.at(at.clone()).element("requires"))
}

/// The store's directory.
fn store_path(config: &Config, path: &Path, over: Option<&Path>) -> Result<PathBuf, Error> {
    if let Some(p) = over {
        return Ok(p.to_owned());
    }
    let r = config.resolver();
    match config.nodes_of("store").collect::<Vec<_>>().as_slice() {
        [] => Ok(path.parent().unwrap_or(Path::new("")).join(DEFAULT_STORE)),
        [s] => {
            r.check_props(s, &[])?;
            let dir: String = r.arg(s, 0)?;
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                return Ok(dir);
            }
            let written = s.args()[0].origin.at.path();
            Ok(written.parent().unwrap_or(Path::new("")).join(dir))
        }
        [_, second, ..] => Err(r
            .error(
                second,
                second.origin(),
                "a config has one `store`, and this is a second",
            )
            .help("remove one")),
    }
}

/// An `asset` node, before its task is made.
struct Decl<'c> {
    name: String,
    recipe: String,
    group: Option<String>,
    node: &'c Node,
    /// (label, asset name, where the name is written)
    inputs: Vec<(String, String, Origin)>,
}

fn declare_assets(config: &Config) -> Result<Vec<Decl<'_>>, Error> {
    let r = config.resolver();
    let mut out: Vec<Decl<'_>> = Vec::new();
    let mut seen: BTreeMap<String, Origin> = BTreeMap::new();
    for node in config.nodes() {
        match node.kind() {
            "asset" => declare(node, &[], r, &mut out, &mut seen)?,
            "group" => group(node, &[], r, &mut out, &mut seen)?,
            _ => {}
        }
    }
    Ok(out)
}

fn group<'c>(
    node: &'c Node,
    path: &[String],
    r: &Resolver,
    out: &mut Vec<Decl<'c>>,
    seen: &mut BTreeMap<String, Origin>,
) -> Result<(), Error> {
    r.check_props(node, &[])?;
    let name: String = r.arg(node, 0)?;
    check_segments(&name, node, &node.args()[0].origin, r)?;
    let mut path = path.to_vec();
    path.push(name);
    for c in node.children() {
        match c.kind() {
            "asset" => declare(c, &path, r, out, seen)?,
            "group" => group(c, &path, r, out, seen)?,
            other => {
                return Err(r
                    .error(
                        c,
                        c.origin(),
                        format!("a group holds assets and groups, not `{other}`"),
                    )
                    .help("move it out of the group"));
            }
        }
    }
    Ok(())
}

fn check_segments(name: &str, node: &Node, at: &Origin, r: &Resolver) -> Result<(), Error> {
    if name.is_empty() || name.split('/').any(str::is_empty) {
        return Err(r
            .error(node, at, format!("`{name}` isn't an asset name"))
            .help("an asset name is words separated by single `/`s: \"s07/blender\""));
    }
    Ok(())
}

fn declare<'c>(
    node: &'c Node,
    path: &[String],
    r: &Resolver,
    out: &mut Vec<Decl<'c>>,
    seen: &mut BTreeMap<String, Origin>,
) -> Result<(), Error> {
    let Some(recipe_v) = node.prop("recipe") else {
        return Err(r
            .error(node, node.origin(), "an asset needs a recipe kind")
            .help("add recipe=\"<kind>\": the kind that builds it"));
    };
    let recipe: String = r.get(node, "recipe")?;
    let own = if let Some(a) = node.args().first() {
        let n: String = r.arg(node, 0)?;
        check_segments(&n, node, &a.origin, r)?;
        n
    } else {
        check_segments(&recipe, node, &recipe_v.origin, r)?;
        recipe.clone()
    };
    let group = (!path.is_empty()).then(|| path.join("/"));
    let name = match &group {
        Some(g) => format!("{g}/{own}"),
        None => own,
    };
    if let Some(first) = seen.get(&name) {
        return Err(r
            .error(
                node,
                node.origin(),
                format!("a second asset is named `{name}`"),
            )
            .note(format!("the first is at {}", first.at))
            .help(
                "asset names are store slots and must be unique: name it with asset \"<name>\"",
            ));
    }
    seen.insert(name.clone(), node.origin().clone());
    let mut inputs: Vec<(String, String, Origin)> = Vec::new();
    for c in node.children_of(INPUT) {
        r.check_props(c, &[])?;
        if c.args().len() != 2 {
            return Err(r
                .error(c, c.origin(), "an input has a label and an asset name")
                .help("input \"<label>\" \"<asset name>\""));
        }
        let label: String = r.arg(c, 0)?;
        let asset: String = r.arg(c, 1)?;
        if inputs.iter().any(|(l, _, _)| *l == label) {
            return Err(r
                .error(
                    c,
                    &c.args()[0].origin,
                    format!("two inputs are labelled `{label}`"),
                )
                .help("give each input its own label: its role in the recipe"));
        }
        inputs.push((label, asset, c.args()[1].origin.clone()));
    }
    out.push(Decl {
        name,
        recipe,
        group,
        node,
        inputs,
    });
    Ok(())
}

/// The graph, adding assets inputs first.
fn build_graph(decls: &[Decl<'_>], app: &App, r: &Resolver) -> Result<(Graph, Vec<Asset>), Error> {
    let index: HashMap<&str, usize> = decls
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name.as_str(), i))
        .collect();
    // inputs exist
    for d in decls {
        for (_, asset, at) in &d.inputs {
            if !index.contains_key(asset.as_str()) {
                let mut known: Vec<&str> = index.keys().copied().collect();
                known.sort_unstable();
                let help = match did_you_mean(asset, &known) {
                    Some(k) => format!("did you mean `{k}`?"),
                    None => format!("declare it: asset \"{asset}\" recipe=\"…\""),
                };
                return Err(r
                    .error(
                        d.node,
                        at,
                        format!("there is no asset `{asset}` in this config"),
                    )
                    .help(help));
            }
        }
    }
    // depth-first, inputs before dependants
    let mut order: Vec<usize> = Vec::new();
    let mut state = vec![0u8; decls.len()]; // 0 new, 1 on the stack, 2 done
    for i in 0..decls.len() {
        visit(i, decls, &index, &mut state, &mut Vec::new(), &mut order, r)?;
    }
    let mut graph = Graph::new();
    let mut ids: HashMap<&str, NodeId> = HashMap::new();
    let mut assets = Vec::new();
    for i in order {
        let d = &decls[i];
        let cx = RecipeContext {
            kind: d.recipe.clone(),
            name: d.name.clone(),
            group: d.group.clone(),
            inputs: d
                .inputs
                .iter()
                .map(|(l, a, _)| (l.clone(), a.clone()))
                .collect(),
            code: videbimus_runtime::CodeIdentity::default(),
        };
        let task = app.recipes().build(&d.recipe, d.node, r, cx)?;
        let mut node = videbimus_scheduler::Node::from_arc(Arc::from(task));
        for (label, asset, _) in &d.inputs {
            node = node.input(label, ids[asset.as_str()]);
        }
        if let Some(g) = &d.group {
            node = node.group(g);
        }
        let id = graph
            .add(node)
            .map_err(|e| r.error(d.node, d.node.origin(), e.to_string()))?;
        ids.insert(&d.name, id);
        assets.push(Asset {
            name: d.name.clone(),
            recipe: d.recipe.clone(),
            group: d.group.clone(),
            inputs: d
                .inputs
                .iter()
                .map(|(l, a, _)| (l.clone(), a.clone()))
                .collect(),
            node: id,
            origin: d.node.origin().clone(),
        });
    }
    Ok((graph, assets))
}

/// Visits asset `i`'s inputs, then `i`, pushing onto `order`.
fn visit(
    i: usize,
    decls: &[Decl<'_>],
    index: &HashMap<&str, usize>,
    state: &mut [u8],
    stack: &mut Vec<usize>,
    order: &mut Vec<usize>,
    r: &Resolver,
) -> Result<(), Error> {
    match state[i] {
        2 => return Ok(()),
        1 => {
            let from = stack.iter().position(|&s| s == i).unwrap_or(0);
            let mut chain: Vec<&str> = stack[from..]
                .iter()
                .map(|&s| decls[s].name.as_str())
                .collect();
            chain.push(&decls[i].name);
            let d = &decls[stack[stack.len() - 1]];
            return Err(r
                .error(d.node, d.node.origin(), "the assets' inputs form a cycle")
                .note(format!("cycle: {}", chain.join(" → ")))
                .help("an asset can't be built from itself; remove one of these inputs"));
        }
        _ => {}
    }
    state[i] = 1;
    stack.push(i);
    for (_, asset, _) in &decls[i].inputs {
        visit(index[asset.as_str()], decls, index, state, stack, order, r)?;
    }
    stack.pop();
    state[i] = 2;
    order.push(i);
    Ok(())
}

/// The root: `video`'s `root`, or the only candidate.
fn select_root(config: &Config) -> Result<Option<usize>, Error> {
    let r = config.resolver();
    let candidates: Vec<usize> = config
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, n)| !RESERVED.contains(&n.kind()))
        .map(|(i, _)| i)
        .collect();
    let describe = |i: &usize| format!("`{}`", config.nodes()[*i].describe());
    let video = config.nodes_of("video").next();
    if let Some(video) = video
        && let Some(v) = video.prop("root")
    {
        let name: String = r.get(video, "root")?;
        if let Some(&i) = candidates
            .iter()
            .find(|&&i| config.nodes()[i].name() == Some(name.as_str()))
        {
            return Ok(Some(i));
        }
        let names: Vec<&str> = candidates
            .iter()
            .filter_map(|&i| config.nodes()[i].name())
            .collect();
        let help = match did_you_mean(&name, &names) {
            Some(k) => format!("did you mean `{k}`?"),
            None if names.is_empty() => {
                "name a top-level component: sequence \"main\" { … }".to_owned()
            }
            None => format!("the named top-level components are {}", names.join(", ")),
        };
        return Err(r
            .error(
                video,
                &v.origin,
                format!("there is no top-level component named `{name}`"),
            )
            .help(help));
    }
    match candidates.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        [_, second, ..] => {
            let n = &config.nodes()[*second];
            let all: Vec<String> = candidates.iter().map(describe).collect();
            Err(Error::new(format!(
                "the config has {} top-level components, and nothing says which is the video",
                candidates.len()
            ))
            .at(n.origin().clone())
            .element(n.describe())
            .note(format!("they are {}", all.join(", ")))
            .help("add root=\"<name>\" to `video`, naming the one that is the video"))
        }
    }
}
