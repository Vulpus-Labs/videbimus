//! Frames, frame rates, intervals and musical time ([ADR 0001]).
//!
//! Everything that decides *which* frame or *whether* something is active
//! is an integer. Seconds are derived from frames, for computing values.
//!
//! [ADR 0001]: ../../../../adrs/0001-frames-and-time.md

use std::fmt;
use std::ops::{Add, Sub};

/// A frame number. Relative to a component's start when passed to it,
/// absolute at the top of the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Frame(pub i64);

impl Add<i64> for Frame {
    type Output = Frame;
    fn add(self, rhs: i64) -> Frame {
        Frame(self.0 + rhs)
    }
}

impl Sub<i64> for Frame {
    type Output = Frame;
    fn sub(self, rhs: i64) -> Frame {
        Frame(self.0 - rhs)
    }
}

impl Sub for Frame {
    type Output = i64;
    fn sub(self, rhs: Frame) -> i64 {
        self.0 - rhs.0
    }
}

impl fmt::Display for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "f{}", self.0)
    }
}

/// A frame rate as a fraction, `num / den` frames per second: 30/1,
/// 24/1, 30000/1001.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rate {
    num: u32,
    den: u32,
}

impl Rate {
    /// `num / den` frames per second.
    ///
    /// # Panics
    ///
    /// If either part is zero.
    #[must_use]
    pub const fn new(num: u32, den: u32) -> Self {
        assert!(
            num > 0 && den > 0,
            "a frame rate needs a non-zero numerator and denominator"
        );
        Self { num, den }
    }

    /// The numerator: frames per `den` seconds.
    #[must_use]
    pub const fn num(self) -> u32 {
        self.num
    }

    /// The denominator.
    #[must_use]
    pub const fn den(self) -> u32 {
        self.den
    }

    /// Frames per second, for display.
    #[must_use]
    pub fn fps(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// The time of frame `n`, in seconds. For computing values only.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn seconds(self, n: Frame) -> f64 {
        n.0 as f64 * f64::from(self.den) / f64::from(self.num)
    }

    /// The audio sample at the start of frame `n`, at `sample_rate`:
    /// `floor(n · sample_rate · den / num)`, exact in integers.
    ///
    /// # Panics
    ///
    /// If the sample position does not fit in an `i64` (frame numbers in
    /// the quintillions).
    #[must_use]
    pub fn sample(self, n: Frame, sample_rate: u32) -> i64 {
        let x = i128::from(n.0) * i128::from(sample_rate) * i128::from(self.den);
        let q = x.div_euclid(i128::from(self.num));
        i64::try_from(q).expect("sample position overflows i64")
    }

    /// The nearest frame to `seconds`. Used once, when config is resolved.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn frame_at(self, seconds: f64) -> Frame {
        Frame((seconds * f64::from(self.num) / f64::from(self.den)).round() as i64)
    }
}

/// A half-open range of frames, `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Interval {
    /// The first frame inside.
    pub start: Frame,
    /// The first frame after.
    pub end: Frame,
}

impl Interval {
    /// `[start, end)`.
    ///
    /// # Panics
    ///
    /// If `end` is before `start`.
    #[must_use]
    pub fn new(start: Frame, end: Frame) -> Self {
        assert!(
            end >= start,
            "interval ends ({end}) before it starts ({start})"
        );
        Self { start, end }
    }

    /// How many frames it holds.
    #[must_use]
    pub fn len(self) -> i64 {
        self.end - self.start
    }

    /// Whether it holds no frames.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.end == self.start
    }

    /// Whether frame `n` is inside.
    #[must_use]
    pub fn contains(self, n: Frame) -> bool {
        self.start <= n && n < self.end
    }

    /// `n` relative to the start: what a component is called with.
    #[must_use]
    pub fn local(self, n: Frame) -> Frame {
        Frame(n - self.start)
    }

    /// The same length, moved by `by` frames.
    #[must_use]
    pub fn shift(self, by: i64) -> Self {
        Self::new(self.start + by, self.end + by)
    }

    /// The frames both hold, if any: a crossfade's overlap.
    #[must_use]
    pub fn overlap(self, other: Self) -> Option<Self> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        (start < end).then(|| Self::new(start, end))
    }
}

/// A constant tempo, for turning bars and beats into frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tempo {
    /// Beats per minute.
    pub bpm: f64,
    /// Beats per bar.
    pub beats_per_bar: u32,
}

