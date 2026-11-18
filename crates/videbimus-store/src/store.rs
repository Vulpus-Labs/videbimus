//! The store itself: opening, building assets in chunks, querying, GC.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use videbimus_core::{AssetKey, Frame, Interval};

use crate::error::{Error, Result, io_err};
use crate::fsutil::{
    escape_name, hash_files, now_ms, replace_atomic, sync_dir, sync_tree, tree_size,
};
use crate::hex::{key_hex, parse_key};
use crate::key::{KeyInputs, Why};
use crate::lock::{Lock, Warning, machine_name};
use crate::manifest::{
    ChunkId, ChunkRecord, FileHash, Generation, MANIFEST_FORMAT, Manifest, NameRecord, Shape,
    Status, hash_file_list,
};

const TMP: &str = "tmp";
const NAMES: &str = "names";
const CHUNKS: &str = "chunks";
const MANIFEST: &str = "manifest.json";
const CHUNK_MANIFEST: &str = "chunk.json";
const OUT: &str = "out";
const LOG: &str = "log.txt";

/// A content-addressed asset store on disk. Cheap to clone; clones share
/// the lock, which is released when the last one drops.
#[derive(Clone, Debug)]
pub struct Store {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    root: PathBuf,
    lock: Option<Lock>,
    /// Keys with a live [`AssetWriter`]. GC holds this for its whole run.
    active: Mutex<HashSet<AssetKey>>,
    counter: AtomicU64,
    machine: String,
}

/// Whether a rebuilt output equals the one before it (ADR 0003 §4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cutoff {
    /// The output hash equals the previous complete build's: dependants
    /// can stay cached.
    Same {
        /// The previous key.
        previous: AssetKey,
    },
    /// The output differs from the previous complete build's.
    Different {
        /// The previous key.
        previous: AssetKey,
    },
    /// No earlier complete build under this name.
    NoPrevious,
    /// This build, or the previous one, wasn't hashed, or isn't complete.
    NotHashed,
}

/// How far an asset has got, from its committed chunks only (ADR 0007 §4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Its shape.
    pub shape: Shape,
    /// Its status.
    pub status: Status,
    /// Its committed chunks, in order.
    pub committed: Vec<ChunkRecord>,
    /// Frames committed (0 for a single output).
    pub done_frames: u64,
    /// Frames in all (0 for a single output).
    pub total_frames: u64,
    /// What remains to build: the gaps between committed chunks, or
    /// [`ChunkId::Whole`].
    pub missing: Vec<ChunkId>,
}

/// What a GC run removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Keys deleted, with the bytes each used.
    pub removed: Vec<(AssetKey, u64)>,
    /// Bytes freed in all.
    pub freed_bytes: u64,
    /// Keys kept.
    pub kept: usize,
}

impl Store {
    /// Opens (creating if need be) the store at `root` for building: takes
    /// its lock, and deletes the temporaries an interrupted build left
    /// (ADR 0008 §1, §6). Returns warnings for the caller to show.
    ///
    /// # Errors
    ///
    /// [`Error::Locked`] if another live build holds the store, or I/O.
    pub fn open(root: impl AsRef<Path>) -> Result<(Store, Vec<Warning>)> {
        let root = root.as_ref().to_owned();
        fs::create_dir_all(&root).map_err(io_err(&root))?;
        let (lock, warning) = Lock::acquire(&root)?;
        let mut warnings: Vec<Warning> = warning.into_iter().collect();
        let tmp = root.join(TMP);
        let mut removed = 0;
        match fs::read_dir(&tmp) {
            Ok(rd) => {
                for entry in rd {
                    let path = entry.map_err(io_err(&tmp))?.path();
                    remove_any(&path)?;
                    removed += 1;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&tmp)(e)),
        }
        if removed > 0 {
            warnings.push(Warning::TemporariesRemoved { count: removed });
        }
        for dir in [&tmp, &root.join(NAMES)] {
            fs::create_dir_all(dir).map_err(io_err(dir))?;
        }
        Ok((Self::new(root, Some(lock)), warnings))
    }

