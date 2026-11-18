//! videbimus's component runtime ([ADR 0002]): component trees built from
//! config, driven for random access, and the built-in kinds.
//!
//! - [`registry`]: [`ComponentKind`] and [`ComponentRegistry`], how config
//!   makes components ([ADR 0009] §4), and the [`Builder`] that places
//!   them in their parent's frames and builds children.
//! - [`drive`]: [`Child`], which renders any frame of a component in any
//!   order through [`plan_catch_up`](videbimus_core::plan_catch_up) and
//!   state checkpoints, and [`Scene`], the top of a tree.
//! - [`kinds`]: `sequence`, `stack`, `crossfade`, `filter`,
//!   `frame-player`, `image` and `caption`.
//! - [`color`], [`raster`], [`curve`]: sRGB and PNG, blending and
//!   placement, opacity curves.
//!
//! Images are premultiplied linear RGBA in `f32`
//! ([`Image`](videbimus_core::Image)). PNG input is sRGB and is decoded to
//! linear and premultiplied on load ([`color::read_png`]).
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use videbimus_config::{Config, MemLoader};
//! use videbimus_core::Frame;
//! use videbimus_runtime::{ComponentRegistry, DriveOptions, scene_from_config};
//!
//! let loader = MemLoader::new().with("v.kdl", r#"
//!     video rate=30 width=4 height=2
//!     sequence "cut" to=(f)2 { filter length=(f)2 { fade out=(f)1; stack } }
//! "#);
//! let config = Config::load_with("v.kdl", &loader)?;
//! let root = config.nodes_of("sequence").next().unwrap();
//! let registry = ComponentRegistry::with_builtins();
//! let mut scene = scene_from_config(&config, root, &registry, DriveOptions::default(), 0)?;
//! // the fade's last frame is opaque black
//! assert_eq!(scene.render(Frame(1)).pixels()[0], [0.0, 0.0, 0.0, 1.0]);
//! # Ok(()) }
//! ```
//!
//! [ADR 0002]: ../../../adrs/0002-scene-components.md
//! [ADR 0009]: ../../../adrs/0009-using-videbimus.md

/// The built-in kinds' code identities, from `build.rs`.
mod code {
    include!(concat!(env!("OUT_DIR"), "/videbimus_code.rs"));
}

pub mod color;
pub mod curve;
pub mod drive;
pub mod kinds;
pub mod raster;
pub mod registry;

pub use drive::{Child, DriveOptions, DriveStats, Scene, load_children, save_children};
pub use registry::{
    AssetRead, AssetReads, Builder, CodeIdentity, ComponentKind, ComponentRegistry, Constructor,
    ReadBy,
};

use videbimus_config::{Config, Error, Node};

/// A [`Scene`] of the component `root` (a top-level node of `config`),
/// built with `registry`, at the frame rate and size of the config's
/// `video` node (`video rate=30 width=1920 height=1080`). It keeps the last
/// `window` frames rendered.
///
/// # Errors
///
/// If the config has no `video` node, or it lacks a size, or building
/// `root` fails.
pub fn scene_from_config(
    config: &Config,
    root: &Node,
    registry: &ComponentRegistry,
    options: DriveOptions,
    window: usize,
) -> Result<Scene, Error> {
    scene_from_config_with(config, root, registry.builder(options), window)
}

/// As [`scene_from_config`], building the tree with `builder` (from
/// [`ComponentRegistry::builder_reading`], say, to record the assets it
/// reads).
///
/// # Errors
///
/// As [`scene_from_config`].
pub fn scene_from_config_with(
    config: &Config,
    root: &Node,
    builder: Builder,
    window: usize,
) -> Result<Scene, Error> {
    let r = config.resolver();
    let (Some(video), Some(tb)) = (config.nodes_of("video").next(), config.timebase()) else {
        return Err(Error::new("the config has no `video` node")
            .at(root.origin().clone())
            .element(root.describe())
            .help("add `video rate=30 width=1920 height=1080` at the top"));
    };
    let width: u32 = r.get(video, "width")?;
    let height: u32 = r.get(video, "height")?;
    let component = builder.build(root, r)?;
    Ok(Scene::new(
        component,
        tb.rate,
        width,
        height,
        builder.options(),
        window,
    ))
}
