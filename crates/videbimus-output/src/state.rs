//! Scene state persisted with each segment (ADR 0008 §4).
//!
//! After rendering its frames, a segment saves the scene's state
//! ([`Scene::save_state`](videbimus_runtime::Scene::save_state)) to
//! `state.bin` beside its video. The next segment, if that file exists
//! and matches, loads it into a fresh scene and renders its first frame by
//! continuing, rather than catching stateful components up from frame 0.
//! Components that can't save their state, or whose state didn't load,
//! are caught up as usual, so the frames are the same either way.
//!
//! The file holds a *state key*: a hash of the scene's config and the code
//! identities of the kinds in it. A state is used only if its key and
//! frame match. Asset content isn't in the state key (it isn't known when
//! a segment starts), so components whose state depends on what they read
//! must not save it (see
//! [`Component::save_state`](videbimus_core::Component::save_state)).

use std::path::Path;

use videbimus_store::{ChunkId, Store};

const MAGIC: &[u8; 16] = b"videbimus-state1";

/// The file a segment's state is saved to, in its output.
pub const STATE_FILE: &str = "state.bin";

/// A scene's state at a frame, as saved to a segment's output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedState {
    /// The scene's state key: config and code.
    pub key: [u8; 32],
    /// The frame the state is ready to render: the segment's end.
    pub frame: i64,
    /// [`Scene::save_state`](videbimus_runtime::Scene::save_state)'s bytes.
    pub bytes: Vec<u8>,
}

impl SavedState {
    /// As a file's bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(56 + self.bytes.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.key);
        out.extend_from_slice(&self.frame.to_le_bytes());
        out.extend_from_slice(&self.bytes);
        out
    }

    /// From a file's bytes, if they are one.
    #[must_use]
    pub fn decode(b: &[u8]) -> Option<Self> {
        let rest = b.strip_prefix(MAGIC.as_slice())?;
        if rest.len() < 40 {
            return None;
        }
        let (key, rest) = rest.split_at(32);
        let (frame, bytes) = rest.split_at(8);
        Some(Self {
            key: key.try_into().ok()?,
            frame: i64::from_le_bytes(frame.try_into().ok()?),
            bytes: bytes.to_vec(),
        })
    }
}

/// The state key of a scene: its canonical config and its kinds' code
/// identities.
#[must_use]
pub fn state_key(scene_identity: &[u8], kinds: &[(String, Vec<u8>)]) -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key("videbimus 2026-11-18 output scene state");
    h.update(&(scene_identity.len() as u64).to_le_bytes());
    h.update(scene_identity);
    for (k, code) in kinds {
        h.update(&(k.len() as u64).to_le_bytes());
        h.update(k.as_bytes());
        h.update(&(code.len() as u64).to_le_bytes());
        h.update(code);
    }
    *h.finalize().as_bytes()
}

/// Why no saved state was used, for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoState {
    /// The previous segment has no committed build.
    NotBuilt,
    /// It saved none.
    NoneSaved,
    /// It saved one for another config or code, or another frame.
    Stale,
}

impl std::fmt::Display for NoState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            NoState::NotBuilt => "the previous segment isn't built",
            NoState::NoneSaved => "the previous segment saved no state",
            NoState::Stale => "the previous segment's state is for another config or code",
        })
    }
}

/// The state saved by the latest build of the segment named `prev`, if it
/// is ready to render `frame` of a scene with state key `key`.
///
/// # Errors
///
/// Why there is none.
pub fn load_previous(
    store: &Store,
    prev: &str,
    frame: i64,
    key: &[u8; 32],
) -> Result<SavedState, NoState> {
    let k = store.latest(prev).ok().flatten().ok_or(NoState::NotBuilt)?;
    let committed = store.committed(k).unwrap_or_default();
    if !committed.iter().any(|c| c.id() == ChunkId::Whole) {
        return Err(NoState::NotBuilt);
    }
    let path = store.chunk_dir(k, ChunkId::Whole).join(STATE_FILE);
    let saved = read(&path).ok_or(NoState::NoneSaved)?;
    if saved.key != *key || saved.frame != frame {
        return Err(NoState::Stale);
    }
    Ok(saved)
}

fn read(path: &Path) -> Option<SavedState> {
    SavedState::decode(&std::fs::read(path).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = SavedState {
            key: [7; 32],
            frame: 120,
            bytes: vec![1, 2, 3],
        };
        assert_eq!(SavedState::decode(&s.encode()), Some(s));
        assert_eq!(SavedState::decode(b"nope"), None);
    }

    #[test]
    fn keys_follow_config_and_code() {
        let kinds = vec![("walker".to_owned(), b"a".to_vec())];
        let a = state_key(b"cfg", &kinds);
        assert_eq!(a, state_key(b"cfg", &kinds));
        assert_ne!(a, state_key(b"cfg2", &kinds));
        assert_ne!(
            a,
            state_key(b"cfg", &[("walker".to_owned(), b"b".to_vec())])
        );
    }
}