    /// Opens the store at `root` for reading only, without its lock: for
    /// `videbimus status` and review while a build runs. Writes return
    /// [`Error::ReadOnly`].
    ///
    /// # Errors
    ///
    /// If `root` isn't a directory.
    pub fn open_read_only(root: impl AsRef<Path>) -> Result<Store> {
        let root = root.as_ref().to_owned();
        let meta = fs::metadata(&root).map_err(io_err(&root))?;
        if !meta.is_dir() {
            return Err(io_err(&root)(io::Error::other("not a directory")));
        }
        Ok(Self::new(root, None))
    }

    fn new(root: PathBuf, lock: Option<Lock>) -> Store {
        Store {
            inner: Arc::new(Inner {
                root,
                lock,
                active: Mutex::new(HashSet::new()),
                counter: AtomicU64::new(0),
                machine: machine_name(),
            }),
        }
    }

    /// The store's root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Whether it was opened read-only.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.inner.lock.is_none()
    }

    /// `<store>/<key>/`.
    #[must_use]
    pub fn key_dir(&self, key: AssetKey) -> PathBuf {
        self.inner.root.join(key_hex(&key))
    }

    /// The output directory of a committed chunk:
    /// `<store>/<key>/chunks/<chunk>/out/`.
    #[must_use]
    pub fn chunk_dir(&self, key: AssetKey, chunk: ChunkId) -> PathBuf {
        self.key_dir(key)
            .join(CHUNKS)
            .join(chunk.dir_name())
            .join(OUT)
    }

    /// A key's task log: `<store>/<key>/log.txt`.
    #[must_use]
    pub fn log_path(&self, key: AssetKey) -> PathBuf {
        self.key_dir(key).join(LOG)
    }

    fn writable(&self) -> Result<()> {
        if self.is_read_only() {
            Err(Error::ReadOnly)
        } else {
            Ok(())
        }
    }

    fn active(&self) -> MutexGuard<'_, HashSet<AssetKey>> {
        self.inner
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn staged(&self, what: &str) -> PathBuf {
        let n = self.inner.counter.fetch_add(1, Ordering::Relaxed);
        self.inner
            .root
            .join(TMP)
            .join(format!("{what}.{}.{n}", std::process::id()))
    }

    /// A key's manifest, if it has one.
    ///
    /// # Errors
    ///
    /// I/O, or a manifest that doesn't parse.
    pub fn manifest(&self, key: AssetKey) -> Result<Option<Manifest>> {
        read_json(&self.key_dir(key).join(MANIFEST))
    }

    fn write_manifest(&self, m: &Manifest) -> Result<()> {
        let bytes = to_json(m);
        let dest = self.key_dir(m.key).join(MANIFEST);
        replace_atomic(&self.staged("manifest"), &dest, &bytes)
    }

    fn name_path(&self, name: &str) -> PathBuf {
        self.inner
            .root
            .join(NAMES)
            .join(format!("{}.json", escape_name(name)))
    }

    fn name_record(&self, name: &str) -> Result<NameRecord> {
        Ok(
            read_json(&self.name_path(name))?.unwrap_or_else(|| NameRecord {
                name: name.to_owned(),
                generations: Vec::new(),
            }),
        )
    }

    fn write_name_record(&self, rec: &NameRecord) -> Result<()> {
        let path = self.name_path(&rec.name);
        if rec.generations.is_empty() {
            return match fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_err(&path)(e)),
                _ => Ok(()),
            };
        }
        let bytes = to_json(rec);
        replace_atomic(&self.staged("name"), &path, &bytes)
    }

    /// The keys built under `name`, newest first.
    ///
    /// # Errors
    ///
    /// I/O, or a name record that doesn't parse.
    pub fn generations(&self, name: &str) -> Result<Vec<AssetKey>> {
        Ok(self
            .name_record(name)?
            .generations
            .into_iter()
            .map(|g| g.key)
            .collect())
    }

    /// The newest key built (or used) under `name`.
    ///
    /// # Errors
    ///
    /// As [`generations`](Self::generations).
    pub fn latest(&self, name: &str) -> Result<Option<AssetKey>> {
        Ok(self.generations(name)?.first().copied())
    }

    /// Records that `name` now means `key`, making it the newest
    /// generation. [`begin`](Self::begin) does this; the scheduler calls
    /// it for cache hits too, so the next rebuild diffs against what was
    /// actually used.
    ///
    /// # Errors
    ///
    /// [`Error::ReadOnly`], [`Error::EmptyName`], or I/O.
    pub fn record(&self, name: &str, key: AssetKey) -> Result<()> {
        self.writable()?;
        if name.is_empty() {
            return Err(Error::EmptyName);
        }
        let mut rec = self.name_record(name)?;
        if rec.generations.first().is_some_and(|g| g.key == key) {
            return Ok(());
        }
        rec.generations.retain(|g| g.key != key);
        rec.generations.insert(
            0,
            Generation {
                key,
                at_ms: now_ms(),
            },
        );
        self.write_name_record(&rec)
    }

    /// Why building `inputs` under `name` is needed, against the name's
    /// newest key (ADR 0003 §7).
    ///
    /// # Errors
    ///
    /// I/O, or a manifest or name record that doesn't parse.
    pub fn why(&self, name: &str, inputs: &KeyInputs) -> Result<Why> {
        let key = inputs.key();
        let Some(previous) = self.latest(name)? else {
            return Ok(Why::New);
        };
        if previous == key {
            return Ok(Why::Resume);
        }
        Ok(match self.manifest(previous)? {
            Some(m) => Why::Changed {
                previous,
                changes: inputs.changes_since(&m.inputs),
            },
            None => Why::PreviousGone { previous },
        })
    }

    /// Begins (or resumes) building `inputs`' key under `name`. Writes its
    /// manifest with status [`Status::Building`] and why it is being built,
    /// and records it as the name's newest generation. A resumed key keeps
    /// its committed chunks and its original reason.
    ///
    /// # Errors
    ///
    /// [`Error::AlreadyComplete`] for a cache hit, [`Error::AlreadyWriting`]
    /// if this store already has a writer for the key,
    /// [`Error::ShapeMismatch`] if it was begun with another shape,
    /// [`Error::ReadOnly`], [`Error::EmptyName`], or I/O.
    pub fn begin(&self, name: &str, inputs: &KeyInputs, shape: Shape) -> Result<AssetWriter> {
        self.writable()?;
        if name.is_empty() {
            return Err(Error::EmptyName);
        }
        let key = inputs.key();
        let mut active = self.active();
        if active.contains(&key) {
            return Err(Error::AlreadyWriting { key });
        }
        let manifest = match self.manifest(key)? {
            Some(m) if m.status == Status::Complete => {
                return Err(Error::AlreadyComplete { key });
            }
            Some(m) if m.shape != shape => {
                return Err(Error::ShapeMismatch {
                    key,
                    recorded: m.shape,
                    requested: shape,
                });
            }
            Some(mut m) => {
                if m.status != Status::Building {
                    m.status = Status::Building;
                    self.write_manifest(&m)?;
                }
                m
            }
            None => {
                let why = self.why(name, inputs)?;
                let dir = self.key_dir(key);
                fs::create_dir_all(dir.join(CHUNKS)).map_err(io_err(&dir))?;
                let m = Manifest {
                    format: MANIFEST_FORMAT,
                    key,
                    name: name.to_owned(),
                    kind: inputs.kind().to_owned(),
                    shape,
                    inputs: inputs.records().to_vec(),
                    tools: Manifest::tools_of(inputs.records()),
                    why,
                    status: Status::Building,
                    machine: self.inner.machine.clone(),
                    started_ms: now_ms(),
                    finished_ms: None,
                    build_ms: None,
                    peak_memory_bytes: None,
                    output_hash: None,
                    log: LOG.to_owned(),
                };
                self.write_manifest(&m)?;
                m
            }
        };
        self.record(name, key)?;
        active.insert(key);
        Ok(AssetWriter {
            store: self.clone(),
            name: name.to_owned(),
            manifest,
        })
    }

    /// A key's committed chunks, in frame order. Only these count as
    /// progress, and only these are cache hits on resume (ADR 0007 §4,
    /// ADR 0008 §3).
    ///
    /// # Errors
    ///
    /// I/O, or a chunk record that doesn't parse.
    pub fn committed(&self, key: AssetKey) -> Result<Vec<ChunkRecord>> {
        let dir = self.key_dir(key).join(CHUNKS);
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err(&dir)(e)),
        };
        let mut out = Vec::new();
        for entry in rd {
            let entry = entry.map_err(io_err(&dir))?;
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(ChunkId::parse) else {
                continue;
            };
            if let Some(rec) = read_json::<ChunkRecord>(&entry.path().join(CHUNK_MANIFEST))?
                && rec.id() == id
            {
                out.push(rec);
            }
        }
        out.sort_by_key(ChunkRecord::id);
        Ok(out)
    }

    /// How far a key has got, or `None` if it was never begun.
    ///
    /// # Errors
    ///
    /// As [`manifest`](Self::manifest) and [`committed`](Self::committed).
    pub fn progress(&self, key: AssetKey) -> Result<Option<Progress>> {
        let Some(m) = self.manifest(key)? else {
            return Ok(None);
        };
        let committed = self.committed(key)?;
        let missing = missing(m.shape, &committed);
        let total_frames = match m.shape {
            Shape::Single => 0,
            Shape::Frames(iv) => u64::try_from(iv.len()).unwrap_or(0),
        };
        Ok(Some(Progress {
            shape: m.shape,
            status: m.status,
            done_frames: committed.iter().map(ChunkRecord::frame_count).sum(),
            total_frames,
            missing,
            committed,
        }))
    }

    /// Whether `key`'s output equals that of the newest earlier complete
    /// build under `name` (ADR 0003 §4).
    ///
    /// # Errors
    ///
    /// I/O, or a manifest or name record that doesn't parse.
    pub fn early_cutoff(&self, name: &str, key: AssetKey) -> Result<Cutoff> {
        let Some(mine) = self.manifest(key)? else {
            return Ok(Cutoff::NotHashed);
        };
        let Some(hash) = mine.output_hash.filter(|_| mine.status == Status::Complete) else {
            return Ok(Cutoff::NotHashed);
        };
        for previous in self.generations(name)? {
            if previous == key {
                continue;
            }
            let Some(m) = self.manifest(previous)? else {
                continue;
            };
            if m.status != Status::Complete {
                continue;
            }
            return Ok(match m.output_hash {
                Some(h) if h == hash => Cutoff::Same { previous },
                Some(_) => Cutoff::Different { previous },
                None => Cutoff::NotHashed,
            });
        }
        Ok(Cutoff::NoPrevious)
    }

    /// What a dependant should put in its own key for `key`
    /// ([`KeyBuilder::input`](crate::KeyBuilder::input)): the output hash
    /// if the asset is complete and hashed, so an identical rebuild leaves
    /// dependants' keys, and so their cache, unchanged (ADR 0003 §4); else
    /// the key itself. `None` until the asset is complete.
    ///
    /// # Errors
    ///
    /// As [`manifest`](Self::manifest).
    pub fn identity_for_dependants(&self, key: AssetKey) -> Result<Option<AssetKey>> {
        Ok(self
            .manifest(key)?
            .filter(|m| m.status == Status::Complete)
            .map(|m| m.output_hash.map_or(key, AssetKey)))
    }

    /// Deletes every key that is neither in `reachable` (what the current
    /// config reaches, inputs included), nor among the `keep_generations`
    /// newest unreachable keys of some name, nor being written. Name
    /// records are pruned to the keys left. Deleted directories are first
    /// renamed into `tmp/`, so a crash mid-delete leaves nothing that looks
    /// like an asset.
    ///
    /// With `keep_generations` 0, only reachable keys stay; with 1, each
    /// name also keeps the newest build it no longer uses (so a reverted
    /// edit is still a cache hit). A name the config no longer has keeps
    /// its generations the same way; a key no name records is kept only if
    /// reachable.
    ///
    /// # Errors
    ///
    /// [`Error::ReadOnly`], or I/O.
    pub fn gc(&self, reachable: &HashSet<AssetKey>, keep_generations: usize) -> Result<GcReport> {
        self.writable()?;
        let active = self.active();
        let mut keep: HashSet<AssetKey> = reachable.union(&active).copied().collect();
        let names_dir = self.inner.root.join(NAMES);
        let mut records = Vec::new();
        for entry in fs::read_dir(&names_dir).map_err(io_err(&names_dir))? {
            let path = entry.map_err(io_err(&names_dir))?.path();
            if path.extension().is_some_and(|e| e == "json")
                && let Some(rec) = read_json::<NameRecord>(&path)?
            {
                records.push(rec);
            }
        }
        for rec in &records {
            keep.extend(
                rec.generations
                    .iter()
                    .map(|g| g.key)
                    .filter(|k| !reachable.contains(k) && self.key_dir(*k).is_dir())
                    .take(keep_generations),
            );
        }

        let mut report = GcReport::default();
        let root = &self.inner.root;
        for entry in fs::read_dir(root).map_err(io_err(root))? {
            let entry = entry.map_err(io_err(root))?;
            let Some(key) = entry.file_name().to_str().and_then(parse_key) else {
                continue;
            };
            if keep.contains(&key) {
                report.kept += 1;
                continue;
            }
            let dir = entry.path();
            let size = tree_size(&dir);
            let doomed = self.staged(&format!("gc.{}", key_hex(&key)));
            fs::rename(&dir, &doomed).map_err(io_err(&dir))?;
            remove_any(&doomed)?;
            report.removed.push((key, size));
            report.freed_bytes += size;
        }
        sync_dir(root);

        for mut rec in records {
            let before = rec.generations.len();
            rec.generations.retain(|g| self.key_dir(g.key).is_dir());
            if rec.generations.len() != before {
                self.write_name_record(&rec)?;
            }
        }
        report.removed.sort();
        Ok(report)
    }
}

