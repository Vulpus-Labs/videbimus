//! Driving components for random access ([ADR 0002] §3–§4).
//!
//! A [`Child`] owns one component and everything needed to render any of
//! its frames in any order: the frame its state is at, and its state
//! checkpoints. To render frame `n` it asks [`plan_catch_up`] whether to
//! continue, restore a checkpoint or reset, then steps forward in a loop,
//! so the component only ever sees frames in order.
//!
//! - **Pure components** ([`Horizon::Zero`]) are reset once and never
//!   stepped: frame `n` is rendered directly.
//! - **Stateful components** keep a checkpoint after every frame that is
//!   a multiple of [`DriveOptions::checkpoint_every`], if they can make
//!   one. Rewinding restores the nearest one at or before the target.
//! - **Hidden components are not ticked.** A child is only stepped when
//!   its parent renders it; when it comes back after a gap, catch-up
//!   covers the gap, bounded by its horizon.
//!
//! Every container holds its children as [`Child`]ren, so every container
//! gets this right the same way. A container is itself
//! [`Horizon::Zero`]: it renders any of its frames on request, because its
//! children catch themselves up. (Rendering frames of one tree in parallel
//! therefore needs one tree per thread.)
//!
//! A [`Scene`] is the top of a tree: the root as a [`Child`] of the whole
//! video, the output [`Context`], and a small window of recently rendered
//! frames.
//!
//! [ADR 0002]: ../../../../adrs/0002-scene-components.md

use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroU32;

use videbimus_core::{
    AssetKey, Checkpoint, Component, Context, Frame, Horizon, Image, Interval, Origin, Rate,
    plan_catch_up,
};

/// How children are driven.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriveOptions {
    /// Keep a state checkpoint after every frame that is a multiple of
    /// this (local frames), or none if `None`. 60 by default.
    pub checkpoint_every: Option<NonZeroU32>,
}

impl Default for DriveOptions {
    fn default() -> Self {
        Self {
            checkpoint_every: NonZeroU32::new(60),
        }
    }
}

/// Counts of what a [`Child`] has done to its component, for tests and
/// diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DriveStats {
    /// Resets.
    pub resets: u64,
    /// Checkpoints restored.
    pub restores: u64,
    /// Frames stepped.
    pub steps: u64,
    /// Frames rendered.
    pub renders: u64,
    /// Checkpoints kept.
    pub checkpoints: u64,
}

/// A component as its parent holds it: driven for random access, and
/// addressed in the parent's frames.
pub struct Child {
    component: Box<dyn Component>,
    interval: Interval,
    horizon: Horizon,
    options: DriveOptions,
    /// The local frame whose state the component holds, if any.
    position: Option<Frame>,
    /// Whether `reset` has been called since it was made.
    initialised: bool,
    checkpoints: BTreeMap<Frame, Checkpoint>,
    stats: DriveStats,
}

