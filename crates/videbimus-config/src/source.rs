//! Source files, locations in them, and how files are read.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// A config file's text, kept so that errors can quote it.
pub struct SourceFile {
    path: PathBuf,
    text: String,
    line_starts: Vec<usize>,
}

impl SourceFile {
    pub(crate) fn new(path: PathBuf, text: String) -> Self {
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        Self {
            path,
            text,
            line_starts,
        }
    }

    /// The path the file was read from, as written (joined onto the
    /// including file's directory for includes).
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The 0-based line holding byte `offset`.
    fn line_index(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }

    /// The text of 0-based line `i`, without its newline.
    fn line(&self, i: usize) -> &str {
        let start = self.line_starts[i];
        let end = self
            .line_starts
            .get(i + 1)
            .map_or(self.text.len(), |&e| e - 1);
        self.text[start..end].trim_end_matches('\r')
    }
}

impl fmt::Debug for SourceFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceFile")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// A span of a config file: where a node, value or part of an expression
/// was written. Displays as `path:line:column`, 1-based, columns in
/// characters.
#[derive(Clone)]
pub struct Location {
    file: Arc<SourceFile>,
    offset: usize,
    len: usize,
}

impl Location {
    pub(crate) fn new(file: &Arc<SourceFile>, offset: usize, len: usize) -> Self {
        Self {
            file: Arc::clone(file),
            offset,
            len,
        }
    }

    /// The same file, another span.
    pub(crate) fn with_span(&self, offset: usize, len: usize) -> Self {
        Self::new(&self.file, offset, len)
    }

    /// The file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    /// The 1-based line.
    #[must_use]
    pub fn line(&self) -> usize {
        self.file.line_index(self.offset) + 1
    }

    /// The 1-based column, in characters.
    #[must_use]
    pub fn column(&self) -> usize {
        let i = self.file.line_index(self.offset);
        let start = self.file.line_starts[i];
        self.file.text[start..self.offset].chars().count() + 1
    }

    /// The text this location covers.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.file.text[self.offset..self.offset + self.len]
    }

    /// The source line, and a caret line underlining this span on it.
    pub(crate) fn snippet(&self) -> (usize, String, String) {
        let i = self.file.line_index(self.offset);
        let line = self.file.line(i);
        let start = self.file.line_starts[i];
        let lead: String = self.file.text[start..self.offset]
            .chars()
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect();
        let on_line = (start + line.len()).saturating_sub(self.offset);
        let width = self.file.text[self.offset..self.offset + self.len.min(on_line)]
            .chars()
            .count()
            .max(1);
        (
            i + 1,
            line.to_owned(),
            format!("{lead}{}", "^".repeat(width)),
        )
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.path().display(),
            self.line(),
            self.column()
        )
    }
}

impl fmt::Debug for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Reads config files: the root and its includes.
pub trait Loader {
    /// The text of the file at `path`.
    ///
    /// # Errors
    ///
    /// If the file can't be read.
    fn load(&self, path: &Path) -> io::Result<String>;
}

/// Reads files from the filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct FsLoader;

impl Loader for FsLoader {
    fn load(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
}

/// Serves files from memory: for tests, and for configs embedded in a
/// binary.
#[derive(Clone, Debug, Default)]
pub struct MemLoader {
    files: HashMap<PathBuf, String>,
}

impl MemLoader {
    /// No files.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a file at `path`.
    #[must_use]
    pub fn with(mut self, path: impl AsRef<Path>, text: impl Into<String>) -> Self {
        self.insert(path, text);
        self
    }

    /// Adds or replaces a file at `path`.
    pub fn insert(&mut self, path: impl AsRef<Path>, text: impl Into<String>) {
        self.files.insert(normalize(path.as_ref()), text.into());
    }
}

impl Loader for MemLoader {
    fn load(&self, path: &Path) -> io::Result<String> {
        self.files
            .get(&normalize(path))
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file in the MemLoader"))
    }
}

/// `a/./b/../c` → `a/c`, without touching the filesystem.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}