fn missing(shape: Shape, committed: &[ChunkRecord]) -> Vec<ChunkId> {
    match shape {
        Shape::Single => {
            if committed.iter().any(|c| c.id() == ChunkId::Whole) {
                Vec::new()
            } else {
                vec![ChunkId::Whole]
            }
        }
        Shape::Frames(iv) => {
            let mut out = Vec::new();
            let mut at = iv.start;
            for c in committed.iter().filter_map(|c| c.id().frames()) {
                if c.start > at {
                    out.push(ChunkId::Frames(Interval::new(at, c.start.min(iv.end))));
                }
                at = at.max(c.end);
            }
            if at < iv.end {
                out.push(ChunkId::Frames(Interval::new(at, iv.end)));
            }
            out
        }
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec_pretty(value).expect("store records always serialise")
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| Error::Corrupt {
                path: path.to_owned(),
                message: e.to_string(),
            }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path)(e)),
    }
}

fn remove_any(path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err(path)(e)),
    };
    let r = if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    match r {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_err(path)(e)),
        _ => Ok(()),
    }
}

/// Builds one asset: hands out chunk writers, then finishes or fails it.
/// While it lives, GC won't touch its key. Dropping it without finishing
/// leaves the asset [`Status::Building`], to resume later.
#[derive(Debug)]
pub struct AssetWriter {
    store: Store,
    name: String,
    manifest: Manifest,
}

