//! What the store records about an asset and its chunks.

use std::fmt;

use serde::{Deserialize, Serialize};
use videbimus_core::{AssetKey, Frame, Interval};

use crate::error::fmt_interval;
use crate::key::{InputClass, InputRecord, Why};

/// The manifest format written by this version.
pub const MANIFEST_FORMAT: u32 = 1;

/// What an asset's output is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "ShapeRepr", into = "ShapeRepr")]
pub enum Shape {
    /// A single output, built in one go (an exported movie, a raster).
    Single,
    /// A frame sequence over these frames, built in chunks of frame ranges
    /// (ADR 0003 §5).
    Frames(Interval),
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "shape")]
enum ShapeRepr {
    Single,
    Frames { start: i64, end: i64 },
}

impl From<ShapeRepr> for Shape {
    fn from(r: ShapeRepr) -> Self {
        match r {
            ShapeRepr::Single => Shape::Single,
            ShapeRepr::Frames { start, end } => {
                Shape::Frames(Interval::new(Frame(start), Frame(end.max(start))))
            }
        }
    }
}

impl From<Shape> for ShapeRepr {
    fn from(s: Shape) -> Self {
        match s {
            Shape::Single => ShapeRepr::Single,
            Shape::Frames(iv) => ShapeRepr::Frames {
                start: iv.start.0,
                end: iv.end.0,
            },
        }
    }
}

impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Shape::Single => f.write_str("a single output"),
            Shape::Frames(iv) => write!(f, "frames {}", fmt_interval(*iv)),
        }
    }
}

/// Which part of an asset a chunk holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChunkId {
    /// All of a [`Shape::Single`] asset.
    Whole,
    /// A frame range of a [`Shape::Frames`] asset.
    Frames(Interval),
}

impl ChunkId {
    /// Its directory name under `chunks/`: `whole`, or `f<start>_<end>`.
    #[must_use]
    pub fn dir_name(self) -> String {
        match self {
            ChunkId::Whole => "whole".to_owned(),
            ChunkId::Frames(iv) => format!("f{}_{}", iv.start.0, iv.end.0),
        }
    }

    /// Parses a directory name from [`dir_name`](Self::dir_name).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        if name == "whole" {
            return Some(ChunkId::Whole);
        }
        let (a, b) = name.strip_prefix('f')?.split_once('_')?;
        let (a, b) = (a.parse().ok()?, b.parse().ok()?);
        (a < b).then(|| ChunkId::Frames(Interval::new(Frame(a), Frame(b))))
    }

    /// Its frames, if it is a frame range.
    #[must_use]
    pub fn frames(self) -> Option<Interval> {
        match self {
            ChunkId::Whole => None,
            ChunkId::Frames(iv) => Some(iv),
        }
    }

    fn sort_key(self) -> (i64, i64) {
        match self {
            ChunkId::Whole => (i64::MIN, i64::MIN),
            ChunkId::Frames(iv) => (iv.start.0, iv.end.0),
        }
    }
}

impl PartialOrd for ChunkId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ChunkId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

impl fmt::Display for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChunkId::Whole => f.write_str("whole"),
            ChunkId::Frames(iv) => f.write_str(&fmt_interval(*iv)),
        }
    }
}

/// Where an asset's build stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "status")]
pub enum Status {
    /// Begun; chunks may be committed, more may be missing. A build that
    /// was stopped or killed stays here and resumes (ADR 0008 §3).
    Building,
    /// Every chunk is committed.
    Complete,
    /// The last attempt failed. Its committed chunks are whole and kept.
    Failed {
        /// What failed.
        message: String,
    },
}

/// An external tool and the version a build used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolVersion {
    /// The tool.
    pub name: String,
    /// Its version.
    pub version: String,
}