impl std::fmt::Debug for Child {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Child")
            .field("interval", &self.interval)
            .field("horizon", &self.horizon)
            .field("position", &self.position)
            .field("checkpoints", &self.checkpoints.len())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Child {
    /// Takes ownership of `component`. Its interval and horizon are read
    /// once, here.
    #[must_use]
    pub fn new(component: Box<dyn Component>, options: DriveOptions) -> Self {
        Self {
            interval: component.interval(),
            horizon: component.horizon(),
            component,
            options,
            position: None,
            initialised: false,
            checkpoints: BTreeMap::new(),
            stats: DriveStats::default(),
        }
    }

    /// Its frames, in the parent's timeline.
    #[must_use]
    pub fn interval(&self) -> Interval {
        self.interval
    }

    /// Whether it shows at the parent's frame `n`.
    #[must_use]
    pub fn holds(&self, n: Frame) -> bool {
        self.interval.contains(n)
    }

    /// The component's horizon.
    #[must_use]
    pub fn horizon(&self) -> Horizon {
        self.horizon
    }

    /// What it has done so far.
    #[must_use]
    pub fn stats(&self) -> DriveStats {
        self.stats
    }

    /// The component.
    #[must_use]
    pub fn component(&self) -> &dyn Component {
        self.component.as_ref()
    }

    /// The assets it needs, given the parent's context.
    #[must_use]
    pub fn needs(&self, parent: &Context) -> Vec<AssetKey> {
        self.component.needs(&parent.child(self.interval))
    }

    /// Renders the parent's frame `n` into `out`, catching the component
    /// up first. `parent` is the parent's context. Does nothing if the
    /// child doesn't hold `n`.
    pub fn render(&mut self, n: Frame, parent: &Context, out: &mut Image) {
        if !self.holds(n) {
            return;
        }
        let ctx = parent.child(self.interval);
        let local = self.interval.local(n);
        self.seek(local, &ctx);
        self.stats.renders += 1;
        self.component.render(local, &ctx, out);
    }

    /// Brings the component to the state of local frame `target`.
    fn seek(&mut self, target: Frame, ctx: &Context) {
        if self.horizon == Horizon::Zero {
            if !self.initialised {
                self.component.reset(ctx);
                self.stats.resets += 1;
                self.initialised = true;
            }
            return;
        }
        let frames: Vec<Frame> = self.checkpoints.keys().copied().collect();
        let mut plan = plan_catch_up(self.position, target, self.horizon, &frames);
        match plan.origin {
            Origin::Continue => {}
            Origin::Restore(f) => {
                if self.component.restore(&self.checkpoints[&f]) {
                    self.stats.restores += 1;
                } else {
                    // it made the checkpoint but won't take it back: forget
                    // its checkpoints and plan again without them
                    self.checkpoints.clear();
                    plan = plan_catch_up(self.position, target, self.horizon, &[]);
                    if plan.origin != Origin::Continue {
                        self.reset(ctx);
                    }
                }
            }
            Origin::Reset(_) => self.reset(ctx),
        }
        for f in plan.steps.start.0..plan.steps.end.0 {
            let f = Frame(f);
            self.component.step(f, ctx);
            self.stats.steps += 1;
            self.position = Some(f);
            self.keep_checkpoint(f);
        }
        self.position = Some(target);
    }

    /// Its state as bytes, to persist across processes (ADR 0008 §4):
    /// the frame its component is at and the component's
    /// [`save_state`](Component::save_state). A component that can't
    /// save, or hasn't been initialised, saves as "unknown", and is caught
    /// up as usual after [`load_state`](Self::load_state). In-memory
    /// checkpoints aren't saved.
    #[must_use]
    pub fn save_state(&self) -> Vec<u8> {
        let saved = if self.initialised {
            self.component.save_state()
        } else {
            None
        };
        let Some(bytes) = saved else {
            return vec![STATE_UNKNOWN];
        };
        let mut out = Vec::with_capacity(bytes.len() + 18);
        out.push(STATE_KNOWN);
        out.extend_from_slice(&self.position.map_or(i64::MIN, |f| f.0).to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(&bytes);
        out
    }

    /// Restores bytes from [`save_state`](Self::save_state). Returns
    /// whether the component took its state; if not (or it was saved as
    /// unknown) it is left fresh, to be caught up as usual.
    pub fn load_state(&mut self, bytes: &[u8]) -> bool {
        let mut r = StateReader(bytes);
        match r.u8() {
            Some(STATE_KNOWN) => {}
            _ => return false,
        }
        let (Some(position), Some(state)) = (r.i64(), r.blob()) else {
            return false;
        };
        if !r.0.is_empty() || !self.component.load_state(state) {
            return false;
        }
        self.initialised = true;
        self.position = (position != i64::MIN).then_some(Frame(position));
        self.checkpoints.clear();
        true
    }

    fn reset(&mut self, ctx: &Context) {
        self.component.reset(ctx);
        self.stats.resets += 1;
        self.initialised = true;
    }

    fn keep_checkpoint(&mut self, f: Frame) {
        let Some(every) = self.options.checkpoint_every else {
            return;
        };
        if f.0 % i64::from(every.get()) != 0 || self.checkpoints.contains_key(&f) {
            return;
        }
        if let Some(cp) = self.component.checkpoint() {
            self.checkpoints.insert(f, cp);
            self.stats.checkpoints += 1;
        }
    }
}

const STATE_UNKNOWN: u8 = 0;
const STATE_KNOWN: u8 = 1;

/// Reads the little-endian fields of a saved state.
struct StateReader<'a>(&'a [u8]);

impl<'a> StateReader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")))
    }

    fn i64(&mut self) -> Option<i64> {
        self.take(8)
            .map(|b| i64::from_le_bytes(b.try_into().expect("8 bytes")))
    }

    fn blob(&mut self) -> Option<&'a [u8]> {
        let n = usize::try_from(self.u64()?).ok()?;
        self.take(n)
    }
}

