//! Small filesystem helpers: durable writes, atomic replacement, tree walks.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

use crate::error::{Result, io_err};
use crate::manifest::FileHash;

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Writes `bytes` to `path` and syncs them to disk.
pub(crate) fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = File::create(path).map_err(io_err(path))?;
    f.write_all(bytes).map_err(io_err(path))?;
    f.sync_all().map_err(io_err(path))
}

/// Replaces `dest` with `bytes` atomically: staged at `staged` (on the same
/// filesystem), synced, renamed over.
pub(crate) fn replace_atomic(staged: &Path, dest: &Path, bytes: &[u8]) -> Result<()> {
    write_synced(staged, bytes)?;
    fs::rename(staged, dest).map_err(io_err(dest))?;
    if let Some(parent) = dest.parent() {
        sync_dir(parent);
    }
    Ok(())
}

/// Syncs a directory's entries. Best effort: not every platform can open a
/// directory as a file.
pub(crate) fn sync_dir(dir: &Path) {
    if let Ok(f) = File::open(dir) {
        let _ = f.sync_all();
    }
}

/// Syncs every file and directory under `dir`, so a rename that commits it
/// never exposes unwritten data after a power loss.
pub(crate) fn sync_tree(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(io_err(dir))? {
        let entry = entry.map_err(io_err(dir))?;
        let path = entry.path();
        let ty = entry.file_type().map_err(io_err(&path))?;
        if ty.is_dir() {
            sync_tree(&path)?;
        } else if ty.is_file() {
            File::open(&path)
                .and_then(|f| f.sync_all())
                .map_err(io_err(&path))?;
        }
    }
    sync_dir(dir);
    Ok(())
}

/// The bytes used by everything under `path` (symlinks counted, not
/// followed).
pub(crate) fn tree_size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    fs::read_dir(path).map_or(0, |rd| {
        rd.filter_map(io::Result::ok)
            .map(|e| tree_size(&e.path()))
            .sum()
    })
}

/// Hashes every file under `dir`, by path relative to it.
pub(crate) fn hash_files(dir: &Path) -> Result<Vec<FileHash>> {
    let mut out = Vec::new();
    if dir.exists() {
        hash_into(dir, "", &mut out)?;
    }
    out.sort();
    Ok(out)
}

fn hash_into(dir: &Path, prefix: &str, out: &mut Vec<FileHash>) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(io_err(dir))? {
        let entry = entry.map_err(io_err(dir))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let ty = entry.file_type().map_err(io_err(&path))?;
        if ty.is_dir() {
            hash_into(&path, &rel, out)?;
        } else {
            let mut h = blake3::Hasher::new();
            if ty.is_symlink() {
                let target = fs::read_link(&path).map_err(io_err(&path))?;
                h.update(b"symlink:");
                h.update(target.to_string_lossy().as_bytes());
            } else {
                let mut f = File::open(&path).map_err(io_err(&path))?;
                let mut buf = vec![0u8; 1 << 16];
                loop {
                    let n = f.read(&mut buf).map_err(io_err(&path))?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                }
            }
            out.push(FileHash {
                path: rel,
                hash: *h.finalize().as_bytes(),
            });
        }
    }
    Ok(())
}

/// Escapes an asset name into one file name: bytes outside
/// `[A-Za-z0-9._-]` become `%XX`, so `s07/blender` is `s07%2Fblender`.
pub(crate) fn escape_name(name: &str) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(name.len());
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-') {
            s.push(char::from(b));
        } else {
            let _ = write!(s, "%{b:02X}");
        }
    }
    if s.starts_with('.') {
        s.replace_range(0..1, "%2E");
    }
    s
}
