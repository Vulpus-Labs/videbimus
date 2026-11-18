//! `filter`: one child, post-processed.

use videbimus_config::{Error, Node, Resolver};
use videbimus_core::{AssetKey, Component, Context, Frame, Image, Interval};

use crate::color::parse_hex;
use crate::curve::Envelope;
use crate::drive::Child;
use crate::registry::{Builder, ComponentKind};

/// One post-processing step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    /// Towards `color` (premultiplied linear) by `1 − gain`, where the
    /// gain follows `envelope`: a fade in from, or out to, a colour.
    Fade {
        /// The gain over the filter's frames.
        envelope: Envelope,
        /// The colour faded to.
        color: [f32; 4],
    },
    /// Darkens towards the corners.
    Vignette {
        /// How dark the corners get, 0 to 1.
        strength: f32,
        /// Where darkening starts: 0 at the centre, 1 at the corners.
        radius: f32,
        /// How far it takes to reach full strength, in the same units.
        softness: f32,
    },
    /// Film grain: each pixel's light moves by up to ±`amount`, by a hash
    /// of (seed, frame, pixel), never a running random generator (ADR 0002
    /// §7), so any frame's grain is the same however it was reached.
    Grain {
        /// The largest change, in linear light.
        amount: f32,
        /// Keys the hash.
        seed: u64,
    },
}

impl Effect {
    /// The effects' node names in config.
    pub const NAMES: [&'static str; 3] = ["fade", "vignette", "grain"];

    /// Applies it to `img`, which is local frame `k` of a filter `len`
    /// frames long, at absolute frame `abs`.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::many_single_char_names
    )]
    pub fn apply(&self, img: &mut Image, k: Frame, len: i64, abs: Frame) {
        let (w, h) = (img.width() as usize, img.height() as usize);
        match *self {
            Effect::Fade { envelope, color } => {
                let g = envelope.at(k, len);
                if g >= 1.0 {
                    return;
                }
                for p in img.pixels_mut() {
                    *p = std::array::from_fn(|c| p[c] * g + color[c] * (1.0 - g));
                }
            }
            Effect::Vignette {
                strength,
                radius,
                softness,
            } => {
                let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
                for y in 0..h {
                    for x in 0..w {
                        let dx = (x as f32 + 0.5 - cx) / cx;
                        let dy = (y as f32 + 0.5 - cy) / cy;
                        let d = (dx * dx + dy * dy).sqrt() / std::f32::consts::SQRT_2;
                        let t = if softness > 0.0 {
                            ((d - radius) / softness).clamp(0.0, 1.0)
                        } else {
                            f32::from(u8::from(d >= radius))
                        };
                        let m = 1.0 - strength * t * t * (3.0 - 2.0 * t);
                        let p = &mut img.pixels_mut()[y * w + x];
                        for c in &mut p[..3] {
                            *c *= m;
                        }
                    }
                }
            }
            Effect::Grain { amount, seed } => {
                let frame = abs.0 as u64;
                for y in 0..h {
                    for x in 0..w {
                        let u = unit(hash(&[seed, frame, x as u64, y as u64]));
                        let g = (u * 2.0 - 1.0) * amount;
                        let p = &mut img.pixels_mut()[y * w + x];
                        let a = p[3];
                        for c in &mut p[..3] {
                            *c = (*c + g * a).max(0.0);
                        }
                    }
                }
            }
        }
    }

    /// Reads the effect `node` (one of [`NAMES`](Self::NAMES)).
    ///
    /// # Errors
    ///
    /// On unknown or ill-typed properties, or values out of range.
    #[allow(clippy::cast_possible_truncation)]
    pub fn from_config(node: &Node, r: &Resolver) -> Result<Self, Error> {
        let num = |key: &str, default: f64, range: (f64, f64)| -> Result<f32, Error> {
            let v = r.opt::<f64>(node, key)?.unwrap_or(default);
            if v < range.0 || v > range.1 {
                return Err(r
                    .error(
                        node,
                        &crate::registry::origin_of(node, key),
                        format!("`{key}` is {v}, outside {} to {}", range.0, range.1),
                    )
                    .help(format!("{key}={default} is the default")));
            }
            Ok(v as f32)
        };
        match node.kind() {
            "fade" => {
                r.check_props(node, &["in", "out", "ease", "color"])?;
                let envelope = Envelope::read(node, r, None, "in", "out")?;
                let color = match r.opt::<String>(node, "color")? {
                    None => [0.0, 0.0, 0.0, 1.0],
                    Some(s) => parse_hex(&s).ok_or_else(|| {
                        r.error(
                            node,
                            &crate::registry::origin_of(node, "color"),
                            format!("`{s}` isn't a colour"),
                        )
                        .help("write it as \"#rrggbb\" or \"#rrggbbaa\", in sRGB")
                    })?,
                };
                if envelope.ramp_in == 0 && envelope.ramp_out == 0 {
                    return Err(r
                        .error(node, node.origin(), "the fade has neither `in` nor `out`")
                        .help("add in=(s)1 to fade in from black, out=(s)1 to fade out to it"));
                }
                Ok(Effect::Fade { envelope, color })
            }
            "vignette" => {
                r.check_props(node, &["strength", "radius", "softness"])?;
                Ok(Effect::Vignette {
                    strength: num("strength", 0.5, (0.0, 1.0))?,
                    radius: num("radius", 0.5, (0.0, 2.0))?,
                    softness: num("softness", 0.5, (0.0, 2.0))?,
                })
            }
            "grain" => {
                r.check_props(node, &["amount", "seed"])?;
                let seed: i64 = r.opt(node, "seed")?.unwrap_or(0);
                Ok(Effect::Grain {
                    amount: num("amount", 0.03, (0.0, 1.0))?,
                    seed: seed.cast_unsigned(),
                })
            }
            other => unreachable!("not an effect: {other}"),
        }
    }
}

