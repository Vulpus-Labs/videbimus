//! `image`: a still image.

use std::path::PathBuf;

use videbimus_config::{Error, Node, Resolver};
use videbimus_core::{Component, Context, Frame, Image, Interval};

use super::{PLACEMENT_PICTURE_PROPS, no_children, read_placement};
use crate::color::read_png;
use crate::raster::{Placement, draw, placeholder};
use crate::registry::{Builder, ComponentKind, path_prop};

/// A still PNG, placed in the frame (ADR 0002 §5). Read on first render,
/// sRGB decoded to linear and premultiplied. Pure: horizon 0.
///
/// ```kdl
/// image "logo" path="art/logo.png" fit="native" x=0.9 y=0.1 scale=0.5
/// ```
///
/// - `path`: the file, relative to the config file; it must exist.
/// - `fit` (`contain`, `cover`, `fill`, `native`), `scale`, and the centre
///   `x`, `y` as fractions of the frame.
///
/// If the file can't be decoded when it is drawn, a labelled placeholder
/// shows instead.
#[derive(Debug)]
pub struct Picture {
    interval: Interval,
    path: PathBuf,
    placement: Placement,
    image: Option<Result<Image, String>>,
}

impl Picture {
    /// The PNG at `path`.
    #[must_use]
    pub fn new(interval: Interval, path: PathBuf, placement: Placement) -> Self {
        Self {
            interval,
            path,
            placement,
            image: None,
        }
    }
}

impl Component for Picture {
    fn interval(&self) -> Interval {
        self.interval
    }

    fn reset(&mut self, _: &Context) {}

    fn render(&mut self, _: Frame, _: &Context, out: &mut Image) {
        let path = &self.path;
        match self.image.get_or_insert_with(|| read_png(path)) {
            Ok(img) => draw(out, img, self.placement, 1.0),
            Err(_) => placeholder(out, "UNREADABLE IMAGE"),
        }
    }
}

impl ComponentKind for Picture {
    const KIND: &'static str = "image";

    fn from_config(node: &Node, r: &Resolver, b: &Builder) -> Result<Box<dyn Component>, Error> {
        let mut own = vec!["path"];
        own.extend(PLACEMENT_PICTURE_PROPS);
        b.check_props(node, r, &own)?;
        no_children(node, r)?;
        let interval = b.interval(node, r)?;
        let Some(path) = path_prop(node, r, "path")? else {
            return Err(r
                .error(node, node.origin(), "the image has no `path`")
                .help("add path=\"…png\", relative to this config file"));
        };
        if !path.is_file() {
            return Err(r
                .error(
                    node,
                    &crate::registry::origin_of(node, "path"),
                    format!("there is no file at {}", path.display()),
                )
                .help("paths are relative to the config file they are written in"));
        }
        Ok(Box::new(Self::new(
            interval,
            path,
            read_placement(node, r)?,
        )))
    }
}