/// A container's [`Component::save_state`]: each child's
/// [`Child::save_state`], in order. Always `Some`: a child that can't save
/// is recorded as unknown.
#[must_use]
pub fn save_children<'a>(children: impl IntoIterator<Item = &'a Child>) -> Option<Vec<u8>> {
    let states: Vec<Vec<u8>> = children.into_iter().map(Child::save_state).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&(states.len() as u64).to_le_bytes());
    for s in states {
        out.extend_from_slice(&(s.len() as u64).to_le_bytes());
        out.extend_from_slice(&s);
    }
    Some(out)
}

/// A container's [`Component::load_state`], from [`save_children`]: each
/// child takes its state, or is left to catch up. `false` if the bytes
/// don't hold one state per child (nothing is loaded then).
pub fn load_children<'a>(children: impl IntoIterator<Item = &'a mut Child>, bytes: &[u8]) -> bool {
    let children: Vec<&mut Child> = children.into_iter().collect();
    let mut r = StateReader(bytes);
    if r.u64().and_then(|n| usize::try_from(n).ok()) != Some(children.len()) {
        return false;
    }
    let mut states = Vec::with_capacity(children.len());
    for _ in 0..children.len() {
        let Some(s) = r.blob() else {
            return false;
        };
        states.push(s);
    }
    if !r.0.is_empty() {
        return false;
    }
    for (c, s) in children.into_iter().zip(states) {
        c.load_state(s);
    }
    true
}

/// The top of a component tree: renders frames of the whole video, in any
/// order.
#[derive(Debug)]
pub struct Scene {
    root: Child,
    ctx: Context,
    window: usize,
    recent: VecDeque<(Frame, Image)>,
}

impl Scene {
    /// A scene of `root`, whose interval is in video frames, rendered at
    /// `rate` and `width` × `height`. It keeps the last `window` frames it
    /// rendered in memory (0 for none).
    #[must_use]
    pub fn new(
        root: Box<dyn Component>,
        rate: Rate,
        width: u32,
        height: u32,
        options: DriveOptions,
        window: usize,
    ) -> Self {
        Self {
            root: Child::new(root, options),
            ctx: Context {
                rate,
                width,
                height,
                start: Frame(0),
            },
            window,
            recent: VecDeque::new(),
        }
    }

    /// The video's frames: the root's interval.
    #[must_use]
    pub fn interval(&self) -> Interval {
        self.root.interval()
    }

    /// The output context.
    #[must_use]
    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// The root, as driven.
    #[must_use]
    pub fn root(&self) -> &Child {
        &self.root
    }

    /// The assets the whole tree needs.
    #[must_use]
    pub fn needs(&self) -> Vec<AssetKey> {
        self.root.needs(&self.ctx)
    }

    /// The whole tree's state, to persist (ADR 0008 §4): the root's
    /// [`Child::save_state`], which containers fill with their children's.
    #[must_use]
    pub fn save_state(&self) -> Vec<u8> {
        self.root.save_state()
    }

    /// Restores [`save_state`](Self::save_state)'s bytes into a fresh
    /// scene of the same config. Components that took no state are caught
    /// up as usual, so frames rendered afterwards are the same either way.
    /// Returns whether the root took its state.
    pub fn load_state(&mut self, bytes: &[u8]) -> bool {
        self.recent.clear();
        self.root.load_state(bytes)
    }

