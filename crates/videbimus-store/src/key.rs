//! Asset keys from labelled inputs (ADR 0003 §2), and the difference
//! between two sets of inputs: why an asset rebuilds (ADR 0003 §7).
//!
//! A key hashes a list of inputs, each with a class, a label and a digest
//! of its content. The list is sorted by class then label before hashing,
//! so the order in which a recipe adds its inputs never matters; labels do.
//! A recipe whose inputs are ordered (segments to join, say) says so in the
//! labels (`segment/000`, `segment/001`): swapping two inputs' keys between
//! labels changes the key. Labels are part of the key, so they must be
//! portable: never an absolute path, or keys would differ between machines
//! (ADR 0006).

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use videbimus_core::AssetKey;

use crate::error::{Error, Result};

/// What an input to a key is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputClass {
    /// The recipe kind (one per key).
    Kind,
    /// The kind's code identity, from its source files (ADR 0009 §5; one
    /// per key).
    Code,
    /// The recipe's own config, as canonical bytes (one per key).
    Config,
    /// Another asset, by its key (or its output identity, for early
    /// cutoff: see [`Store::identity_for_dependants`](crate::Store::identity_for_dependants)).
    Input,
    /// A source file or slice of one (ADR 0003 §3: as narrow as what is
    /// read).
    Source,
    /// A script the recipe runs, hashed per entry point.
    Script,
    /// An external tool and its version.
    Tool,
}

impl InputClass {
    fn tag(self) -> u8 {
        match self {
            InputClass::Kind => 1,
            InputClass::Code => 2,
            InputClass::Config => 3,
            InputClass::Input => 4,
            InputClass::Source => 5,
            InputClass::Script => 6,
            InputClass::Tool => 7,
        }
    }

    fn singleton(self) -> bool {
        matches!(
            self,
            InputClass::Kind | InputClass::Code | InputClass::Config
        )
    }

    fn name(self) -> &'static str {
        match self {
            InputClass::Kind => "kind",
            InputClass::Code => "code",
            InputClass::Config => "config",
            InputClass::Input => "input",
            InputClass::Source => "source",
            InputClass::Script => "script",
            InputClass::Tool => "tool",
        }
    }
}

impl fmt::Display for InputClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One labelled input to a key, as recorded in a manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRecord {
    /// What it is.
    pub class: InputClass,
    /// Its label within the class: a script's or source's portable name,
    /// an input asset's role, a tool's name. Empty for the singletons
    /// (kind, code, config).
    pub label: String,
    /// The digest of its content (an input asset's key as is).
    #[serde(with = "crate::hex::digest")]
    pub digest: [u8; 32],
    /// What to show when it changes, where the digest alone says nothing:
    /// a kind's name, a tool's version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

fn digest(class: InputClass, bytes: &[u8]) -> [u8; 32] {
    let context = match class {
        InputClass::Kind => "videbimus 2026-11-18 key input: kind",
        InputClass::Code => "videbimus 2026-11-18 key input: code",
        InputClass::Config => "videbimus 2026-11-18 key input: config",
        InputClass::Input => "videbimus 2026-11-18 key input: input",
        InputClass::Source => "videbimus 2026-11-18 key input: source",
        InputClass::Script => "videbimus 2026-11-18 key input: script",
        InputClass::Tool => "videbimus 2026-11-18 key input: tool",
    };
    let mut h = blake3::Hasher::new_derive_key(context);
    h.update(bytes);
    *h.finalize().as_bytes()
}

/// Builds an asset's key from labelled inputs.
///
/// ```
/// use videbimus_store::KeyBuilder;
/// # use videbimus_core::AssetKey;
/// # let panel = AssetKey([7; 32]);
/// let inputs = KeyBuilder::new("blender-shot")
///     .code(b"hash of the kind's source files")
///     .config(br#"{"frames":[0,480],"samples":64}"#)
///     .input("panel", panel)
///     .script("hwscene.py", b"import bpy\n")
///     .tool("blender", "5.2.0")
///     .build()
///     .unwrap();
/// let key = inputs.key();
/// ```
///
/// The builder takes canonical bytes and knows nothing of the config
/// language: canonicalising config is the caller's job.
#[derive(Debug)]
#[must_use]
pub struct KeyBuilder {
    entries: Vec<InputRecord>,
    error: Option<Error>,
}

impl KeyBuilder {
    /// Starts a key for a recipe of `kind`.
    pub fn new(kind: &str) -> Self {
        let mut b = Self {
            entries: Vec::new(),
            error: None,
        };
        b.put(
            InputClass::Kind,
            String::new(),
            digest(InputClass::Kind, kind.as_bytes()),
            Some(kind.to_owned()),
        );
        b
    }

