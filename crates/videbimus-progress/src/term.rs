//! The terminal: whether output is one, and its size.

use std::io::IsTerminal;

/// Size assumed when the terminal's can't be read.
pub const FALLBACK: (usize, usize) = (100, 40);

/// Whether standard output is a terminal, so the live view can redraw in
/// place. `TERM=dumb` counts as not.
#[must_use]
pub fn stdout_is_terminal() -> bool {
    std::io::stdout().is_terminal() && std::env::var("TERM").map_or(true, |t| t != "dumb")
}

/// Standard output's size in columns and rows: the terminal's, else
/// `COLUMNS` and `LINES`, else 100 by 40.
#[must_use]
pub fn size() -> (usize, usize) {
    if let Some(s) = ioctl_size() {
        return s;
    }
    let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<usize>().ok());
    (
        env("COLUMNS").unwrap_or(FALLBACK.0),
        env("LINES").unwrap_or(FALLBACK.1),
    )
}

#[cfg(unix)]
fn ioctl_size() -> Option<(usize, usize)> {
    // SAFETY: winsize is plain data; TIOCGWINSZ fills it or fails.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: the pointer is to a live winsize.
    let r = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &raw mut ws) };
    (r == 0 && ws.ws_col > 0).then(|| (usize::from(ws.ws_col), usize::from(ws.ws_row.max(1))))
}

#[cfg(not(unix))]
fn ioctl_size() -> Option<(usize, usize)> {
    None
}
