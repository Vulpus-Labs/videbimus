//! `stack` and `crossfade`: children layered.

use videbimus_config::{Error, Node, Resolver};
use videbimus_core::{AssetKey, Component, Context, Frame, Image, Interval};

use crate::curve::{Envelope, read_ease, read_name};
use crate::drive::{Child, load_children, save_children};
use crate::raster::{Blend, composite};
use crate::registry::{Builder, ComponentKind};

/// The properties a stack reads from each child's node.
pub const STACK_PROPS: [&str; 6] = ["opacity", "fade-in", "fade-out", "ease", "blend", "z"];

/// One layer of a [`Stack`].
#[derive(Debug)]
pub struct Layer {
    /// The child.
    pub child: Child,
    /// Its opacity over its own frames.
    pub opacity: Envelope,
    /// How it combines with the layers under it.
    pub blend: Blend,
}

/// Children layered bottom to top (ADR 0002 §5), each over its own
/// interval with an opacity curve and a blend mode.
///
/// ```kdl
/// stack from=(s)0 to=(s)20 {
///     frame-player "shot" dir="frames/shot"                 // bottom
///     image "logo" path="logo.png" fit="native" blend="add" opacity=0.8
///     caption "title" from=(s)2 to=(s)6 fade-in=(s)0.5 fade-out=(s)0.5 \
///         text="Fenland" font="fonts/Jost.ttf" size=64
/// }
/// ```
///
/// Each child may carry:
///
/// - `opacity`: a constant from 0 to 1 (default 1);
/// - `fade-in`, `fade-out`: ramps at the start and end of its interval;
/// - `ease`: `linear` (default) or `smooth`, the ramps' shape;
/// - `blend`: `normal` (default), `add` or `multiply`;
/// - `z`: its order, lowest at the bottom; ties keep config order (all 0
///   by default, so the first child is at the bottom).
///
/// A layer whose opacity at a frame is 0 isn't rendered, and so isn't
/// ticked: catch-up covers the gap when it shows again.
#[derive(Debug)]
pub struct Stack {
    interval: Interval,
    layers: Vec<Layer>,
    scratch: Image,
}

impl Stack {
    /// A stack of `layers`, bottom first, whose intervals are in its
    /// frames.
    #[must_use]
    pub fn new(interval: Interval, layers: Vec<Layer>) -> Self {
        Self {
            interval,
            layers,
            scratch: Image::new(0, 0),
        }
    }

    /// Its layers, bottom first.
    #[must_use]
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }
}

impl Component for Stack {
    fn interval(&self) -> Interval {
        self.interval
    }

    fn needs(&self, ctx: &Context) -> Vec<AssetKey> {
        self.layers
            .iter()
            .flat_map(|l| l.child.needs(ctx))
            .collect()
    }

    fn reset(&mut self, _: &Context) {}

    fn render(&mut self, n: Frame, ctx: &Context, out: &mut Image) {
        for layer in &mut self.layers {
            let iv = layer.child.interval();
            if !iv.contains(n) {
                continue;
            }
            let opacity = layer.opacity.at(iv.local(n), iv.len());
            if opacity <= 0.0 {
                continue;
            }
            if (self.scratch.width(), self.scratch.height()) == (out.width(), out.height()) {
                self.scratch.fill([0.0; 4]);
            } else {
                self.scratch = Image::new(out.width(), out.height());
            }
            layer.child.render(n, ctx, &mut self.scratch);
            composite(out, &self.scratch, opacity, layer.blend);
        }
    }

    fn save_state(&self) -> Option<Vec<u8>> {
        save_children(self.layers.iter().map(|l| &l.child))
    }

    fn load_state(&mut self, bytes: &[u8]) -> bool {
        load_children(self.layers.iter_mut().map(|l| &mut l.child), bytes)
    }
}

impl ComponentKind for Stack {
    const KIND: &'static str = "stack";
    const CODE: &'static str = crate::code::STACK;