    /// Sets the kind's code identity (ADR 0009 §5): the hash of its
    /// declared source files and `VERSION`, from the build script.
    pub fn code(mut self, identity: impl AsRef<[u8]>) -> Self {
        let d = digest(InputClass::Code, identity.as_ref());
        self.put(InputClass::Code, String::new(), d, None);
        self
    }

    /// Sets the recipe's own config, as canonical bytes.
    pub fn config(mut self, canonical: impl AsRef<[u8]>) -> Self {
        let d = digest(InputClass::Config, canonical.as_ref());
        self.put(InputClass::Config, String::new(), d, None);
        self
    }

    /// Adds another asset as an input, under the role `label`.
    pub fn input(mut self, label: &str, key: AssetKey) -> Self {
        self.put(InputClass::Input, label.to_owned(), key.0, None);
        self
    }

    /// Adds source data (a file, or the slice of one the recipe reads).
    pub fn source(mut self, label: &str, bytes: impl AsRef<[u8]>) -> Self {
        let d = digest(InputClass::Source, bytes.as_ref());
        self.put(InputClass::Source, label.to_owned(), d, None);
        self
    }

    /// Adds a source file's contents, read from `path`, under the portable
    /// `label`.
    pub fn source_file(self, label: &str, path: impl AsRef<Path>) -> Self {
        self.file(InputClass::Source, label, path.as_ref())
    }

    /// Adds a script's contents, under the portable `label` (its name as
    /// the user knows it, e.g. `hwscene.py`).
    pub fn script(mut self, label: &str, bytes: impl AsRef<[u8]>) -> Self {
        let d = digest(InputClass::Script, bytes.as_ref());
        self.put(InputClass::Script, label.to_owned(), d, None);
        self
    }

    /// Adds a script's contents, read from `path`, under the portable
    /// `label`.
    pub fn script_file(self, label: &str, path: impl AsRef<Path>) -> Self {
        self.file(InputClass::Script, label, path.as_ref())
    }

    /// Adds an external tool and its version.
    pub fn tool(mut self, name: &str, version: &str) -> Self {
        let d = digest(InputClass::Tool, version.as_bytes());
        self.put(
            InputClass::Tool,
            name.to_owned(),
            d,
            Some(version.to_owned()),
        );
        self
    }

    fn file(mut self, class: InputClass, label: &str, path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => {
                let d = digest(class, &bytes);
                self.put(class, label.to_owned(), d, None);
            }
            Err(source) => {
                self.error.get_or_insert(Error::InputFile {
                    label: label.to_owned(),
                    path: path.to_owned(),
                    source,
                });
            }
        }
        self
    }

    fn put(&mut self, class: InputClass, label: String, digest: [u8; 32], detail: Option<String>) {
        let rec = InputRecord {
            class,
            label,
            digest,
            detail,
        };
        if class.singleton() {
            if let Some(e) = self.entries.iter_mut().find(|e| e.class == class) {
                *e = rec;
                return;
            }
        } else if rec.label.is_empty() {
            self.error.get_or_insert(Error::EmptyLabel { class });
            return;
        } else if self
            .entries
            .iter()
            .any(|e| e.class == class && e.label == rec.label)
        {
            self.error.get_or_insert(Error::DuplicateInput {
                class,
                label: rec.label,
            });
            return;
        }
        self.entries.push(rec);
    }

    /// The inputs, ready to key.
    ///
    /// # Errors
    ///
    /// The first problem met while adding inputs: a file that couldn't be
    /// read, an empty label, or a label given twice in one class.
    pub fn build(self) -> Result<KeyInputs> {
        if let Some(e) = self.error {
            return Err(e);
        }
        let mut entries = self.entries;
        entries.sort_by(|a, b| (a.class, &a.label).cmp(&(b.class, &b.label)));
        Ok(KeyInputs { entries })
    }
}

/// A key's labelled inputs, sorted. Hashes to the key, and is recorded in
/// the manifest so a later build can say what changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInputs {
    entries: Vec<InputRecord>,
}