/// splitmix64 over the words, chained.
fn hash(words: &[u64]) -> u64 {
    let mut h: u64 = 0x9e37_79b9_7f4a_7c15;
    for &w in words {
        h ^= w;
        h = h.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = h;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        h = z ^ (z >> 31);
    }
    h
}

/// The top 24 bits as a number in `0..1`.
#[allow(clippy::cast_precision_loss)]
fn unit(h: u64) -> f32 {
    (h >> 40) as f32 / (1u64 << 24) as f32
}

/// One child, post-processed by effects in order (ADR 0002 §5).
///
/// ```kdl
/// filter from=(s)0 to=(s)30 {
///     grain amount=0.03 seed=7
///     vignette strength=0.4 radius=0.5 softness=0.6
///     fade in=(s)1 out=(s)2 ease="smooth"      // from and to black
///     sequence { … }                           // the one child
/// }
/// ```
///
/// Effects, applied in the order written:
///
/// - `fade in=… out=… ease=… color=…`: from `color` (default black,
///   `"#000000"`) over `in` at the start, and to it over `out` at the end.
///   The first frame of a fade in and the last of a fade out are exactly
///   the colour.
/// - `vignette strength=0.5 radius=0.5 softness=0.5`: darkens towards the
///   corners; `radius` is where it starts (0 the centre, 1 the corners).
/// - `grain amount=0.03 seed=0`: keyed grain, the same for a frame however
///   it is reached.
///
/// The child's span defaults to the filter's.
#[derive(Debug)]
pub struct Filter {
    interval: Interval,
    child: Child,
    effects: Vec<Effect>,
}

impl Filter {
    /// `child` (interval in the filter's frames) through `effects`.
    #[must_use]
    pub fn new(interval: Interval, child: Child, effects: Vec<Effect>) -> Self {
        Self {
            interval,
            child,
            effects,
        }
    }
}

impl Component for Filter {
    fn interval(&self) -> Interval {
        self.interval
    }

    fn needs(&self, ctx: &Context) -> Vec<AssetKey> {
        self.child.needs(ctx)
    }

    fn reset(&mut self, _: &Context) {}

    fn render(&mut self, n: Frame, ctx: &Context, out: &mut Image) {
        self.child.render(n, ctx, out);
        for e in &self.effects {
            e.apply(out, n, self.interval.len(), ctx.absolute(n));
        }
    }
}

impl ComponentKind for Filter {
    const KIND: &'static str = "filter";

    fn from_config(node: &Node, r: &Resolver, b: &Builder) -> Result<Box<dyn Component>, Error> {
        b.check_props(node, r, &[])?;
        let interval = b.interval(node, r)?;
        let inner = b.inside(interval);
        let mut effects = Vec::new();
        let mut child: Option<(Child, &Node)> = None;
        for c in node.children() {
            if Effect::NAMES.contains(&c.kind()) {
                effects.push(Effect::from_config(c, r)?);
                continue;
            }
            if let Some((_, first)) = &child {
                return Err(r
                    .error(
                        c,
                        c.origin(),
                        "a filter wraps one child, and this is a second",
                    )
                    .note(format!(
                        "the first is {} at {}",
                        first.describe(),
                        first.origin()
                    ))
                    .help("wrap them in a stack or a sequence, and filter that"));
            }
            child = Some((inner.build_child(c, r)?, c));
        }
        let Some((child, _)) = child else {
            return Err(r
                .error(node, node.origin(), "the filter has nothing to filter")
                .help(format!(
                    "add one component inside it, after its effects ({})",
                    Effect::NAMES.join(", ")
                )));
        };
        Ok(Box::new(Self::new(interval, child, effects)))
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn grain_is_keyed() {
        let g = Effect::Grain {
            amount: 0.1,
            seed: 3,
        };
        let frame = |f: i64| {
            let mut img = Image::new(8, 8);
            img.fill([0.5, 0.5, 0.5, 1.0]);
            g.apply(&mut img, Frame(0), 1, Frame(f));
            img
        };
        assert_eq!(frame(10), frame(10), "same frame, same grain");
        assert_ne!(frame(10), frame(11), "next frame, new grain");
        let img = frame(10);
        assert!(img.pixels().iter().all(|p| (p[0] - 0.5).abs() <= 0.1));
        assert!(img.pixels().iter().any(|p| p[0] != img.pixels()[0][0]));
    }

    #[test]
    fn vignette_darkens_corners_not_the_centre() {
        let v = Effect::Vignette {
            strength: 1.0,
            radius: 0.2,
            softness: 0.5,
        };
        let mut img = Image::new(9, 9);
        img.fill([1.0; 4]);
        v.apply(&mut img, Frame(0), 1, Frame(0));
        assert_eq!(img.pixels()[4 * 9 + 4], [1.0; 4]);
        assert!(img.pixels()[0][0] < 0.2);
        assert_eq!(img.pixels()[0][3], 1.0, "alpha kept");
    }
}
