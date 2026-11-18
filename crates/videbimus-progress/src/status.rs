//! `videbimus status` (ADR 0007 §3): the running build's tree from the
//! status file, or, when nothing is building, the last run's outcome and
//! whether each asset is current for the config as it stands.

use std::fmt::Write as _;
use std::path::Path;

use videbimus_core::AssetKey;
use videbimus_scheduler::Graph;
use videbimus_store::{LockHolder, Status, Store, machine_name};

use crate::record::{read_status, utc};
use crate::render::{FAILURE_LINES, duration, outcome_word, render};

/// Whether an asset's latest build matches the current config's key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyState {
    /// Built (or begun) under the current key. Progress counts only the
    /// chunks committed under it (ADR 0007 §4).
    Current {
        /// Its status in the store.
        status: Status,
        /// Frames committed under the key (0 for a single output).
        done_frames: u64,
        /// Frames in all (0 for a single output).
        total_frames: u64,
    },
    /// Built before, but not under the current key: the config, code or
    /// an input has changed since.
    Stale {
        /// The latest key built under its name.
        latest: AssetKey,
    },
    /// Never built under its name.
    Missing,
    /// Its key can't be known yet: an input hashes its output (early
    /// cutoff, ADR 0003 §4) and isn't built under its own current key.
    PendingInput {
        /// That input.
        input: String,
    },
    /// Its key couldn't be computed.
    Unknown {
        /// Why.
        error: String,
    },
}

impl KeyState {
    /// One word: `current`, `stale`, `missing`, `pending`, `unknown`.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            KeyState::Current { .. } => "current",
            KeyState::Stale { .. } => "stale",
            KeyState::Missing => "missing",
            KeyState::PendingInput { .. } => "pending",
            KeyState::Unknown { .. } => "unknown",
        }
    }
}

/// An asset's name, current key (if knowable) and [`KeyState`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetStatus {
    /// Its name.
    pub name: String,
    /// Its key under the current config, if it can be known.
    pub key: Option<AssetKey>,
    /// Where it stands.
    pub state: KeyState,
}

/// Each task of `graph` against `store` (`None`: no store yet), in graph
/// order. Keys are computed as the scheduler computes them: the task's own
/// key inputs plus each input's identity. An input that doesn't hash its
/// output is identified by its key, built or not; one that does is
/// identified by its output, so only once complete under its current key:
/// until then its dependants are [`KeyState::PendingInput`].
#[must_use]
pub fn asset_statuses(graph: &Graph, store: Option<&Store>) -> Vec<AssetStatus> {
    let mut identity: Vec<Option<AssetKey>> = vec![None; graph.len()];
    let mut out = Vec::with_capacity(graph.len());
    for id in graph.ids() {
        let task = graph.task(id);
        let name = graph.name(id).to_owned();
        let mut kb = task.key();
        let mut pending = None;
        for (label, input) in graph.inputs(id) {
            if let Some(k) = identity[input.index()] {
                kb = kb.input(label, k);
            } else {
                pending = Some(graph.name(*input).to_owned());
                break;
            }
        }
        if let Some(input) = pending {
            out.push(AssetStatus {
                name,
                key: None,
                state: KeyState::PendingInput { input },
            });
            continue;
        }
        let key = match kb.build() {
            Ok(k) => k.key(),
            Err(e) => {
                out.push(AssetStatus {
                    name,
                    key: None,
                    state: KeyState::Unknown {
                        error: e.to_string(),
                    },
                });
                continue;
            }
        };
        let unhashed = (!task.hash_output()).then_some(key);
        let (state, ident) = match store.map(|s| key_state(s, &name, key)) {
            None => (KeyState::Missing, unhashed),
            Some(Ok((state, complete_identity))) => (state, complete_identity.or(unhashed)),
            Some(Err(e)) => (
                KeyState::Unknown {
                    error: e.to_string(),
                },
                None,
            ),
        };
        identity[id.index()] = ident;
        out.push(AssetStatus {
            name,
            key: Some(key),
            state,
        });
    }
    out
}

fn key_state(
    store: &Store,
    name: &str,
    key: AssetKey,
) -> videbimus_store::Result<(KeyState, Option<AssetKey>)> {
    if let Some(p) = store.progress(key)? {
        let identity = store.identity_for_dependants(key)?;
        return Ok((
            KeyState::Current {
                status: p.status,
                done_frames: p.done_frames,
                total_frames: p.total_frames,
            },
            identity,
        ));
    }
    Ok(match store.latest(name)? {
        Some(latest) => (KeyState::Stale { latest }, None),
        None => (KeyState::Missing, None),
    })
}

