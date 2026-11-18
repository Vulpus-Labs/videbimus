//! videbimus's logs and progress ([ADR 0007]).
//!
//! The scheduler sends [`Event`](videbimus_scheduler::Event)s; this crate
//! turns them into what people and tools read while a build runs and
//! after it:
//!
//! - **The run event log.** A [`Recorder`] appends every event of a run
//!   as one JSON line to `<store>/runs/<UTC time>-<pid>.jsonl`, and
//!   names the newest in `<store>/runs/latest`. The newest 50 are kept.
//!   (Each task's own log, `<store>/<key>/log.txt`, is the scheduler's.)
//! - **The tree.** [`Tree`] folds events into a hierarchy that mirrors
//!   the scene: groups (a task's scene group, else its name's directory)
//!   over assets, each with its [`State`], frames committed of the total,
//!   elapsed time, ETA and machine. Groups aggregate their children; an
//!   all-cached group collapses to one line. ETAs come from the plan's
//!   estimates (cost history, ADR 0004 §2), replaced by the rates
//!   measured as chunks commit, and include waiting for inputs; the
//!   run's is the longer of the longest chain and the remaining
//!   core-seconds spread over the core budget.
//! - **Progress only from the store.** Frames done are those committed
//!   under the current key: what the scheduler reports when an asset is
//!   begun (from the store's committed chunks), plus chunks committed
//!   since. Files lying in folders never count (ADR 0007 §4).
//! - **The status file**, `<store>/status.json`: a [`Snapshot`] of the
//!   tree as JSON, rewritten atomically every second and once at the end.
//! - **The live view**: [`render`] draws a snapshot at a given width;
//!   [`View::Live`] redraws it in place on a terminal a few times a
//!   second. [`View::Plain`] is for output that isn't a terminal.
//! - **`status`**: [`status_report`] prints the running build's tree,
//!   or, with nothing running, the last run's outcome and each asset
//!   against the current config ([`asset_statuses`]).
//!
//! [ADR 0007]: ../../../adrs/0007-logs-and-progress.md

mod record;
mod render;
mod status;
mod term;
mod tree;

pub use record::{
    EventLine, LATEST, RUNS, Recorder, RecorderOptions, STATUS, View, compact_utc, latest_run,
    read_status, replace_atomic, runs_dir, status_path, utc,
};
pub use render::{FAILURE_LINES, bytes, duration, render};
pub use status::{AssetStatus, KeyState, asset_statuses, running, status_report};
pub use term::{size as terminal_size, stdout_is_terminal};
pub use tree::{
    Budget, Failure, NodeView, RunInfo, RunOutcome, SNAPSHOT_FORMAT, Snapshot, State, Tree,
};

/// Now, in Unix milliseconds.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