    /// Video frame `n`: transparent outside the root's interval.
    pub fn render(&mut self, n: Frame) -> Image {
        if let Some(i) = self.recent.iter().position(|(f, _)| *f == n)
            && let Some(hit) = self.recent.remove(i)
        {
            let image = hit.1.clone();
            self.recent.push_back(hit);
            return image;
        }
        let mut out = Image::new(self.ctx.width, self.ctx.height);
        self.root.render(n, &self.ctx, &mut out);
        if self.window > 0 {
            if self.recent.len() == self.window {
                self.recent.pop_front();
            }
            self.recent.push_back((n, out.clone()));
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_sign_loss)]
pub(crate) mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// Shared counts of what was done to a test component, readable after
    /// it has been moved into a tree.
    #[derive(Clone, Debug, Default)]
    pub(crate) struct Counts {
        pub resets: Arc<AtomicU64>,
        pub restores: Arc<AtomicU64>,
        pub steps: Arc<AtomicU64>,
    }

    impl Counts {
        pub fn get(&self) -> (u64, u64, u64) {
            (
                self.resets.load(Ordering::Relaxed),
                self.restores.load(Ordering::Relaxed),
                self.steps.load(Ordering::Relaxed),
            )
        }
    }

    /// A stateful test component. With an unbounded horizon its state is
    /// a running hash of every frame stepped; with a horizon of H it is the
    /// sum of the last H + 1 frames' numbers, which depends only on them.
    /// It draws its state into red, and its frame into green.
    pub(crate) struct Walker {
        pub interval: Interval,
        pub horizon: Horizon,
        pub state: u64,
        pub window: VecDeque<i64>,
        pub counts: Counts,
    }

    impl Walker {
        pub fn new(interval: Interval, horizon: Horizon) -> Self {
            Self {
                interval,
                horizon,
                state: 0,
                window: VecDeque::new(),
                counts: Counts::default(),
            }
        }

        /// The state at frame `n`, computed directly.
        pub fn expected(horizon: Horizon, n: i64) -> u64 {
            match horizon {
                Horizon::Frames(h) => ((n - i64::from(h)).max(0)..=n).map(|f| f as u64).sum(),
                _ => (0..=n).fold(0, mix),
            }
        }
    }

    fn mix(state: u64, f: i64) -> u64 {
        (state ^ f as u64)
            .wrapping_mul(0x0100_0000_01b3)
            .rotate_left(5)
    }

    impl Component for Walker {
        fn interval(&self) -> Interval {
            self.interval
        }
        fn horizon(&self) -> Horizon {
            self.horizon
        }
        fn reset(&mut self, _: &Context) {
            self.state = 0;
            self.window.clear();
            self.counts.resets.fetch_add(1, Ordering::Relaxed);
        }
        fn step(&mut self, n: Frame, _: &Context) {
            self.counts.steps.fetch_add(1, Ordering::Relaxed);
            match self.horizon {
                Horizon::Frames(h) => {
                    self.window.push_back(n.0);
                    while self.window.len() > h as usize + 1 {
                        self.window.pop_front();
                    }
                    self.state = self.window.iter().map(|&f| f as u64).sum();
                }
                _ => self.state = mix(self.state, n.0),
            }
        }
        #[allow(clippy::cast_precision_loss)]
        fn render(&mut self, n: Frame, _: &Context, out: &mut Image) {
            out.fill([(self.state % 1_000_003) as f32, n.0 as f32, 0.0, 1.0]);
        }
        fn checkpoint(&self) -> Option<Checkpoint> {
            Some(Box::new((self.state, self.window.clone())))
        }
        fn restore(&mut self, cp: &Checkpoint) -> bool {
            self.counts.restores.fetch_add(1, Ordering::Relaxed);
            cp.downcast_ref::<(u64, VecDeque<i64>)>()
                .map(|(s, w)| {
                    self.state = *s;
                    self.window.clone_from(w);
                })
                .is_some()
        }
    }

    pub(crate) fn ctx() -> Context {
        Context {
            rate: Rate::new(30, 1),
            width: 1,
            height: 1,
            start: Frame(0),
        }
    }

    fn red(child: &mut Child, n: i64) -> f32 {
        let mut out = Image::new(1, 1);
        child.render(Frame(n), &ctx(), &mut out);
        out.pixels()[0][0]
    }

    #[allow(clippy::cast_precision_loss)]
    fn want(h: Horizon, local: i64) -> f32 {
        (Walker::expected(h, local) % 1_000_003) as f32
    }

    fn opts(k: u32) -> DriveOptions {
        DriveOptions {
            checkpoint_every: NonZeroU32::new(k),
        }
    }