/// Who holds `store`'s lock, if a live process does: a build is running.
/// A lock held on another machine is taken as live (it can't be checked).
#[must_use]
pub fn running(store: &Path) -> Option<LockHolder> {
    let bytes = std::fs::read(store.join("lock")).ok()?;
    let holder: LockHolder = serde_json::from_slice(&bytes).ok()?;
    (holder.host != machine_name() || process_alive(holder.pid)).then_some(holder)
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_: u32) -> bool {
    true
}

/// What `videbimus status` prints for the project whose assets are
/// `graph` and whose store is at `store`, `width` columns wide:
///
/// - while a build runs (its lock is held), who runs it and the tree from
///   its status file;
/// - otherwise, that nothing is building, the last run's outcome (with
///   its failures), and each asset against the current config: current
///   (with committed frames), stale, missing, or pending an input.
#[must_use]
pub fn status_report(graph: &Graph, store: &Path, width: usize, now_ms: u64) -> String {
    let mut out = String::new();
    let snapshot = read_status(store);
    if let Some(holder) = running(store) {
        let _ = writeln!(out, "building in {} ({holder})", store.display());
        match snapshot {
            Ok(Some(s)) if s.run.pid == holder.pid => {
                let age = now_ms.saturating_sub(s.at_ms);
                out.push_str(&render(&s, width, false));
                if age > 10_000 {
                    let _ = writeln!(
                        out,
                        "(the status file is {} old: is the build stuck?)",
                        duration(age)
                    );
                }
            }
            Ok(_) => out.push_str("its status file isn't written yet\n"),
            Err(e) => {
                let _ = writeln!(out, "its status file can't be read: {e}");
            }
        }
        return out;
    }

    let _ = writeln!(out, "no build is running in {}", store.display());
    match snapshot {
        Ok(Some(s)) => {
            let ended = s.at_ms;
            let _ = write!(
                out,
                "last run: {}, ended {} ({} ago), profile {}",
                outcome_word(s.outcome),
                utc(ended),
                duration(now_ms.saturating_sub(ended)),
                s.run.profile,
            );
            if s.outcome.is_some() {
                let _ = write!(out, ", took {}", duration(s.elapsed_ms));
            }
            out.push('\n');
            if let Some(log) = &s.run.run_log {
                let _ = writeln!(out, "  events: {}", log.display());
            }
            for f in &s.failures {
                let _ = writeln!(out, "  {} failed: {}", f.name, f.message);
                let from = f.last_lines.len().saturating_sub(FAILURE_LINES);
                for l in &f.last_lines[from..] {
                    let _ = writeln!(out, "    | {l}");
                }
                if let Some(log) = &f.log {
                    let _ = writeln!(out, "    log: {}", log.display());
                }
            }
        }
        Ok(None) => out.push_str("no run recorded yet\n"),
        Err(e) => {
            let _ = writeln!(out, "the last run's status file can't be read: {e}");
        }
    }

    assets_report(graph, store, &mut out);
    out
}

fn assets_report(graph: &Graph, store: &Path, out: &mut String) {
    let store = if store.is_dir() {
        Store::open_read_only(store).ok()
    } else {
        None
    };
    let statuses = asset_statuses(graph, store.as_ref());
    if statuses.is_empty() {
        out.push_str("the config declares no assets\n");
        return;
    }
    out.push_str("assets against the current config:\n");
    let w = statuses.iter().map(|s| s.name.len()).max().unwrap_or(0);
    for a in &statuses {
        let detail = match &a.state {
            KeyState::Current {
                status,
                done_frames,
                total_frames,
            } => {
                let frames = if *total_frames > 0 {
                    format!("{done_frames}/{total_frames} frames")
                } else {
                    String::new()
                };
                let what = match status {
                    Status::Complete => "built".to_owned(),
                    Status::Building => "partly built; building again resumes".to_owned(),
                    Status::Failed { message } => format!("failed: {message}"),
                };
                [frames, what]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
            KeyState::Stale { latest } => format!(
                "its latest build ({latest}) isn't the config's key ({}); building rebuilds it",
                a.key.map(|k| k.to_string()).unwrap_or_default()
            ),
            KeyState::Missing => "never built".to_owned(),
            KeyState::PendingInput { input } => {
                format!("input `{input}`: its key is known once that is built")
            }
            KeyState::Unknown { error } => error.clone(),
        };
        let sep = if matches!(a.state, KeyState::PendingInput { .. }) {
            " "
        } else {
            "  "
        };
        let _ = writeln!(out, "  {:w$}  {:<7}{sep}{detail}", a.name, a.state.word());
    }
}