/// `<key>/manifest.json`: what an asset is made of and how its build went.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The format version ([`MANIFEST_FORMAT`]).
    pub format: u32,
    /// The asset's key.
    #[serde(with = "crate::hex::key")]
    pub key: AssetKey,
    /// The name it was first built under (e.g. `s07/blender`).
    pub name: String,
    /// The recipe kind.
    pub kind: String,
    /// Its output's shape.
    #[serde(flatten)]
    pub shape: Shape,
    /// The labelled inputs its key hashes, for why-rebuilt diffs.
    pub inputs: Vec<InputRecord>,
    /// The tools it ran, with their versions (also among `inputs`).
    pub tools: Vec<ToolVersion>,
    /// Why it was first begun.
    #[serde(flatten)]
    pub why: Why,
    /// Where its build stands.
    #[serde(flatten)]
    pub status: Status,
    /// The machine that began it.
    pub machine: String,
    /// When it was first begun, in Unix milliseconds.
    pub started_ms: u64,
    /// When it was finished, in Unix milliseconds.
    #[serde(default)]
    pub finished_ms: Option<u64>,
    /// The sum of its chunks' build times, in milliseconds.
    #[serde(default)]
    pub build_ms: Option<u64>,
    /// The highest peak memory of any chunk or of the task, in bytes.
    #[serde(default)]
    pub peak_memory_bytes: Option<u64>,
    /// The hash of its output, if every chunk was hashed (early cutoff,
    /// ADR 0003 §4). Independent of how the frames were chunked.
    #[serde(default, with = "crate::hex::opt_digest")]
    pub output_hash: Option<[u8; 32]>,
    /// The task log, relative to the key's directory (ADR 0007 §1).
    pub log: String,
}

impl Manifest {
    pub(crate) fn tools_of(inputs: &[InputRecord]) -> Vec<ToolVersion> {
        inputs
            .iter()
            .filter(|r| r.class == InputClass::Tool)
            .map(|r| ToolVersion {
                name: r.label.clone(),
                version: r.detail.clone().unwrap_or_default(),
            })
            .collect()
    }
}

/// One output file's hash, by its path relative to the chunk's `out/`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileHash {
    /// The path, with `/` separators.
    pub path: String,
    /// The content hash.
    #[serde(with = "crate::hex::digest")]
    pub hash: [u8; 32],
}

/// `<key>/chunks/<chunk>/chunk.json`: a chunk's manifest entry, committed
/// with its output in one rename.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRecord {
    /// The format version ([`MANIFEST_FORMAT`]).
    pub format: u32,
    /// The first frame, for a frame range.
    #[serde(default)]
    pub start: Option<i64>,
    /// The first frame after, for a frame range.
    #[serde(default)]
    pub end: Option<i64>,
    /// When it was begun, in Unix milliseconds.
    pub started_ms: u64,
    /// When it was committed, in Unix milliseconds.
    pub finished_ms: u64,
    /// Its peak memory, in bytes, if measured.
    #[serde(default)]
    pub peak_memory_bytes: Option<u64>,
    /// The machine that built it.
    pub machine: String,
    /// Its output files' hashes, if hashed.
    #[serde(default)]
    pub files: Option<Vec<FileHash>>,
}

impl ChunkRecord {
    /// Which chunk it is.
    #[must_use]
    pub fn id(&self) -> ChunkId {
        match (self.start, self.end) {
            (Some(a), Some(b)) if a < b => ChunkId::Frames(Interval::new(Frame(a), Frame(b))),
            _ => ChunkId::Whole,
        }
    }

    /// How many frames it holds (0 for a single output).
    #[must_use]
    pub fn frame_count(&self) -> u64 {
        self.id()
            .frames()
            .map_or(0, |iv| u64::try_from(iv.len()).unwrap_or(0))
    }

    /// How long it took, in milliseconds.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        self.finished_ms.saturating_sub(self.started_ms)
    }

    /// The hash of its output files, if hashed.
    #[must_use]
    pub fn output_hash(&self) -> Option<[u8; 32]> {
        self.files.as_deref().map(hash_file_list)
    }
}

/// Hashes a list of file hashes, sorted by path: the same files give the
/// same hash however they were chunked.
pub(crate) fn hash_file_list(files: &[FileHash]) -> [u8; 32] {
    let mut sorted: Vec<&FileHash> = files.iter().collect();
    sorted.sort();
    let mut h = blake3::Hasher::new_derive_key("videbimus 2026-11-18 output files");
    for f in sorted {
        h.update(&(f.path.len() as u64).to_le_bytes());
        h.update(f.path.as_bytes());
        h.update(&f.hash);
    }
    *h.finalize().as_bytes()
}

/// `names/<name>.json`: the keys built under a name, newest first.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct NameRecord {
    pub name: String,
    pub generations: Vec<Generation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Generation {
    #[serde(with = "crate::hex::key")]
    pub key: AssetKey,
    pub at_ms: u64,
}