    #[test]
    fn random_access_matches_sequential_rendering() {
        for h in [Horizon::Unbounded, Horizon::Frames(7)] {
            let w = Walker::new(Interval::new(Frame(10), Frame(310)), h);
            let mut c = Child::new(Box::new(w), opts(25));
            for n in [10, 11, 12, 300, 13, 200, 199, 309, 10, 150, 151, 40] {
                assert_eq!(red(&mut c, n), want(h, n - 10), "{h:?} frame {n}");
            }
        }
    }

    #[test]
    fn a_finite_horizon_bounds_catch_up_after_a_gap() {
        let w = Walker::new(Interval::new(Frame(0), Frame(1000)), Horizon::Frames(5));
        let counts = w.counts.clone();
        let mut c = Child::new(Box::new(w), opts(0));
        for n in 0..10 {
            red(&mut c, n);
        }
        assert_eq!(counts.get(), (1, 0, 10));
        // hidden from 10 to 500: six frames of warm-up, not 490
        assert_eq!(red(&mut c, 500), want(Horizon::Frames(5), 500));
        assert_eq!(counts.get(), (2, 0, 16));
    }

    #[test]
    fn rewinding_restores_the_nearest_checkpoint() {
        let w = Walker::new(Interval::new(Frame(0), Frame(1000)), Horizon::Unbounded);
        let counts = w.counts.clone();
        let mut c = Child::new(Box::new(w), opts(50));
        for n in 0..=200 {
            red(&mut c, n);
        }
        assert_eq!(c.stats().checkpoints, 5, "0, 50, 100, 150, 200");
        assert_eq!(counts.get(), (1, 0, 201));
        assert_eq!(red(&mut c, 120), want(Horizon::Unbounded, 120));
        assert_eq!(counts.get(), (1, 1, 221), "restored 100, stepped 20");
        // without checkpoints, rewinding replays from the start
        let w = Walker::new(Interval::new(Frame(0), Frame(1000)), Horizon::Unbounded);
        let counts = w.counts.clone();
        let mut c = Child::new(Box::new(w), opts(0));
        for n in 0..=200 {
            red(&mut c, n);
        }
        red(&mut c, 120);
        assert_eq!(counts.get(), (2, 0, 201 + 121));
    }

    #[test]
    fn pure_components_are_never_stepped() {
        let w = Walker::new(Interval::new(Frame(0), Frame(100)), Horizon::Zero);
        let counts = w.counts.clone();
        let mut c = Child::new(Box::new(w), DriveOptions::default());
        for n in [5, 90, 0, 42] {
            let mut out = Image::new(1, 1);
            c.render(Frame(n), &ctx(), &mut out);
            #[allow(clippy::cast_precision_loss)]
            let frame = n as f32;
            assert_eq!(out.pixels()[0][1], frame);
        }
        assert_eq!(counts.get(), (1, 0, 0), "reset once, to initialise");
    }

    #[test]
    fn children_outside_their_interval_draw_nothing() {
        let w = Walker::new(Interval::new(Frame(10), Frame(20)), Horizon::Unbounded);
        let counts = w.counts.clone();
        let mut c = Child::new(Box::new(w), DriveOptions::default());
        let mut out = Image::new(1, 1);
        c.render(Frame(20), &ctx(), &mut out);
        c.render(Frame(9), &ctx(), &mut out);
        assert_eq!(out.pixels()[0], [0.0; 4]);
        assert_eq!(counts.get(), (0, 0, 0));
    }

    #[test]
    fn the_scene_window_serves_repeats() {
        let w = Walker::new(Interval::new(Frame(0), Frame(100)), Horizon::Unbounded);
        let counts = w.counts.clone();
        let mut s = Scene::new(Box::new(w), Rate::new(30, 1), 1, 1, opts(0), 2);
        let a = s.render(Frame(50));
        s.render(Frame(51));
        assert_eq!(counts.get().2, 52);
        assert_eq!(s.render(Frame(50)), a, "from the window: no rewind");
        assert_eq!(counts.get(), (1, 0, 52));
        assert_eq!(s.render(Frame(200)).pixels()[0], [0.0; 4]);
    }
}
