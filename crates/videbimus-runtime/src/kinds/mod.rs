//! The built-in component kinds (ADR 0002 §5).
//!
//! | kind | type | what |
//! |---|---|---|
//! | `sequence` | [`Sequence`] | children one after another |
//! | `stack` | [`Stack`] | children layered, with opacity curves and blend modes |
//! | `crossfade` | [`Crossfade`] | a stack of two overlapping children |
//! | `filter` | [`Filter`] | one child, post-processed: fade, vignette, grain |
//! | `frame-player` | [`FramePlayer`] | an image sequence, frame by frame |
//! | `image` | [`Picture`] | a still image |
//! | `caption` | [`Caption`] | lines of text |
//!
//! Every component draws onto the transparent image its parent hands it.

mod caption;
mod filter;
mod frame_player;
mod picture;
mod sequence;
mod stack;

pub use caption::{Align, Caption};
pub use filter::{Effect, Filter};
pub use frame_player::{FramePlayer, FrameSource};
pub use picture::Picture;
pub use sequence::Sequence;
pub use stack::{Crossfade, Layer, STACK_PROPS, Stack};

use videbimus_config::{Error, Node, Resolver};

use crate::curve::read_name;
use crate::raster::{Fit, Placement};

/// The properties [`read_placement`] reads.
pub const PLACEMENT_PICTURE_PROPS: [&str; 4] = ["fit", "scale", "x", "y"];

/// A picture's placement in the frame: `fit` (`contain`, `cover`, `fill`
/// or `native`), `scale`, and its centre `x` and `y` as fractions of the
/// frame.
///
/// # Errors
///
/// If a property has the wrong type or an unknown name.
#[allow(clippy::cast_possible_truncation)]
pub fn read_placement(node: &Node, r: &Resolver) -> Result<Placement, Error> {
    let d = Placement::default();
    let fit = read_name(node, r, "fit", &Fit::NAMES)?
        .and_then(Fit::from_name)
        .unwrap_or(d.fit);
    let num = |key: &str, default: f32| -> Result<f32, Error> {
        Ok(r.opt::<f64>(node, key)?.map_or(default, |v| v as f32))
    };
    let scale = num("scale", d.scale)?;
    if scale <= 0.0 {
        return Err(r
            .error(
                node,
                &crate::registry::origin_of(node, "scale"),
                "`scale` must be more than 0",
            )
            .help("scale=1 is the fitted size"));
    }
    Ok(Placement {
        fit,
        scale,
        x: num("x", d.x)?,
        y: num("y", d.y)?,
    })
}

/// Fails if `node` has any children: for leaf kinds.
///
/// # Errors
///
/// At the first child.
pub fn no_children(node: &Node, r: &Resolver) -> Result<(), Error> {
    match node.children().first() {
        None => Ok(()),
        Some(c) => Err(r
            .error(c, c.origin(), format!("{} has no children", node.kind()))
            .help("move it out, or wrap both in a stack")),
    }
}
