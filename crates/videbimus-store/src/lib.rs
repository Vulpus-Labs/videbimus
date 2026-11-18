//! videbimus's asset store: content-addressed, on disk ([ADR 0003] §6).
//!
//! An asset's key hashes its labelled inputs ([`KeyBuilder`]). Its output
//! is built in chunks, each committed whole by one rename ([ADR 0008] §1),
//! so a stopped build resumes from the chunks present. Each asset also has
//! a *name* (`s07/blender`): the stable slot in the scene that its keys
//! fill over time. The name links a new key to the one before it, so the
//! store can say why it rebuilt ([ADR 0003] §7), whether the output
//! changed (early cutoff, §4), and which old generations to keep in GC.
//!
//! # Layout
//!
//! ```text
//! <store>/
//!   lock                        the writer's pid, host and start (JSON)
//!   tmp/                        in-flight temporaries; emptied on open
//!   names/<name>.json           keys built under a name, newest first
//!                               (`/` and other bytes escaped as %XX)
//!   <key>/                      one asset; <key> is 64 hex digits
//!     manifest.json             inputs, tools, why, status, timings,
//!                               peak memory, machine, output hash
//!     log.txt                   its task log (ADR 0007 §1)
//!     chunks/
//!       whole/                  a single output's one chunk, or
//!       f<start>_<end>/         a frame range [start, end)
//!         chunk.json            the chunk's manifest entry
//!         out/                  its output files
//! ```
//!
//! A chunk is written under `tmp/`, with its `chunk.json` beside its
//! output, synced, then renamed into `chunks/`. The rename is the commit:
//! a chunk directory is either absent or whole. Progress and resume read
//! only `chunks/` of the current key ([ADR 0007] §4). Manifests and name
//! records are replaced by an atomic rename from `tmp/` too.
//!
//! # Use
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use videbimus_store::{CommitOptions, KeyBuilder, Shape, Store, frames};
//! use videbimus_core::{Frame, Interval};
//! # let dir = tempfile::tempdir()?;
//! let (store, warnings) = Store::open(dir.path().join("store"))?;
//! for w in &warnings {
//!     eprintln!("warning: {w}");
//! }
//! let inputs = KeyBuilder::new("blender-shot")
//!     .config(br#"{"samples":64}"#)
//!     .script("hwscene.py", b"import bpy\n")
//!     .tool("blender", "5.2.0")
//!     .build()?;
//! let shot = Shape::Frames(Interval::new(Frame(0), Frame(96)));
//! let asset = store.begin("s07/blender", &inputs, shot)?;
//! println!("building s07/blender: {}", asset.why());
//! for id in asset.missing()? {
//!     let chunk = asset.chunk(id)?;
//!     std::fs::write(chunk.out_dir().join("0001.png"), b"...")?;
//!     chunk.commit(CommitOptions { hash: true, ..CommitOptions::default() })?;
//! }
//! let manifest = asset.finish(None)?;
//! # assert!(manifest.output_hash.is_some());
//! # Ok(()) }
//! ```
//!
//! # Platform
//!
//! Unix first. The store relies on atomic `rename` of directories and on
//! `hard_link` for the lock, both on one filesystem. On Windows, renaming a
//! directory fails while any file in it is open, lock liveness isn't
//! checked (a stale lock must be deleted by hand), and `fsync` of
//! directories is skipped.
//!
//! [ADR 0003]: ../../../adrs/0003-asset-graph-and-cache.md
//! [ADR 0007]: ../../../adrs/0007-logs-and-progress.md
//! [ADR 0008]: ../../../adrs/0008-stop-and-resume.md

mod error;
mod fsutil;
mod hex;
mod key;
mod lock;
mod manifest;
mod store;

pub use error::{Error, Result};
pub use hex::{key_hex, parse_key};
pub use key::{Change, ChangeKind, InputClass, InputRecord, KeyBuilder, KeyInputs, Why};
pub use lock::{LockHolder, Warning, machine_name};
pub use manifest::{
    ChunkId, ChunkRecord, FileHash, MANIFEST_FORMAT, Manifest, Shape, Status, ToolVersion,
};
pub use store::{
    AssetWriter, ChunkWriter, CommitOptions, Cutoff, GcReport, Progress, Store, frames,
};