/// Options for [`ChunkWriter::commit`].
#[derive(Clone, Copy, Debug, Default)]
pub struct CommitOptions {
    /// Hash the chunk's output files, for early cutoff (ADR 0003 §4).
    pub hash: bool,
    /// The chunk's peak memory in bytes, if the task measured it.
    pub peak_memory_bytes: Option<u64>,
}

impl AssetWriter {
    /// The asset's key.
    #[must_use]
    pub fn key(&self) -> AssetKey {
        self.manifest.key
    }

    /// The name it is being built under.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its shape.
    #[must_use]
    pub fn shape(&self) -> Shape {
        self.manifest.shape
    }

    /// Why it is being built.
    #[must_use]
    pub fn why(&self) -> &Why {
        &self.manifest.why
    }

    /// Its task log, for the task to append to (ADR 0007 §1).
    #[must_use]
    pub fn log_path(&self) -> PathBuf {
        self.store.log_path(self.key())
    }

    /// Its committed chunks.
    ///
    /// # Errors
    ///
    /// As [`Store::committed`].
    pub fn committed(&self) -> Result<Vec<ChunkRecord>> {
        self.store.committed(self.key())
    }

    /// What remains to build.
    ///
    /// # Errors
    ///
    /// As [`Store::committed`].
    pub fn missing(&self) -> Result<Vec<ChunkId>> {
        Ok(missing(self.shape(), &self.committed()?))
    }

