//! Recording a run (ADR 0007 §1, §3): its event log, its status file and
//! its view in the terminal.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use videbimus_scheduler::{Event, EventSink};

use crate::render::render;
use crate::term;
use crate::tree::{Snapshot, Tree};

/// The directory of run event logs, under the store.
pub const RUNS: &str = "runs";

/// The file in [`RUNS`] naming the newest run's log.
pub const LATEST: &str = "latest";

/// The status file, under the store.
pub const STATUS: &str = "status.json";

/// The run event logs of `store`.
#[must_use]
pub fn runs_dir(store: &Path) -> PathBuf {
    store.join(RUNS)
}

/// The status file of `store`.
#[must_use]
pub fn status_path(store: &Path) -> PathBuf {
    store.join(STATUS)
}

/// The newest run's event log in `store`, if there is one.
#[must_use]
pub fn latest_run(store: &Path) -> Option<PathBuf> {
    let dir = runs_dir(store);
    let name = fs::read_to_string(dir.join(LATEST)).ok()?;
    let path = dir.join(name.trim());
    path.is_file().then_some(path)
}

/// The status file of `store`, if there is one.
///
/// # Errors
///
/// I/O other than a missing file, or a file that isn't a status file.
pub fn read_status(store: &Path) -> io::Result<Option<Snapshot>> {
    match fs::read(status_path(store)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Writes `bytes` to `dest` atomically: staged beside it, synced, renamed
/// over, so a reader sees the old file or the new, never half of one.
///
/// # Errors
///
/// I/O.
pub fn replace_atomic(dest: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = dest
        .file_name()
        .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let staged = dest.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = File::create(&staged)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&staged, dest)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result
}

/// How a run shows in the terminal.
pub enum View {
    /// Nothing.
    Quiet,
    /// A line per event, from this function (for output that isn't a
    /// terminal).
    Plain(EventLine),
    /// The tree, redrawn in place with ANSI escapes.
    Live {
        /// Where to draw.
        out: Box<dyn Write + Send>,
        /// The terminal's size, in columns and rows, asked at each redraw.
        size: Box<dyn Fn() -> (usize, usize) + Send>,
        /// Whether to colour states.
        color: bool,
    },
}

impl View {
    /// The live tree on standard output, sized to the terminal.
    #[must_use]
    pub fn live_stdout() -> Self {
        View::Live {
            out: Box::new(io::stdout()),
            size: Box::new(term::size),
            color: true,
        }
    }
}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            View::Quiet => "Quiet",
            View::Plain(_) => "Plain",
            View::Live { .. } => "Live",
        })
    }
}

/// How often a [`Recorder`] redraws and rewrites.
#[derive(Clone, Copy, Debug)]
pub struct RecorderOptions {
    /// The least time between redraws of the live view (a few a second).
    pub redraw: Duration,
    /// How often the status file is rewritten (every second or two).
    pub status: Duration,
    /// Run logs kept in the store: the newest.
    pub keep_runs: usize,
}

impl Default for RecorderOptions {
    fn default() -> Self {
        Self {
            redraw: Duration::from_millis(250),
            status: Duration::from_secs(1),
            keep_runs: 50,
        }
    }
}

/// A function shown each event, for [`View::Plain`].
pub type EventLine = Box<dyn Fn(&Event) + Send + Sync>;

struct Inner {
    tree: Tree,
    log: Option<File>,
    status: PathBuf,
    plain: Option<EventLine>,
    live: Option<Live>,
    dirty: bool,
    last_status_ms: u64,
    error: Option<String>,
}

struct Live {
    out: Box<dyn Write + Send>,
    size: Box<dyn Fn() -> (usize, usize) + Send>,
    color: bool,
    drawn: usize,
    last_ms: u64,
}

struct Shared {
    inner: Mutex<Inner>,
    done: Mutex<bool>,
    wake: Condvar,
    options: RecorderOptions,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Records one run: appends every scheduler event, as a JSON line, to
/// `<store>/runs/<UTC time>-<pid>.jsonl` (and names it in
/// `<store>/runs/latest`); folds the events into the progress [`Tree`];
/// rewrites `<store>/status.json` atomically every
/// [`RecorderOptions::status`]; and shows the run through its [`View`].
///
/// Give [`sink`](Self::sink) to the scheduler, and call
/// [`finish`](Self::finish) when it returns.
pub struct Recorder {
    shared: Arc<Shared>,
    ticker: Option<JoinHandle<()>>,
    run_log: PathBuf,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("run_log", &self.run_log)
            .finish_non_exhaustive()
    }
}

impl Recorder {
    /// Starts recording a run on the store at `store`, with the default
    /// options.
    ///
    /// # Errors
    ///
    /// If the run log can't be created.
    pub fn start(store: &Path, view: View) -> io::Result<Self> {
        Self::with_options(store, view, RecorderOptions::default())
    }

    /// As [`start`](Self::start), with `options`.
    ///
    /// # Errors
    ///
    /// As [`start`](Self::start).
    pub fn with_options(store: &Path, view: View, options: RecorderOptions) -> io::Result<Self> {
        let dir = runs_dir(store);
        fs::create_dir_all(&dir)?;
        let now = crate::now_ms();
        let name = format!("{}-{}.jsonl", compact_utc(now), std::process::id());
        let run_log = dir.join(&name);
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&run_log)?;
        replace_atomic(&dir.join(LATEST), name.as_bytes())?;
        prune(&dir, options.keep_runs);