    fn from_config(node: &Node, r: &Resolver, b: &Builder) -> Result<Box<dyn Component>, Error> {
        b.check_props(node, r, &[])?;
        let interval = b.interval(node, r)?;
        let inner = b.inside(interval).with_parent_props(&STACK_PROPS);
        let mut layers = Vec::new();
        for c in node.children() {
            let child = inner.build_child(c, r)?;
            let opacity = Envelope::read(c, r, Some("opacity"), "fade-in", "fade-out")?;
            let blend = read_name(c, r, "blend", &Blend::NAMES)?
                .and_then(Blend::from_name)
                .unwrap_or_default();
            let z = r.opt::<f64>(c, "z")?.unwrap_or(0.0);
            layers.push((
                z,
                Layer {
                    child,
                    opacity,
                    blend,
                },
            ));
        }
        layers.sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(Box::new(Self::new(
            interval,
            layers.into_iter().map(|(_, l)| l).collect(),
        )))
    }
}

/// A crossfade: sugar for a [`Stack`] of two children whose intervals
/// overlap (ADR 0002 §5). The second fades in over the first across the
/// overlap, reaching half at its middle.
///
/// ```kdl
/// crossfade overlap=(s)1 ease="smooth" {
///     frame-player "s01" from=(bar)0 to=(bar)4 dir="frames/s01"
///     frame-player "s02" to=(bar)8 dir="frames/s02"   // from bar 4 − 1 s
/// }
/// ```
///
/// - `overlap`: how long the two overlap. The second child's `from`
///   defaults to the first's end less this. If the second gives its own
///   `from`, the overlap is wherever their intervals meet, and `overlap`
///   may be left out.
/// - `ease`: the fade's shape, `linear` (default) or `smooth`.
///
/// The first child's `from` defaults to the crossfade's start; the second
/// child's end to the crossfade's end. The second must start after the
/// first and end after it. With opaque children, the frame at the middle
/// of the overlap is an even mix.
#[derive(Debug)]
pub struct Crossfade;

impl ComponentKind for Crossfade {
    const KIND: &'static str = "crossfade";
    const CODE: &'static str = crate::code::CROSSFADE;

    fn from_config(node: &Node, r: &Resolver, b: &Builder) -> Result<Box<dyn Component>, Error> {
        b.check_props(node, r, &["overlap", "ease"])?;
        let interval = b.interval(node, r)?;
        let ease = read_ease(node, r)?;
        let overlap: Option<Frame> = r.opt(node, "overlap")?;
        let inner = b.inside(interval);
        let [first, second] = node.children() else {
            return Err(r
                .error(
                    node,
                    node.origin(),
                    format!(
                        "a crossfade has two children, the outgoing and the incoming; this has {}",
                        node.children().len()
                    ),
                )
                .help("for more layers, use a stack"));
        };
        let a = inner.build_child(first, r)?;
        let b_from = match (overlap, second.prop("from")) {
            (_, Some(_)) => None,
            (Some(o), None) => Some(a.interval().end - o.0),
            (None, None) => {
                return Err(r
                    .error(
                        node,
                        node.origin(),
                        "the crossfade doesn't say how long the overlap is",
                    )
                    .help("add overlap=… (a duration), or give the second child a `from`"));
            }
        };
        let b2 = match b_from {
            Some(f) => inner.offering(f, None).build_child(second, r)?,
            None => inner.build_child(second, r)?,
        };
        let (ai, bi) = (a.interval(), b2.interval());
        let Some(lap) = ai
            .overlap(bi)
            .filter(|_| bi.start > ai.start && bi.end >= ai.end)
        else {
            return Err(r
                .error(
                    second,
                    second.origin(),
                    format!(
                        "the second child ({}–{}) doesn't overlap the end of the first ({}–{})",
                        bi.start, bi.end, ai.start, ai.end
                    ),
                )
                .help("start the second before the first ends, and end it after"));
        };
        let fade_in = Envelope {
            level: 1.0,
            ramp_in: lap.len(),
            ramp_out: 0,
            ease,
        };
        let layers = vec![
            Layer {
                child: a,
                opacity: Envelope::constant(1.0),
                blend: Blend::Normal,
            },
            Layer {
                child: b2,
                opacity: fade_in,
                blend: Blend::Normal,
            },
        ];
        Ok(Box::new(Stack::new(interval, layers)))
    }
}