    fn check_chunk(&self, chunk: ChunkId) -> Result<()> {
        let key = self.key();
        let fits = match (self.shape(), chunk) {
            (Shape::Single, ChunkId::Whole) => true,
            (Shape::Frames(iv), ChunkId::Frames(c)) => {
                c.start < c.end && c.start >= iv.start && c.end <= iv.end
            }
            _ => false,
        };
        if !fits {
            return Err(Error::ChunkOutsideShape {
                key,
                chunk,
                shape: self.shape(),
            });
        }
        for c in self.committed()? {
            let overlaps = match (c.id(), chunk) {
                (ChunkId::Frames(a), ChunkId::Frames(b)) => a.overlap(b).is_some(),
                _ => true,
            };
            if overlaps {
                return Err(Error::ChunkOverlaps {
                    key,
                    chunk,
                    committed: c.id(),
                });
            }
        }
        Ok(())
    }

    /// Starts a chunk: a temporary directory inside the store to write its
    /// output into. Nothing of it is visible until
    /// [`commit`](ChunkWriter::commit); dropped uncommitted, it is deleted.
    ///
    /// # Errors
    ///
    /// [`Error::ChunkOutsideShape`], [`Error::ChunkOverlaps`] with a
    /// committed chunk, or I/O.
    pub fn chunk(&self, chunk: ChunkId) -> Result<ChunkWriter<'_>> {
        self.check_chunk(chunk)?;
        let tmp = self.store.staged(&format!(
            "chunk.{}.{}",
            &key_hex(&self.key())[..16],
            chunk.dir_name()
        ));
        let out = tmp.join(OUT);
        fs::create_dir_all(&out).map_err(io_err(&out))?;
        Ok(ChunkWriter {
            asset: self,
            chunk,
            tmp,
            started_ms: now_ms(),
            done: false,
        })
    }

    /// Marks the asset complete: every frame committed. Records the total
    /// build time, peak memory (the highest of the chunks' and
    /// `peak_memory_bytes`, the task's own), and the output hash if every
    /// chunk was hashed.
    ///
    /// # Errors
    ///
    /// [`Error::Incomplete`] if chunks are missing, or I/O.
    pub fn finish(mut self, peak_memory_bytes: Option<u64>) -> Result<Manifest> {
        let committed = self.committed()?;
        let missing = missing(self.shape(), &committed);
        if !missing.is_empty() {
            return Err(Error::Incomplete {
                key: self.key(),
                missing,
            });
        }
        let files: Option<Vec<FileHash>> = committed
            .iter()
            .map(|c| c.files.clone())
            .collect::<Option<Vec<_>>>()
            .map(|v| v.into_iter().flatten().collect());
        let m = &mut self.manifest;
        m.output_hash = files.as_deref().map(hash_file_list);
        m.build_ms = Some(committed.iter().map(ChunkRecord::duration_ms).sum());
        m.peak_memory_bytes = committed
            .iter()
            .filter_map(|c| c.peak_memory_bytes)
            .chain(peak_memory_bytes)
            .max();
        m.finished_ms = Some(now_ms());
        m.status = Status::Complete;
        self.store.write_manifest(&self.manifest)?;
        Ok(self.manifest.clone())
    }

    /// Marks the asset failed. Its committed chunks stay, whole, for the
    /// retry.
    ///
    /// # Errors
    ///
    /// I/O.
    pub fn fail(mut self, message: &str) -> Result<()> {
        self.manifest.status = Status::Failed {
            message: message.to_owned(),
        };
        self.manifest.finished_ms = Some(now_ms());
        self.store.write_manifest(&self.manifest)
    }
}

