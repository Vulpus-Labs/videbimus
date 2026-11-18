//! Random access for stateful components ([ADR 0002] §2–§3).
//!
//! A component's state after stepping frame `p` is `state(p)`. To render
//! frame `n`, the framework needs `state(n)`. It can continue from where
//! the component is, restore a checkpoint, or reset and step from a point
//! its horizon says is far enough back, whichever steps the fewest frames.
//!
//! [ADR 0002]: ../../../../adrs/0002-scene-components.md

use crate::time::{Frame, Interval};

/// How much history a component's state depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Horizon {
    /// Pure: frame `n` depends on `n` alone. Any order, in parallel.
    Zero,
    /// Frame `n` depends only on the last `H` frames: resetting at `n − H`
    /// and stepping to `n` gives the right state.
    Frames(u32),
    /// Exact: state depends on every frame since the start.
    Unbounded,
}

/// Where the steps start from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The component already holds the state just before the first step.
    Continue,
    /// Restore the checkpoint taken after this frame, then step.
    Restore(Frame),
    /// Reset to the initial state, then step from this frame.
    Reset(Frame),
}

/// How to reach `state(target)`: an origin, then `step` for every frame
/// in `steps`, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Where to start.
    pub origin: Origin,
    /// The frames to step, ending at the target. Empty if the component is
    /// already there.
    pub steps: Interval,
}

/// The cheapest way to bring a component to `state(target)`.
///
/// `position` is the frame whose state the component holds now, if any.
/// `checkpoints` are the frames whose state is saved, in any order. Ties
/// go to continuing, then restoring, then resetting.
///
/// # Panics
///
/// If `target` is negative: components are called with frames relative
/// to their start.
#[must_use]
pub fn plan_catch_up(
    position: Option<Frame>,
    target: Frame,
    horizon: Horizon,
    checkpoints: &[Frame],
) -> Plan {
    assert!(
        target.0 >= 0,
        "catch-up target {target} is before the component starts"
    );
    let after = |from: Frame| Interval::new(from + 1, target + 1);

    let reset_at = match horizon {
        Horizon::Zero => target,
        Horizon::Frames(h) => Frame((target.0 - i64::from(h)).max(0)),
        Horizon::Unbounded => Frame(0),
    };
    let mut best = Plan {
        origin: Origin::Reset(reset_at),
        steps: Interval::new(reset_at, target + 1),
    };

    if let Some(&c) = checkpoints.iter().filter(|&&c| c <= target).max() {
        let restore = Plan {
            origin: Origin::Restore(c),
            steps: after(c),
        };
        if restore.steps.len() <= best.steps.len() {
            best = restore;
        }
    }
    if let Some(p) = position.filter(|&p| p <= target) {
        let cont = Plan {
            origin: Origin::Continue,
            steps: after(p),
        };
        if cont.steps.len() <= best.steps.len() {
            best = cont;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(pos: Option<i64>, target: i64, h: Horizon, cps: &[i64]) -> (Origin, i64, i64) {
        let cps: Vec<Frame> = cps.iter().map(|&c| Frame(c)).collect();
        let p = plan_catch_up(pos.map(Frame), Frame(target), h, &cps);
        (p.origin, p.steps.start.0, p.steps.end.0)
    }

    #[test]
    fn pure_components_render_any_frame_directly() {
        assert_eq!(
            plan(Some(10), 500, Horizon::Zero, &[]),
            (Origin::Reset(Frame(500)), 500, 501)
        );
        // already there: nothing to step
        assert_eq!(
            plan(Some(7), 7, Horizon::Zero, &[]),
            (Origin::Continue, 8, 8)
        );
    }

    #[test]
    fn sequential_rendering_just_continues() {
        assert_eq!(
            plan(Some(99), 100, Horizon::Unbounded, &[0, 50]),
            (Origin::Continue, 100, 101)
        );
    }

    #[test]
    fn a_finite_horizon_bounds_catch_up_after_a_gap() {
        // hidden from 100 to 1000: 30 frames of warm-up, not 900
        assert_eq!(
            plan(Some(100), 1000, Horizon::Frames(30), &[]),
            (Origin::Reset(Frame(970)), 970, 1001)
        );
        // near the start, the reset can't go before frame 0
        assert_eq!(
            plan(None, 10, Horizon::Frames(30), &[]),
            (Origin::Reset(Frame(0)), 0, 11)
        );
        // but continuing is cheaper when the gap is short
        assert_eq!(
            plan(Some(990), 1000, Horizon::Frames(30), &[]),
            (Origin::Continue, 991, 1001)
        );
    }

    #[test]
    fn unbounded_state_restores_the_nearest_checkpoint() {
        // rewinding: the component is past the target, so it can't continue
        assert_eq!(
            plan(Some(800), 420, Horizon::Unbounded, &[0, 300, 400, 600]),
            (Origin::Restore(Frame(400)), 401, 421)
        );
        // with no checkpoint, from the very start
        assert_eq!(
            plan(None, 420, Horizon::Unbounded, &[]),
            (Origin::Reset(Frame(0)), 0, 421)
        );
        // a checkpoint at the target itself: no steps
        assert_eq!(
            plan(None, 400, Horizon::Unbounded, &[400]),
            (Origin::Restore(Frame(400)), 401, 401)
        );
    }

    #[test]
    fn the_cheapest_origin_wins_and_ties_prefer_continuing() {
        // continue (from 380: 40 steps) beats the checkpoint at 300 (120)
        assert_eq!(
            plan(Some(380), 420, Horizon::Unbounded, &[300]),
            (Origin::Continue, 381, 421)
        );
        // equal cost: continue, then restore, then reset
        assert_eq!(
            plan(Some(400), 420, Horizon::Unbounded, &[400]),
            (Origin::Continue, 401, 421)
        );
        assert_eq!(
            plan(None, 30, Horizon::Frames(30), &[0]),
            (Origin::Restore(Frame(0)), 1, 31)
        );
    }

    #[test]
    #[should_panic(expected = "before the component starts")]
    fn negative_targets_are_a_bug() {
        let _ = plan_catch_up(None, Frame(-1), Horizon::Zero, &[]);
    }
}
