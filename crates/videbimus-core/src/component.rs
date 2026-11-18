//! The scene-component contract ([ADR 0002] §1).
//!
//! A component renders its frame `n`, counted from its own start, into an
//! image. Stateful components advance one frame per [`Component::step`]. The
//! framework decides when to reset, restore or step, using
//! [`plan_catch_up`](crate::plan_catch_up), so a component only ever sees
//! frames in order.
//!
//! [ADR 0002]: ../../../../adrs/0002-scene-components.md

use std::any::Any;
use std::fmt;

use crate::catch_up::Horizon;
use crate::time::{Frame, Interval, Rate};

/// The key of a built asset: a content hash of what it is made from
/// (ADR 0003 §2).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AssetKey(pub [u8; 32]);

impl fmt::Display for AssetKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0[..8] {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for AssetKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AssetKey({self})")
    }
}

/// What a component is told about where it renders.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    /// The output frame rate.
    pub rate: Rate,
    /// The output size in pixels.
    pub width: u32,
    /// The output height in pixels.
    pub height: u32,
    /// The component's first frame, on the top-level timeline.
    pub start: Frame,
}

impl Context {
    /// The absolute top-level frame of local frame `n`.
    #[must_use]
    pub fn absolute(&self, n: Frame) -> Frame {
        self.start + n.0
    }

    /// Seconds since the start of the video at local frame `n`, for
    /// values keyed to the song (automation, chords).
    #[must_use]
    pub fn time(&self, n: Frame) -> f64 {
        self.rate.seconds(self.absolute(n))
    }

    /// Seconds since the component's own start at local frame `n`.
    #[must_use]
    pub fn local_time(&self, n: Frame) -> f64 {
        self.rate.seconds(n)
    }

    /// The context a child with interval `child` (in this component's
    /// frames) renders in.
    #[must_use]
    pub fn child(&self, child: Interval) -> Self {
        Self {
            start: self.start + child.start.0,
            ..*self
        }
    }
}

/// An image to render into: premultiplied linear RGBA, `f32` per channel,
/// row by row from the top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    width: u32,
    height: u32,
    pixels: Vec<[f32; 4]>,
}

impl Image {
    /// A transparent image.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![[0.0; 4]; width as usize * height as usize],
        }
    }

    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// All pixels, row by row.
    #[must_use]
    pub fn pixels(&self) -> &[[f32; 4]] {
        &self.pixels
    }

    /// All pixels, row by row, to write.
    pub fn pixels_mut(&mut self) -> &mut [[f32; 4]] {
        &mut self.pixels
    }

    /// Fills with one premultiplied colour.
    pub fn fill(&mut self, rgba: [f32; 4]) {
        self.pixels.fill(rgba);
    }
}

/// A saved state, opaque to the framework.
pub type Checkpoint = Box<dyn Any + Send>;

/// A scene component (ADR 0002 §1). Object-safe: parents hold children as
/// `Box<dyn Component>`.
pub trait Component {
    /// Its frames, in its parent's timeline.
    fn interval(&self) -> Interval;

    /// How much history its state depends on. Pure by default.
    fn horizon(&self) -> Horizon {
        Horizon::Zero
    }

    /// The assets it needs built before it can render (ADR 0003). Asking
    /// does no work.
    fn needs(&self, _ctx: &Context) -> Vec<AssetKey> {
        Vec::new()
    }

    /// Back to the initial state.
    fn reset(&mut self, ctx: &Context);

    /// Advances the state to frame `n`, from frame `n − 1` (or from the
    /// reset). Pure components need not do anything.
    fn step(&mut self, _n: Frame, _ctx: &Context) {}

    /// Draws frame `n`'s state into `out`.
    fn render(&mut self, n: Frame, ctx: &Context, out: &mut Image);

    /// A snapshot of the current state, if it can make one.
    fn checkpoint(&self) -> Option<Checkpoint> {
        None
    }

    /// Restores a snapshot from [`checkpoint`](Self::checkpoint). Returns
    /// whether it could.
    fn restore(&mut self, _cp: &Checkpoint) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catch_up::{Origin, plan_catch_up};