impl Drop for AssetWriter {
    fn drop(&mut self) {
        self.store.active().remove(&self.manifest.key);
    }
}

/// One chunk being written. Write its output under
/// [`out_dir`](Self::out_dir), then [`commit`](Self::commit).
#[derive(Debug)]
pub struct ChunkWriter<'a> {
    asset: &'a AssetWriter,
    chunk: ChunkId,
    tmp: PathBuf,
    started_ms: u64,
    done: bool,
}

impl ChunkWriter<'_> {
    /// Which chunk it is.
    #[must_use]
    pub fn id(&self) -> ChunkId {
        self.chunk
    }

    /// Where to write the chunk's output: a temporary directory inside the
    /// store, on the same filesystem as its final place.
    #[must_use]
    pub fn out_dir(&self) -> PathBuf {
        self.tmp.join(OUT)
    }

    /// Commits the chunk: writes its manifest entry beside its output,
    /// syncs both, and renames the pair into `<key>/chunks/` in one step
    /// (ADR 0008 §1). A crash before the rename leaves only a temporary,
    /// deleted on the next open; after it, a whole chunk.
    ///
    /// # Errors
    ///
    /// [`Error::ChunkOverlaps`] if an overlapping chunk was committed
    /// meanwhile, or I/O.
    pub fn commit(mut self, options: CommitOptions) -> Result<ChunkRecord> {
        let frames = self.chunk.frames();
        let files = if options.hash {
            Some(hash_files(&self.out_dir())?)
        } else {
            None
        };
        let rec = ChunkRecord {
            format: MANIFEST_FORMAT,
            start: frames.map(|f| f.start.0),
            end: frames.map(|f| f.end.0),
            started_ms: self.started_ms,
            finished_ms: now_ms(),
            peak_memory_bytes: options.peak_memory_bytes,
            machine: self.asset.store.inner.machine.clone(),
            files,
        };
        let bytes = to_json(&rec);
        fs::write(self.tmp.join(CHUNK_MANIFEST), bytes).map_err(io_err(&self.tmp))?;
        sync_tree(&self.tmp)?;
        self.asset.check_chunk(self.chunk)?;
        let chunks = self.asset.store.key_dir(self.asset.key()).join(CHUNKS);
        fs::create_dir_all(&chunks).map_err(io_err(&chunks))?;
        let dest = chunks.join(self.chunk.dir_name());
        // A committed chunk's directory is never empty, so the rename can't
        // replace one; the check above makes the error a clear one.
        fs::rename(&self.tmp, &dest).map_err(io_err(&dest))?;
        sync_dir(&chunks);
        self.done = true;
        Ok(rec)
    }
}

impl Drop for ChunkWriter<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = fs::remove_dir_all(&self.tmp);
        }
    }
}

/// Frames as a [`ChunkId`]: `frames(0, 48)` is frames `[0, 48)`.
///
/// # Panics
///
/// If `end` is before `start`.
#[must_use]
pub fn frames(start: i64, end: i64) -> ChunkId {
    ChunkId::Frames(Interval::new(Frame(start), Frame(end)))
}
