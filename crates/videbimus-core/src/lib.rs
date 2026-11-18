//! videbimus's core: the integer time base ([ADR 0001]), the random-access
//! planner for stateful components, and the scene-component contract
//! ([ADR 0002]).
//!
//! [ADR 0001]: ../../../adrs/0001-frames-and-time.md
//! [ADR 0002]: ../../../adrs/0002-scene-components.md

pub mod catch_up;
pub mod component;
pub mod time;

pub use catch_up::{Horizon, Origin, Plan, plan_catch_up};
pub use component::{AssetKey, Checkpoint, Component, Context, Image};
pub use time::{Frame, Interval, Rate, Tempo};
