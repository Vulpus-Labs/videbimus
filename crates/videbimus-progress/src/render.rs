//! Drawing a [`Snapshot`] as text: the live view's frame, and what
//! `videbimus status` prints.

use std::fmt::Write as _;

use crate::tree::{NodeView, RunOutcome, Snapshot, State};

/// Lines of a failure's output shown under it.
pub const FAILURE_LINES: usize = 5;

/// `snapshot` as lines of at most `width` characters (ANSI colour codes,
/// with `color`, not counted): a two-line header (run state, profile,
/// machine; budget in use, elapsed, ETA), then the tree, one line per node
/// with its state, progress, elapsed time, ETA and machine. Cached groups
/// are one line. A failed task is followed by its last lines of output
/// and its log; a blocked one names what it waits on.
#[must_use]
pub fn render(s: &Snapshot, width: usize, color: bool) -> String {
    let width = width.max(20);
    let mut lines: Vec<Line> = Vec::new();

    let status = match (s.outcome, s.stopping) {
        (Some(o), _) => format!("{} in {}", o.word(), duration(s.elapsed_ms)),
        (None, true) => "stopping".to_owned(),
        (None, false) => "building".to_owned(),
    };
    let targets = if s.run.targets.is_empty() {
        "everything".to_owned()
    } else {
        s.run.targets.join(", ")
    };
    lines.push(Line::plain(format!(
        "{status} · profile {} (nice {}) · {} · {targets}",
        s.run.profile, s.run.niceness, s.run.machine
    )));
    let mut h2 = format!(
        "cores {}/{} · memory {}/{}",
        s.budget.cores_in_use,
        s.budget.cores_limit,
        bytes(s.budget.memory_in_use),
        bytes(s.budget.memory_limit)
    );
    if s.outcome.is_none() {
        let _ = write!(h2, " · elapsed {}", duration(s.elapsed_ms));
    }
    if let Some(eta) = s.eta_s {
        let _ = write!(h2, " · eta {}", eta_text(eta));
    }
    lines.push(Line::plain(h2));

    let name_w = name_width(&s.nodes, 0).clamp(12, (width / 2).max(12));
    for n in &s.nodes {
        node_lines(n, 0, name_w, s, &mut lines);
    }
    for w in &s.warnings {
        lines.push(Line::plain(format!("warning: {w}")));
    }

    let mut out = String::new();
    for l in lines {
        out.push_str(&l.draw(width, color));
        out.push('\n');
    }
    out
}

/// A line, and where its state word is, to colour it after truncation.
struct Line {
    text: String,
    state: Option<(usize, State)>,
}

impl Line {
    fn plain(text: String) -> Self {
        Self { text, state: None }
    }

    fn draw(&self, width: usize, color: bool) -> String {
        let chars: Vec<char> = self.text.chars().collect();
        let mut text: String = if chars.len() > width {
            let mut t: String = chars[..width - 1].iter().collect();
            t.push('…');
            t
        } else {
            self.text.clone()
        };
        text.truncate(text.trim_end().len());
        let Some((at, state)) = self.state.filter(|_| color) else {
            return text;
        };
        let word = state.word();
        let chars: Vec<char> = text.chars().collect();
        if at + word.chars().count() > chars.len() {
            return text;
        }
        let before: String = chars[..at].iter().collect();
        let after: String = chars[at + word.chars().count()..].iter().collect();
        format!("{before}\x1b[{}m{word}\x1b[0m{after}", ansi(state))
    }
}

fn ansi(s: State) -> &'static str {
    match s {
        State::Running => "33",
        State::Queued | State::Waiting => "36",
        State::Done => "32",
        State::Cached | State::NotNeeded => "2",
        State::Failed => "1;31",
        State::Blocked | State::Cancelled => "35",
    }
}

fn name_width(nodes: &[NodeView], depth: usize) -> usize {
    nodes
        .iter()
        .map(|n| (depth * 2 + n.label.chars().count() + 2).max(name_width(&n.children, depth + 1)))
        .max()
        .unwrap_or(0)
}

fn node_lines(n: &NodeView, depth: usize, name_w: usize, s: &Snapshot, out: &mut Vec<Line>) {
    let indent = depth * 2;
    let mut text = format!("{:indent$}{}", "", n.label);
    let shown = text.chars().count();
    if shown + 1 > name_w {
        text = text.chars().take(name_w.saturating_sub(2)).collect();
        text.push('…');
    }
    let pad = name_w.saturating_sub(text.chars().count());
    text.push_str(&" ".repeat(pad));
    let state_at = text.chars().count();
    let _ = write!(text, "{:<10}", n.state.word());

    let progress = if n.collapsed {
        format!(
            "({} asset{})",
            n.total_assets,
            if n.total_assets == 1 { "" } else { "s" }
        )
    } else if let Some(by) = &n.blocked_by {
        format!("by {by}")
    } else if n.total_frames > 0 {
        format!("{}/{} f", n.done_frames, n.total_frames)
    } else if !n.task {
        format!("{}/{}", n.done_assets, n.total_assets)
    } else {
        String::new()
    };
    if n.blocked_by.is_some() || n.collapsed {
        let _ = write!(text, "{progress}");
    } else {
        let _ = write!(text, "{progress:>13}");
        let elapsed = n.elapsed_ms.map(duration).unwrap_or_default();
        let _ = write!(text, " {elapsed:>7}");
        let eta = match n.eta_s {
            Some(e) if s.outcome.is_none() => format!("eta {}", eta_text(e)),
            _ => String::new(),
        };
        let _ = write!(text, " {eta:>10}");
        if let Some(m) = &n.machine {
            let _ = write!(text, "  {m}");
        }
    }
    out.push(Line {
        text,
        state: Some((state_at, n.state)),
    });

    if n.task
        && n.state == State::Failed
        && let Some(f) = s.failures.iter().find(|f| f.name == n.path)
    {
        let pad = " ".repeat(indent + 2);
        let shown = if f.last_lines.is_empty() {
            vec![f.message.clone()]
        } else {
            let from = f.last_lines.len().saturating_sub(FAILURE_LINES);
            let mut v = vec![f.message.clone()];
            v.extend(f.last_lines[from..].iter().cloned());
            v
        };
        for l in shown {
            out.push(Line::plain(format!("{pad}| {l}")));
        }
        if let Some(log) = f.log.as_ref().or(n.log.as_ref()) {
            out.push(Line::plain(format!("{pad}log: {}", log.display())));
        }
    }
    for c in &n.children {
        node_lines(c, depth + 1, name_w, s, out);
    }
}

/// `42s`, `3m05s`, `2h07m`.
#[must_use]
pub fn duration(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, s % 3600 / 60)
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn eta_text(s: f64) -> String {
    if !s.is_finite() {
        return "?".to_owned();
    }
    duration((s.max(0.0) * 1000.0).round() as u64)
}

/// `512M`, `6.0G`.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn bytes(b: u64) -> String {
    const GIB: u64 = 1 << 30;
    if b >= GIB {
        format!("{:.1}G", b as f64 / GIB as f64)
    } else {
        format!("{}M", b >> 20)
    }
}

/// How a finished run reads in one phrase: `complete`, `failed`, …
#[must_use]
pub fn outcome_word(o: Option<RunOutcome>) -> &'static str {
    o.map_or("interrupted (it ended without finishing)", RunOutcome::word)
}
