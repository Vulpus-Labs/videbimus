//! The store's errors. Each names what went wrong and, where there is one,
//! the fix.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use videbimus_core::{AssetKey, Interval};

use crate::key::InputClass;
use crate::lock::LockHolder;
use crate::manifest::{ChunkId, Shape};

/// What can go wrong in the store.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A filesystem operation failed.
    Io {
        /// The path it was working on.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// A file in the store could not be parsed.
    Corrupt {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        message: String,
    },
    /// Another live process holds the store's lock (ADR 0008 §6).
    Locked {
        /// The lock file.
        path: PathBuf,
        /// Who holds it.
        holder: LockHolder,
        /// Whether the holder is on another machine, so its liveness could
        /// not be checked.
        other_host: bool,
    },
    /// A write was attempted through a store opened read-only.
    ReadOnly,
    /// Two key inputs of the same class share a label.
    DuplicateInput {
        /// The class.
        class: InputClass,
        /// The repeated label.
        label: String,
    },
    /// A labelled key input has an empty label.
    EmptyLabel {
        /// The class.
        class: InputClass,
    },
    /// A key input file could not be read.
    InputFile {
        /// The input's label.
        label: String,
        /// The file.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// An asset name is empty.
    EmptyName,
    /// The key is already being written by this process.
    AlreadyWriting {
        /// The key.
        key: AssetKey,
    },
    /// The key is already complete; it is a cache hit, not a build.
    AlreadyComplete {
        /// The key.
        key: AssetKey,
    },
    /// The key was begun with a different shape before.
    ShapeMismatch {
        /// The key.
        key: AssetKey,
        /// The shape in its manifest.
        recorded: Shape,
        /// The shape asked for now.
        requested: Shape,
    },
    /// A chunk does not fit the asset's shape.
    ChunkOutsideShape {
        /// The key.
        key: AssetKey,
        /// The chunk.
        chunk: ChunkId,
        /// The asset's shape.
        shape: Shape,
    },
    /// A chunk overlaps one already committed.
    ChunkOverlaps {
        /// The key.
        key: AssetKey,
        /// The new chunk.
        chunk: ChunkId,
        /// The committed one it overlaps.
        committed: ChunkId,
    },
    /// An asset was finished with frames still missing.
    Incomplete {
        /// The key.
        key: AssetKey,
        /// What is missing.
        missing: Vec<ChunkId>,
    },
}

/// The store's result type.
pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn io_err(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_owned(),
        source,
    }
}

pub(crate) fn fmt_interval(iv: Interval) -> String {
    format!("[{}, {})", iv.start.0, iv.end.0)
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Corrupt { path, message } => write!(
                f,
                "{}: {message}; delete it and rebuild the asset if it can't be repaired",
                path.display()
            ),
            Error::Locked {
                path,
                holder,
                other_host: false,
            } => write!(
                f,
                "the store is in use by {holder} (lock {}); stop that build first",
                path.display()
            ),
            Error::Locked {
                path,
                holder,
                other_host: true,
            } => write!(
                f,
                "the store is locked by {holder} (lock {}); its liveness can't be checked from \
                 this machine, so stop that build, or delete the lock file if it is gone",
                path.display()
            ),
            Error::ReadOnly => write!(
                f,
                "the store was opened read-only; open it with `Store::open` to build"
            ),
            Error::DuplicateInput { class, label } => write!(
                f,
                "key input `{class} {label}` is given twice; give each {class} a distinct label"
            ),
            Error::EmptyLabel { class } => {
                write!(f, "a {class} key input has an empty label; label it")
            }
            Error::InputFile {
                label,
                path,
                source,
            } => write!(
                f,
                "key input `{label}`: can't read {}: {source}",
                path.display()
            ),
            Error::EmptyName => write!(f, "an asset name is empty; name it, e.g. `s07/blender`"),
            Error::AlreadyWriting { key } => {
                write!(f, "asset {key} is already being written by this build")
            }
            Error::AlreadyComplete { key } => write!(
                f,
                "asset {key} is already complete; use it from the store instead of rebuilding"
            ),
            Error::ShapeMismatch {
                key,
                recorded,
                requested,
            } => write!(
                f,
                "asset {key} was begun as {recorded}, now asked for as {requested}; \
                 the shape must be part of the key's config"
            ),
            Error::ChunkOutsideShape { key, chunk, shape } => {
                write!(
                    f,
                    "chunk {chunk} of asset {key} is outside its shape, {shape}"
                )
            }
            Error::ChunkOverlaps {
                key,
                chunk,
                committed,
            } => write!(
                f,
                "chunk {chunk} of asset {key} overlaps committed chunk {committed}; \
                 build only the missing frames"
            ),
            Error::Incomplete { key, missing } => {
                write!(f, "asset {key} can't be finished: missing")?;
                for m in missing {
                    write!(f, " {m}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } | Error::InputFile { source, .. } => Some(source),
            _ => None,
        }
    }
}
