//! `frame-player`: an image sequence, frame by frame.

use std::path::PathBuf;

use videbimus_config::{Error, Node, Resolver};
use videbimus_core::{AssetKey, Component, Context, Frame, Image, Interval};
use videbimus_store::{ChunkRecord, Store, parse_key};

use super::{PLACEMENT_PICTURE_PROPS, no_children, read_placement};
use crate::color::read_png;
use crate::raster::{Placement, draw, placeholder};
use crate::registry::{AssetRead, Builder, ComponentKind, ReadBy, path_prop};

/// Where a frame player's files come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameSource {
    /// A directory of files.
    Dir(PathBuf),
    /// The committed chunks of a frame-sequence asset in a store: the file
    /// for asset frame `a` is in the chunk whose frames hold `a`.
    Store {
        /// The store's root.
        root: PathBuf,
        /// The asset's key, if given.
        key: Option<AssetKey>,
        /// Otherwise the asset's name: its latest key is used.
        name: Option<String>,
    },
}

/// What a frame player found for a frame.
enum Found {
    File(PathBuf),
    Missing(String),
}

/// Plays a sequence of PNG files (ADR 0002 §5): local frame `n` shows file
/// number `start-index + n`, sRGB decoded to linear and premultiplied,
/// placed in the frame. A frame that is missing or unreadable shows a
/// labelled placeholder instead of failing, so a partly built shot can be
/// reviewed (ADR 0008 §5). Pure: horizon 0.
///
/// ```kdl
/// frame-player "s07" from=(bar)4 to=(bar)8 dir="frames/s07" \
///     pattern="####.png" start-index=1 fit="cover"
/// frame-player "s07" asset="s07/blender" start-index=0
/// frame-player "s08" store="../store" name="s08/blender" pattern="####.png"
/// ```
///
/// - `asset`: a frame-sequence asset the project declares, by name. It is
///   read from the project's store, and building the scene builds it
///   (the frames shown, for a preview). The usual way.
/// - `dir`: a directory of frames, relative to the config file. Not
///   managed: nothing builds it, and editing its files rebuilds nothing.
/// - `store` with `key` (64 hex digits) or `name`: an asset in a given
///   store. A `name` in the project's store that the project declares is
///   built like `asset`; otherwise, and always for a `key`, the asset is
///   read as it is.
/// - `pattern`: the file name, with a run of `#` for the zero-padded
///   frame number (default `"####.png"`).
/// - `start-index`: the file number of local frame 0 (default 0).
/// - `fit`, `scale`, `x`, `y`: placement (default: contained, centred).
#[derive(Debug)]
pub struct FramePlayer {
    interval: Interval,
    source: FrameSource,
    pattern: (String, usize, String),
    start_index: i64,
    placement: Placement,
    store: Option<Result<Store, String>>,
    chunks: Vec<ChunkRecord>,
}

impl FramePlayer {
    /// A player of `source`, with file names from `pattern` (a run of `#`
    /// for the number).
    ///
    /// # Panics
    ///
    /// If `pattern` has no `#`.
    #[must_use]
    pub fn new(
        interval: Interval,
        source: FrameSource,
        pattern: &str,
        start_index: i64,
        placement: Placement,
    ) -> Self {
        Self {
            interval,
            source,
            pattern: split_pattern(pattern).expect("a pattern has a run of #"),
            start_index,
            placement,
            store: None,
            chunks: Vec::new(),
        }
    }

    /// The file name of file number `i`.
    #[must_use]
    pub fn file_name(&self, i: i64) -> String {
        let (pre, width, post) = &self.pattern;
        format!("{pre}{i:0width$}{post}")
    }

    fn find(&mut self, i: i64) -> Found {
        let name = self.file_name(i);
        match &self.source {
            FrameSource::Dir(dir) => Found::File(dir.join(name)),
            FrameSource::Store {
                root,
                key,
                name: asset,
            } => {
                let store = self
                    .store
                    .get_or_insert_with(|| Store::open_read_only(root).map_err(|e| e.to_string()));
                let Ok(store) = store else {
                    return Found::Missing(format!("NO STORE {i}"));
                };
                let key = match (key, asset) {
                    (Some(k), _) => *k,
                    (None, Some(n)) => match store.latest(n) {
                        Ok(Some(k)) => k,
                        _ => return Found::Missing(format!("NOT BUILT {i}")),
                    },
                    (None, None) => return Found::Missing(format!("NO ASSET {i}")),
                };
                let holds =
                    |c: &ChunkRecord| c.id().frames().is_some_and(|iv| iv.contains(Frame(i)));
                if !self.chunks.iter().any(holds) {
                    // chunks only arrive: look again
                    self.chunks = store.committed(key).unwrap_or_default();
                }
                match self.chunks.iter().find(|c| holds(c)) {
                    Some(c) => Found::File(store.chunk_dir(key, c.id()).join(name)),
                    None => Found::Missing(format!("MISSING {i}")),
                }
            }
        }
    }
}

