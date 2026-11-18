//! Budget profiles (ADR 0004 §3) and what the machine has.

use serde::Serialize;

/// A machine's size: what profiles are fractions of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Machine {
    /// Its host name.
    pub name: String,
    /// Logical cores.
    pub cores: u32,
    /// Physical memory, in bytes (0 if unknown).
    pub memory_bytes: u64,
}

impl Machine {
    /// This machine.
    #[must_use]
    pub fn detect() -> Self {
        let cores = std::thread::available_parallelism()
            .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX));
        Self {
            name: videbimus_store::machine_name(),
            cores,
            memory_bytes: physical_memory(),
        }
    }
}

#[cfg(unix)]
fn physical_memory() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let (pages, size) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    match (u64::try_from(pages), u64::try_from(size)) {
        (Ok(p), Ok(s)) => p.saturating_mul(s),
        _ => 0,
    }
}

#[cfg(not(unix))]
fn physical_memory() -> u64 {
    0
}

/// A budget: how much of the machine a build may use, and how politely
/// (ADR 0004 §3). Running tasks' declared cores and memory are packed to
/// stay within it; subprocesses get its niceness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Profile {
    /// `interactive`, `background`, `overnight`, or a custom name.
    pub name: String,
    /// The core budget.
    pub cores: u32,
    /// The memory budget, in bytes.
    pub memory_bytes: u64,
    /// The niceness subprocesses run at (0 normal, 19 lowest; Unix only).
    pub niceness: i32,
}

/// Memory assumed when the machine's can't be read: 8 GiB.
const FALLBACK_MEMORY: u64 = 8 << 30;

impl Profile {
    /// A custom profile.
    #[must_use]
    pub fn new(name: &str, cores: u32, memory_bytes: u64, niceness: i32) -> Self {
        Self {
            name: name.to_owned(),
            cores: cores.max(1),
            memory_bytes,
            niceness,
        }
    }

    fn fraction(name: &str, m: &Machine, cores: u32, mem_pct: u64, niceness: i32) -> Self {
        let mem = if m.memory_bytes == 0 {
            FALLBACK_MEMORY
        } else {
            m.memory_bytes
        };
        Self::new(name, cores, mem / 100 * mem_pct, niceness)
    }

    /// You are waiting on the build (a preview, say): all cores but one,
    /// 60% of memory, slightly nice (5) so the desktop stays responsive.
    #[must_use]
    pub fn interactive(m: &Machine) -> Self {
        Self::fraction("interactive", m, m.cores.saturating_sub(1), 60, 5)
    }

    /// You are working while it builds: half the cores, 40% of memory,
    /// nice 15.
    #[must_use]
    pub fn background(m: &Machine) -> Self {
        Self::fraction("background", m, m.cores / 2, 40, 15)
    }

    /// Nobody is at the machine: every core, 75% of memory (the rest for
    /// the system and the page cache), nice 10 so a late login still
    /// works.
    #[must_use]
    pub fn overnight(m: &Machine) -> Self {
        Self::fraction("overnight", m, m.cores, 75, 10)
    }

    /// The built-in profile called `name`.
    #[must_use]
    pub fn named(name: &str, m: &Machine) -> Option<Self> {
        match name {
            "interactive" => Some(Self::interactive(m)),
            "background" => Some(Self::background(m)),
            "overnight" => Some(Self::overnight(m)),
            _ => None,
        }
    }
}