    /// A stateful test component: its state is how many frames it has
    /// stepped since a reset, and it draws that count into red.
    struct Counter {
        interval: Interval,
        count: i64,
    }

    impl Component for Counter {
        fn interval(&self) -> Interval {
            self.interval
        }
        fn horizon(&self) -> Horizon {
            Horizon::Unbounded
        }
        fn reset(&mut self, _ctx: &Context) {
            self.count = 0;
        }
        fn step(&mut self, _n: Frame, _ctx: &Context) {
            self.count += 1;
        }
        #[allow(clippy::cast_precision_loss)]
        fn render(&mut self, _n: Frame, _ctx: &Context, out: &mut Image) {
            out.fill([self.count as f32, 0.0, 0.0, 1.0]);
        }
        fn checkpoint(&self) -> Option<Checkpoint> {
            Some(Box::new(self.count))
        }
        fn restore(&mut self, cp: &Checkpoint) -> bool {
            cp.downcast_ref::<i64>().map(|&c| self.count = c).is_some()
        }
    }

    /// Drives `c` to `target` the way the runtime will (0006), and
    /// renders.
    fn render_at(
        c: &mut dyn Component,
        pos: &mut Option<Frame>,
        cps: &[(Frame, Checkpoint)],
        target: Frame,
        ctx: &Context,
    ) -> f32 {
        let frames: Vec<Frame> = cps.iter().map(|(f, _)| *f).collect();
        let plan = plan_catch_up(*pos, target, c.horizon(), &frames);
        match plan.origin {
            Origin::Continue => {}
            Origin::Restore(f) => {
                let cp = &cps.iter().find(|(g, _)| *g == f).unwrap().1;
                assert!(c.restore(cp));
            }
            Origin::Reset(_) => c.reset(ctx),
        }
        for n in plan.steps.start.0..plan.steps.end.0 {
            c.step(Frame(n), ctx);
        }
        *pos = Some(target);
        let mut out = Image::new(1, 1);
        c.render(target, ctx, &mut out);
        out.pixels()[0][0]
    }

    /// Random access through a trait object gives the same state as
    /// stepping in order: frame n has stepped n + 1 frames.
    #[test]
    fn random_access_matches_sequential_rendering() {
        let ctx = Context {
            rate: Rate::new(30, 1),
            width: 1,
            height: 1,
            start: Frame(100),
        };
        let mut c: Box<dyn Component> = Box::new(Counter {
            interval: Interval::new(Frame(100), Frame(1100)),
            count: 0,
        });
        c.reset(&ctx);
        let mut pos = None;
        let mut cps = Vec::new();
        for n in 0..=300 {
            render_at(c.as_mut(), &mut pos, &cps, Frame(n), &ctx);
            if n % 100 == 0 {
                cps.push((Frame(n), c.checkpoint().unwrap()));
            }
        }
        // jump back, forward, back again
        for target in [150, 999, 5, 300, 299] {
            let got = render_at(c.as_mut(), &mut pos, &cps, Frame(target), &ctx);
            #[allow(clippy::cast_possible_truncation)]
            let got = got as i64;
            assert_eq!(got, target + 1, "frame {target}");
        }
    }

    #[test]
    fn contexts_nest() {
        let top = Context {
            rate: Rate::new(30, 1),
            width: 1920,
            height: 1080,
            start: Frame(0),
        };
        let shot = top.child(Interval::new(Frame(300), Frame(600)));
        let caption = shot.child(Interval::new(Frame(24), Frame(276)));
        assert_eq!(caption.absolute(Frame(6)), Frame(330));
        assert!((caption.time(Frame(6)) - 11.0).abs() < 1e-12);
        assert!((caption.local_time(Frame(6)) - 0.2).abs() < 1e-12);
    }

    #[test]
    fn asset_keys_print_short() {
        let mut k = [0u8; 32];
        k[0] = 0xab;
        k[7] = 0x01;
        assert_eq!(AssetKey(k).to_string(), "ab00000000000001");
    }
}
