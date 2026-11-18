//! Measured cost history (ADR 0004 §2), and the load it was measured
//! under.
//!
//! A complete manifest records its build time (`build_ms`, the sum of its
//! chunks' times), peak memory, and the other load seen on the machine
//! while it built (`external_load_pct`). The newest complete build under
//! a task's name whose external load was at most the contention threshold
//! replaces the task's declared seconds and memory. A manifest that never
//! measured its load (built by another tool) counts as uncontended.
//!
//! **Contention.** At each chunk start the scheduler samples the 1-minute
//! load average and subtracts its own share of it: the cores of its
//! running chunks, smoothed with the same one-minute exponential decay the
//! kernel uses, so its own recent work doesn't read as someone else's. The
//! rest is other work. It is a heuristic: tasks that use more threads
//! than they declare look like contention, and short spikes are blurred.

use std::time::Instant;

use videbimus_store::{Shape, Status, Store};

/// A measured cost.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Measured {
    pub total_s: f64,
    pub memory_bytes: Option<u64>,
}

/// The newest uncontended measured cost for `name`, scaled to `shape`'s
/// frame count where both are frame sequences.
pub(crate) fn lookup(
    store: &Store,
    name: &str,
    shape: Shape,
    threshold_pct: u32,
) -> Option<Measured> {
    for key in store.generations(name).ok()? {
        let Ok(Some(m)) = store.manifest(key) else {
            continue;
        };
        if m.status != Status::Complete {
            continue;
        }
        let Some(ms) = m.build_ms else { continue };
        if m.external_load_pct.is_some_and(|p| p > threshold_pct) {
            continue;
        }
        #[allow(clippy::cast_precision_loss)]
        let secs = ms as f64 / 1000.0;
        let total_s = match (m.shape, shape) {
            (Shape::Frames(old), Shape::Frames(new)) if !old.is_empty() => {
                #[allow(clippy::cast_precision_loss)]
                let per = secs / old.len() as f64;
                #[allow(clippy::cast_precision_loss)]
                let n = new.len().max(0) as f64;
                per * n
            }
            _ => secs,
        };
        return Some(Measured {
            total_s,
            memory_bytes: m.peak_memory_bytes,
        });
    }
    None
}

/// The 1-minute load average, where the platform has one.
#[must_use]
pub fn load_average() -> Option<f64> {
    #[cfg(unix)]
    {
        let mut loads = [0.0f64; 3];
        // SAFETY: the buffer holds 3 doubles, as asked for.
        let n = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
        (n >= 1).then_some(loads[0])
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The scheduler's own load, decayed like the kernel's 1-minute average.
#[derive(Debug)]
pub(crate) struct OwnLoad {
    value: f64,
    at: Instant,
}

impl OwnLoad {
    pub fn new() -> Self {
        Self {
            value: 0.0,
            at: Instant::now(),
        }
    }

    /// Advances to now, with `cores` running since the last update, and
    /// returns the smoothed load.
    pub fn update(&mut self, cores: u32) -> f64 {
        let dt = self.at.elapsed().as_secs_f64();
        self.at = Instant::now();
        let decay = (-dt / 60.0).exp();
        self.value = self.value * decay + f64::from(cores) * (1.0 - decay);
        self.value
    }
}