impl Tempo {
    /// The time of a musical position, in seconds from bar 0.
    #[must_use]
    pub fn seconds(self, bars: f64, beats: f64) -> f64 {
        (bars * f64::from(self.beats_per_bar) + beats) * 60.0 / self.bpm
    }

    /// The nearest frame to an absolute musical position. Every boundary
    /// is converted from its own position, never by adding rounded
    /// durations, so rounding error never builds up (ADR 0001 §4).
    #[must_use]
    pub fn frame(self, rate: Rate, bars: f64, beats: f64) -> Frame {
        rate.frame_at(self.seconds(bars, beats))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R30: Rate = Rate::new(30, 1);
    const NTSC: Rate = Rate::new(30_000, 1001);

    #[test]
    fn seconds_and_samples() {
        assert!((R30.seconds(Frame(45)) - 1.5).abs() < 1e-12);
        // 48 kHz at 30 fps: exactly 1600 samples a frame
        assert_eq!(R30.sample(Frame(7), 48_000), 11_200);
        // 44.1 kHz at 24 fps is 1837.5 a frame: floored, never drifting
        let r24 = Rate::new(24, 1);
        assert_eq!(r24.sample(Frame(1), 44_100), 1837);
        assert_eq!(r24.sample(Frame(2), 44_100), 3675);
        assert_eq!(r24.sample(Frame(240_000), 44_100), 441_000_000);
        // NTSC: 1001 seconds hold exactly 30000 frames
        assert_eq!(NTSC.sample(Frame(30_000), 48_000), 48_000 * 1001);
        assert_eq!(R30.sample(Frame(-1), 48_000), -1600);
    }

    #[test]
    fn frame_at_rounds_to_nearest() {
        assert_eq!(R30.frame_at(1.0), Frame(30));
        assert_eq!(R30.frame_at(1.0 / 30.0 * 2.4), Frame(2));
        assert_eq!(R30.frame_at(1.0 / 30.0 * 2.6), Frame(3));
        assert_eq!(NTSC.frame_at(1001.0), Frame(30_000));
    }

    #[test]
    fn intervals() {
        let a = Interval::new(Frame(10), Frame(20));
        assert_eq!(a.len(), 10);
        assert!(a.contains(Frame(10)) && a.contains(Frame(19)));
        assert!(!a.contains(Frame(20)) && !a.contains(Frame(9)));
        assert_eq!(a.local(Frame(15)), Frame(5));
        assert_eq!(a.shift(5), Interval::new(Frame(15), Frame(25)));
        // a crossfade: the outgoing shot runs 15 frames past the cut
        let b = Interval::new(Frame(5), Frame(25));
        let c = Interval::new(Frame(10), Frame(40));
        assert_eq!(b.overlap(c), Some(Interval::new(Frame(10), Frame(25))));
        assert_eq!(a.overlap(Interval::new(Frame(20), Frame(30))), None);
        assert!(Interval::new(Frame(3), Frame(3)).is_empty());
    }

    #[test]
    #[should_panic(expected = "before it starts")]
    fn interval_backwards() {
        let _ = Interval::new(Frame(2), Frame(1));
    }

    /// Fenland: 104 BPM in 4/4 at 30 fps. A bar is 69.23 frames, so
    /// boundaries can't all sit on bar lines; each is the nearest frame
    /// to its own position.
    #[test]
    fn musical_time_snaps_from_absolute_positions() {
        let t = Tempo {
            bpm: 104.0,
            beats_per_bar: 4,
        };
        assert_eq!(t.frame(R30, 4.0, 0.0), Frame(277)); // 276.92
        assert_eq!(t.frame(R30, 12.0, 0.0), Frame(831)); // 830.77
        // 500 bars: every boundary within half a frame of exact, and
        // consecutive durations sum to the last boundary (no drift)
        let mut total = 0;
        let mut prev = Frame(0);
        for bar in 1..=500 {
            let f = t.frame(R30, f64::from(bar), 0.0);
            let exact = t.seconds(f64::from(bar), 0.0) * 30.0;
            #[allow(clippy::cast_precision_loss)]
            let err = (f.0 as f64 - exact).abs();
            assert!(err <= 0.5, "bar {bar}: {f} vs {exact}");
            total += f - prev;
            prev = f;
        }
        assert_eq!(Frame(total), t.frame(R30, 500.0, 0.0));
    }
}