        let mut tree = Tree::new();
        tree.set_run_log(run_log.clone());
        let (plain, live) = match view {
            View::Quiet => (None, None),
            View::Plain(f) => (Some(f), None),
            View::Live { out, size, color } => (
                None,
                Some(Live {
                    out,
                    size,
                    color,
                    drawn: 0,
                    last_ms: 0,
                }),
            ),
        };
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                tree,
                log: Some(log),
                status: status_path(store),
                plain,
                live,
                dirty: true,
                last_status_ms: 0,
                error: None,
            }),
            done: Mutex::new(false),
            wake: Condvar::new(),
            options,
        });
        let ticker = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("videbimus-progress".to_owned())
                .spawn(move || tick(&shared))?
        };
        Ok(Self {
            shared,
            ticker: Some(ticker),
            run_log,
        })
    }

    /// The sink to give the scheduler.
    #[must_use]
    pub fn sink(&self) -> Arc<dyn EventSink> {
        Arc::new(RecorderSink(self.shared.clone()))
    }

    /// The run's event log.
    #[must_use]
    pub fn run_log(&self) -> &Path {
        &self.run_log
    }

    /// The tree as it stands.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        lock(&self.shared.inner).tree.snapshot(crate::now_ms())
    }

    /// Ends the recording: a last write of the status file and a last
    /// redraw. Returns the final snapshot, and the first error writing
    /// the logs or status file met, if any (recording never fails a
    /// build).
    #[must_use = "the second value is a recording error to report"]
    pub fn finish(mut self) -> (Snapshot, Option<String>) {
        self.stop_ticker();
        let mut inner = lock(&self.shared.inner);
        let now = crate::now_ms();
        inner.write_status(now);
        if inner.live.is_some() {
            inner.draw(now);
        }
        if let Some(log) = inner.log.take() {
            let _ = log.sync_all();
        }
        (inner.tree.snapshot(now), inner.error.take())
    }

    fn stop_ticker(&mut self) {
        *lock(&self.shared.done) = true;
        self.shared.wake.notify_all();
        if let Some(t) = self.ticker.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop_ticker();
    }
}

struct RecorderSink(Arc<Shared>);

impl EventSink for RecorderSink {
    fn event(&self, event: &Event) {
        let mut inner = lock(&self.0.inner);
        if let Some(log) = inner.log.as_mut() {
            let mut line = serde_json::to_vec(event).unwrap_or_default();
            line.push(b'\n');
            if let Err(e) = log.write_all(&line) {
                inner
                    .error
                    .get_or_insert(format!("can't write the run log: {e}"));
            }
        }
        inner.tree.apply(event);
        inner.dirty = true;
        if let Some(f) = &inner.plain {
            f(event);
        }
    }
}

impl Inner {
    fn write_status(&mut self, now: u64) {
        let snap = self.tree.snapshot(now);
        let bytes = serde_json::to_vec_pretty(&snap).unwrap_or_default();
        if let Err(e) = replace_atomic(&self.status, &bytes) {
            self.error.get_or_insert(format!(
                "can't write the status file {}: {e}",
                self.status.display()
            ));
        }
        self.last_status_ms = now;
    }

    fn draw(&mut self, now: u64) {
        let snap = self.tree.snapshot(now);
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let (cols, rows) = (live.size)();
        let text = render(&snap, cols.max(20), live.color);
        let mut lines: Vec<&str> = text.lines().collect();
        let max = rows.saturating_sub(1).max(3);
        let more;
        if lines.len() > max {
            more = format!("… {} more lines", lines.len() - (max - 1));
            lines.truncate(max - 1);
            lines.push(&more);
        }
        let mut buf = String::new();
        if live.drawn > 0 {
            let _ = write!(buf, "\x1b[{}F", live.drawn);
        }
        buf.push_str("\x1b[J");
        for l in &lines {
            buf.push_str(l);
            buf.push('\n');
        }
        let _ = live.out.write_all(buf.as_bytes());
        let _ = live.out.flush();
        live.drawn = lines.len();
        live.last_ms = now;
        self.dirty = false;
    }
}

fn tick(shared: &Shared) {
    let o = shared.options;
    let step = o.redraw.min(o.status).max(Duration::from_millis(10));
    loop {
        {
            let done = lock(&shared.done);
            if *done {
                return;
            }
            let (done, _) = shared
                .wake
                .wait_timeout(done, step)
                .unwrap_or_else(PoisonError::into_inner);
            if *done {
                return;
            }
        }
        let mut inner = lock(&shared.inner);
        let now = crate::now_ms();
        let since_status = now.saturating_sub(inner.last_status_ms);
        if u128::from(since_status) >= o.status.as_millis() {
            inner.write_status(now);
        }
        if let Some(live) = &inner.live {
            let since = u128::from(now.saturating_sub(live.last_ms));
            if since >= o.redraw.as_millis() && (inner.dirty || since >= 1000) {
                inner.draw(now);
            }
        }
    }
}

/// Deletes all but the newest `keep` run logs in `dir`.
fn prune(dir: &Path, keep: usize) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    logs.sort();
    let n = logs.len().saturating_sub(keep.max(1));
    for p in &logs[..n] {
        let _ = fs::remove_file(p);
    }
}

/// `20261118T110640.123Z` from Unix milliseconds: sorts by time.
#[must_use]
pub fn compact_utc(ms: u64) -> String {
    let (y, m, d, hh, mm, ss) = civil(ms / 1000);
    format!("{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}.{:03}Z", ms % 1000)
}

/// `2026-11-18 11:06:40 UTC` from Unix milliseconds.
#[must_use]
pub fn utc(ms: u64) -> String {
    let (y, m, d, hh, mm, ss) = civil(ms / 1000);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02} UTC")
}

fn civil(secs: u64) -> (i64, i64, i64, u64, u64, u64) {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}