impl KeyInputs {
    /// The key.
    #[must_use]
    pub fn key(&self) -> AssetKey {
        let mut h = blake3::Hasher::new_derive_key("videbimus 2026-11-18 asset key v1");
        for e in &self.entries {
            h.update(&[e.class.tag()]);
            h.update(&(e.label.len() as u64).to_le_bytes());
            h.update(e.label.as_bytes());
            h.update(&e.digest);
        }
        AssetKey(*h.finalize().as_bytes())
    }

    /// The inputs, sorted by class then label.
    #[must_use]
    pub fn records(&self) -> &[InputRecord] {
        &self.entries
    }

    /// The recipe kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        self.entries
            .iter()
            .find(|e| e.class == InputClass::Kind)
            .and_then(|e| e.detail.as_deref())
            .unwrap_or_default()
    }

    /// What changed from `old` (an earlier manifest's inputs) to these.
    #[must_use]
    pub fn changes_since(&self, old: &[InputRecord]) -> Vec<Change> {
        diff(old, &self.entries)
    }
}

/// How one input changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "change")]
pub enum ChangeKind {
    /// Present before and now, with different content.
    Changed {
        /// The old detail (a tool's version), if it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        /// The new detail.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
    },
    /// New since the last build.
    Added,
    /// Gone since the last build.
    Removed,
}

/// One input that differs from the last build: a reason to rebuild.
///
/// Displays as the progress view and logs show it: `script hwscene.py
/// changed`, `input s07/panel changed`, `tool blender 5.2.0 → 5.2.1`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// The input's class.
    pub class: InputClass,
    /// The input's label.
    pub label: String,
    /// How it changed.
    #[serde(flatten)]
    pub what: ChangeKind,
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match &self.what {
            ChangeKind::Changed {
                from: Some(a),
                to: Some(b),
            } => {
                return if self.label.is_empty() {
                    write!(f, "{} {a} → {b}", self.class)
                } else {
                    write!(f, "{} {} {a} → {b}", self.class, self.label)
                };
            }
            ChangeKind::Changed { .. } => "changed",
            ChangeKind::Added => "added",
            ChangeKind::Removed => "removed",
        };
        if self.label.is_empty() {
            write!(f, "{} {what}", self.class)
        } else {
            write!(f, "{} {} {what}", self.class, self.label)
        }
    }
}

fn diff(old: &[InputRecord], new: &[InputRecord]) -> Vec<Change> {
    let id = |r: &InputRecord| (r.class, r.label.clone());
    let mut out = Vec::new();
    for n in new {
        match old.iter().find(|o| id(o) == id(n)) {
            Some(o) if o.digest == n.digest => {}
            Some(o) => out.push(Change {
                class: n.class,
                label: n.label.clone(),
                what: ChangeKind::Changed {
                    from: o.detail.clone(),
                    to: n.detail.clone(),
                },
            }),
            None => out.push(Change {
                class: n.class,
                label: n.label.clone(),
                what: ChangeKind::Added,
            }),
        }
    }
    for o in old {
        if !new.iter().any(|n| id(n) == id(o)) {
            out.push(Change {
                class: o.class,
                label: o.label.clone(),
                what: ChangeKind::Removed,
            });
        }
    }
    out.sort_by(|a, b| (a.class, &a.label).cmp(&(b.class, &b.label)));
    out
}

/// Why an asset is being built, against the last key built under its name
/// (ADR 0003 §7, ADR 0007 §1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "why")]
pub enum Why {
    /// Nothing has been built under this name before.
    New,
    /// The key is the name's latest: a resume, or a retry after a failure.
    Resume,
    /// Inputs changed since the name's previous key.
    Changed {
        /// The previous key.
        #[serde(with = "crate::hex::key")]
        previous: AssetKey,
        /// What changed.
        changes: Vec<Change>,
    },
    /// The name had a previous key, but its manifest is gone (collected),
    /// so what changed can't be said.
    PreviousGone {
        /// The previous key.
        #[serde(with = "crate::hex::key")]
        previous: AssetKey,
    },
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Why::New => f.write_str("first build"),
            Why::Resume => f.write_str("resuming"),
            Why::Changed { changes, .. } if changes.is_empty() => f.write_str("inputs changed"),
            Why::Changed { changes, .. } => {
                for (i, c) in changes.iter().enumerate() {
                    if i > 0 {
                        f.write_str("; ")?;
                    }
                    write!(f, "{c}")?;
                }
                Ok(())
            }
            Why::PreviousGone { previous } => {
                write!(
                    f,
                    "inputs changed (previous build {previous} was collected)"
                )
            }
        }
    }
}
