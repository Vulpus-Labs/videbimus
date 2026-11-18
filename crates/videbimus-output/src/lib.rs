//! videbimus's output ([ADR 0008] §4–§5): the composite rendered and
//! encoded in keyed segments, joined by stream copy with the audio muxed,
//! and reviewed from whatever exists.
//!
//! # Config
//!
//! ```kdl
//! video rate=30 width=1920 height=1080
//! output "film.mp4" segment=(s)4 crf=18 preset="medium" {
//!     audio asset="mix"          // a declared asset's output file
//!     // or: audio "mix.wav"     // a file, relative to the config
//! }
//! ```
//!
//! See [`OutputSpec`] and [`AudioSpec`].
//!
//! # Tasks
//!
//! [`Output::plan`] adds to the asset graph:
//!
//! - **A task per segment** (`output/segment-00000`, …), each a
//!   single-output asset holding `segment.mp4`, `segment.json` and
//!   `state.bin`. Segments are `segment` frames long (4 s by default), on
//!   frame numbers from 0 to the root's end. A segment renders its frames
//!   through a fresh [`Scene`](videbimus_runtime::Scene) and pipes them to
//!   ffmpeg (through the task's subprocess helper, so stop, niceness and
//!   logging apply) as `rgb24`: premultiplied linear over black, sRGB
//!   encoded, then x264 4:2:0 BT.709. Its key:
//!   - kind `output-segment` and this crate's segment code identity;
//!   - config: the scene's canonical config (the `video` node and the
//!     root), the encoding, and its frames;
//!   - a source `kind/<kind>` per component kind the scene builds, with
//!     that kind's code identity: editing a kind's source re-renders
//!     every segment;
//!   - an input `asset/<name>` per declared asset the scene shows during
//!     its frames: rebuilding an asset re-renders only the segments that
//!     show it;
//!   - the tool `ffmpeg` and its version.
//! - **The join** (`output/join`): concatenates the segments with the
//!   concat demuxer and `-c copy`, and muxes the audio sample-exact
//!   (ADR 0001 §3) into the output's file name. Its inputs are the
//!   segments, labelled in order, and the audio asset; an audio file is a
//!   source. [`Output::export`] copies it to the configured path.
//!
//! Each segment is a separate encode, so each starts with an IDR frame
//! and no frame refers across a boundary: joining is a stream copy.
//!
//! # State across segments
//!
//! A segment saves the scene's state after its last frame
//! ([`state`]). The next segment starts from it if it exists and matches
//! the scene's config and code; otherwise its stateful components catch
//! up as usual (from frame 0, or within their horizon). The frames are
//! the same either way.
//!
//! # Review
//!
//! [`Output::review`] assembles the committed segments, with labelled
//! placeholders for missing ones, into a review file. It builds nothing.
//!
//! [ADR 0008]: ../../../adrs/0008-stop-and-resume.md

pub mod encode;
pub mod join;
mod output;
mod segment;
pub mod spec;
pub mod state;

pub use output::{NAME, Output, PlanContext, Review, Segment};
pub use segment::{SEGMENT_FILE, SEGMENT_META, SceneSource};
pub use spec::{AudioSource, AudioSpec, EncodeSettings, OutputSpec};