/// Where a frame player's frames come from, recording asset reads with
/// `b`.
fn read_source(
    node: &Node,
    r: &Resolver,
    b: &Builder,
    interval: Interval,
    start_index: i64,
) -> Result<FrameSource, Error> {
    let at = |k: &str| crate::registry::origin_of(node, k);
    let read = |name: &str, by: ReadBy, store: &std::path::Path, key: &str| AssetRead {
        name: name.to_owned(),
        by,
        store: store.to_owned(),
        origin: at(key),
        video: b.absolute(interval),
        first: start_index,
    };
    let asset: Option<String> = r.opt(node, "asset")?;
    let source = match (
        asset,
        path_prop(node, r, "dir")?,
        path_prop(node, r, "store")?,
    ) {
        (Some(asset), dir, store) => {
            if dir.is_some() {
                return Err(r
                    .error(node, &at("dir"), "both `asset` and `dir` are given")
                    .help("play from one: an asset the project declares, or a directory"));
            }
            for k in ["key", "name"] {
                if node.prop(k).is_some() {
                    return Err(r
                        .error(node, &at(k), format!("both `asset` and `{k}` are given"))
                        .help("`asset` names the asset already; drop this"));
                }
            }
            let Some(root) = store.or_else(|| b.store().map(Into::into)) else {
                return Err(r
                    .error(
                        node,
                        &at("asset"),
                        "`asset` reads the project's store, and there is none here",
                    )
                    .help("build the scene with a videbimus binary, or add store=\"…\""));
            };
            b.read_asset(read(&asset, ReadBy::Asset, &root, "asset"));
            FrameSource::Store {
                root,
                key: None,
                name: Some(asset),
            }
        }
        (None, Some(_), Some(_)) => {
            return Err(r
                .error(node, &at("store"), "both `dir` and `store` are given")
                .help("play from one: a directory, or an asset in a store"));
        }
        (None, Some(dir), None) => {
            for k in ["key", "name"] {
                if node.prop(k).is_some() {
                    return Err(r
                        .error(
                            node,
                            &at(k),
                            format!("`{k}` names an asset in a store, but this plays a directory"),
                        )
                        .help("drop it, or replace `dir` with store=…"));
                }
            }
            FrameSource::Dir(dir)
        }
        (None, None, Some(root)) => {
            let key = match r.opt::<String>(node, "key")? {
                None => None,
                Some(s) => Some(parse_key(&s).ok_or_else(|| {
                    r.error(node, &at("key"), format!("`{s}` isn't an asset key"))
                        .help("an asset key is 64 hex digits")
                })?),
            };
            let name: Option<String> = r.opt(node, "name")?;
            if key.is_none() && name.is_none() {
                return Err(r
                    .error(node, node.origin(), "it names a store but no asset in it")
                    .help("add key=\"<64 hex digits>\" or name=\"<asset name>\""));
            }
            if let (None, Some(n)) = (key, &name) {
                b.read_asset(read(n, ReadBy::Name, &root, "name"));
            }
            FrameSource::Store { root, key, name }
        }
        (None, None, None) => {
            return Err(r.error(node, node.origin(), "it has nothing to play").help(
                "add asset=\"<asset name>\", dir=\"frames/…\", or store=… with key=… or name=…",
            ));
        }
    };
    Ok(source)
}

fn split_pattern(p: &str) -> Option<(String, usize, String)> {
    let start = p.find('#')?;
    let width = p[start..].chars().take_while(|&c| c == '#').count();
    Some((p[..start].to_owned(), width, p[start + width..].to_owned()))
}

impl Component for FramePlayer {
    fn interval(&self) -> Interval {
        self.interval
    }

    fn needs(&self, _: &Context) -> Vec<AssetKey> {
        match &self.source {
            FrameSource::Store { key: Some(k), .. } => vec![*k],
            _ => Vec::new(),
        }
    }

    fn reset(&mut self, _: &Context) {}

    fn render(&mut self, n: Frame, _: &Context, out: &mut Image) {
        let i = self.start_index + n.0;
        match self.find(i) {
            Found::File(path) if path.exists() => match read_png(&path) {
                Ok(img) => draw(out, &img, self.placement, 1.0),
                Err(_) => placeholder(out, &format!("UNREADABLE {i}")),
            },
            Found::File(_) => placeholder(out, &format!("MISSING {i}")),
            Found::Missing(label) => placeholder(out, &label),
        }
    }
}

impl ComponentKind for FramePlayer {
    const KIND: &'static str = "frame-player";
    const CODE: &'static str = crate::code::FRAME_PLAYER;

    fn from_config(node: &Node, r: &Resolver, b: &Builder) -> Result<Box<dyn Component>, Error> {
        let mut own = vec![
            "asset",
            "dir",
            "store",
            "key",
            "name",
            "pattern",
            "start-index",
        ];
        own.extend(PLACEMENT_PICTURE_PROPS);
        b.check_props(node, r, &own)?;
        no_children(node, r)?;
        let interval = b.interval(node, r)?;
        let start_index: i64 = r.opt(node, "start-index")?.unwrap_or(0);
        let source = read_source(node, r, b, interval, start_index)?;
        let at = |k: &str| crate::registry::origin_of(node, k);
        let pattern = r
            .opt::<String>(node, "pattern")?
            .unwrap_or_else(|| "####.png".into());
        if split_pattern(&pattern).is_none() {
            return Err(r
                .error(
                    node,
                    &at("pattern"),
                    format!("the pattern \"{pattern}\" has no place for the frame number"),
                )
                .help("mark it with #, one per digit: \"####.png\" is 0001.png"));
        }
        Ok(Box::new(Self::new(
            interval,
            source,
            &pattern,
            start_index,
            read_placement(node, r)?,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        let p = FramePlayer::new(
            Interval::new(Frame(0), Frame(1)),
            FrameSource::Dir(PathBuf::new()),
            "shot_####.png",
            1,
            Placement::default(),
        );
        assert_eq!(p.file_name(7), "shot_0007.png");
        assert_eq!(p.file_name(12345), "shot_12345.png");
        assert!(split_pattern("x.png").is_none());
    }
}
