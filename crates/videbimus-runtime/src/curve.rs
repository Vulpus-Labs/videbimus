//! Opacity curves: a level, ramped in at the start and out at the end.
//!
//! The vocabulary is small on purpose: a constant, and a ramp in and a
//! ramp out of given lengths, each linear or smoothstep. A ramp in of `L`
//! frames is 0 at the first frame and reaches 1 at frame `L`; a ramp out
//! mirrors it, reaching 0 at the last frame. So a fade from black really
//! starts black and a fade to black really ends black.

use videbimus_config::{Error, Node, Resolver};
use videbimus_core::Frame;

/// The shape of a ramp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Ease {
    /// Straight.
    #[default]
    Linear,
    /// Smoothstep: `3t² − 2t³`, flat at both ends.
    Smooth,
}

impl Ease {
    /// The names config uses.
    pub const NAMES: [&'static str; 2] = ["linear", "smooth"];

    /// `t` in `0..=1` eased.
    #[must_use]
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::Smooth => t * t * (3.0 - 2.0 * t),
        }
    }
}

/// A level with ramps in and out, over a component's local frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Envelope {
    /// The level between the ramps.
    pub level: f32,
    /// Frames to ramp in over; 0 for none.
    pub ramp_in: i64,
    /// Frames to ramp out over; 0 for none.
    pub ramp_out: i64,
    /// The ramps' shape.
    pub ease: Ease,
}

impl Default for Envelope {
    fn default() -> Self {
        Self::constant(1.0)
    }
}

impl Envelope {
    /// No ramps.
    #[must_use]
    pub fn constant(level: f32) -> Self {
        Self {
            level,
            ramp_in: 0,
            ramp_out: 0,
            ease: Ease::Linear,
        }
    }

    /// The value at local frame `k` of something `len` frames long.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn at(&self, k: Frame, len: i64) -> f32 {
        let mut v = self.level;
        if self.ramp_in > 0 && k.0 < self.ramp_in {
            v *= self.ease.apply(k.0 as f32 / self.ramp_in as f32);
        }
        let from_end = len - 1 - k.0;
        if self.ramp_out > 0 && from_end < self.ramp_out {
            v *= self.ease.apply(from_end as f32 / self.ramp_out as f32);
        }
        v
    }

    /// Reads an envelope from `node`: the level from `level` (if given),
    /// the ramps from durations `ramp_in` and `ramp_out`, and the shape
    /// from `ease`. Absent properties leave the defaults.
    ///
    /// # Errors
    ///
    /// If a property has the wrong type, a level is outside `0..=1`, a
    /// ramp is negative, or the ease isn't known.
    #[allow(clippy::cast_possible_truncation)]
    pub fn read(
        node: &Node,
        r: &Resolver,
        level: Option<&str>,
        ramp_in: &str,
        ramp_out: &str,
    ) -> Result<Self, Error> {
        let mut env = Self::default();
        if let Some(key) = level
            && let Some(l) = r.opt::<f64>(node, key)?
        {
            if !(0.0..=1.0).contains(&l) {
                return Err(r
                    .error(
                        node,
                        &crate::registry::origin_of(node, key),
                        format!("`{key}` is {l}, outside 0 to 1"),
                    )
                    .help(format!("{key}=1 is opaque, {key}=0 invisible")));
            }
            env.level = l as f32;
        }
        for (key, slot) in [(ramp_in, &mut env.ramp_in), (ramp_out, &mut env.ramp_out)] {
            if let Some(f) = r.opt::<Frame>(node, key)? {
                if f.0 < 0 {
                    return Err(r
                        .error(
                            node,
                            &crate::registry::origin_of(node, key),
                            format!("`{key}` is negative ({f})"),
                        )
                        .help("give a duration of zero or more"));
                }
                *slot = f.0;
            }
        }
        env.ease = read_ease(node, r)?;
        Ok(env)
    }
}

/// The `ease` property: `linear` (the default) or `smooth`.
///
/// # Errors
///
/// If it isn't a known name.
pub fn read_ease(node: &Node, r: &Resolver) -> Result<Ease, Error> {
    Ok(match read_name(node, r, "ease", &Ease::NAMES)? {
        Some("smooth") => Ease::Smooth,
        _ => Ease::Linear,
    })
}

/// A string property that must be one of `names`, if present.
///
/// # Errors
///
/// If it is present and isn't a string or isn't one of them.
pub fn read_name<'n>(
    node: &Node,
    r: &Resolver,
    key: &str,
    names: &[&'n str],
) -> Result<Option<&'n str>, Error> {
    let Some(s) = r.opt::<String>(node, key)? else {
        return Ok(None);
    };
    if let Some(n) = names.iter().find(|n| **n == s) {
        Ok(Some(n))
    } else {
        {
            let help = match crate::registry::did_you_mean(&s, names) {
                Some(n) => format!("did you mean \"{n}\"?"),
                None => format!(
                    "use one of {}",
                    names
                        .iter()
                        .map(|n| format!("\"{n}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            Err(r
                .error(
                    node,
                    &crate::registry::origin_of(node, key),
                    format!("`{key}` can't be \"{s}\""),
                )
                .help(help))
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn ramps_hit_their_ends_exactly() {
        let e = Envelope {
            level: 1.0,
            ramp_in: 4,
            ramp_out: 4,
            ease: Ease::Linear,
        };
        let v: Vec<f32> = (0..10).map(|k| e.at(Frame(k), 10)).collect();
        assert_eq!(v, [0.0, 0.25, 0.5, 0.75, 1.0, 1.0, 0.75, 0.5, 0.25, 0.0]);
        let s = Envelope {
            ease: Ease::Smooth,
            ..e
        };
        assert_eq!(s.at(Frame(0), 10), 0.0);
        assert_eq!(s.at(Frame(2), 10), 0.5);
        assert!(s.at(Frame(1), 10) < 0.25, "slow at the ends");
        assert_eq!(Envelope::constant(0.3).at(Frame(5), 10), 0.3);
    }
}
