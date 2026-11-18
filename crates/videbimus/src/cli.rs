//! The command line, the same in every binary ([ADR 0009] §1).
//!
//! ```text
//! <binary> [--config FILE] [--store DIR] <command>
//!
//!   build [TARGET…] [--profile P]   build what the scene reads and the
//!                                   output, or the named assets (or
//!                                   groups); Ctrl-C stops, and building
//!                                   again resumes
//!   preview FRAMES [--out DIR] [--profile P]
//!                                   build what those frames need, then render
//!                                   them to PNG; FRAMES is N, A..B or A..=B
//!   status                          the latest build of every asset
//!   review [--out FILE] [--open]    assemble the output's built segments,
//!                                   with placeholders for the rest, into a
//!                                   review file; builds nothing
//!   stop                            stop the build running on the store
//! ```
//!
//! `--config` defaults to `video.kdl`. The profile is `background` for
//! `build` and `interactive` for `preview`; `overnight` is the third.
//! Exit codes: 0 done, 1 failed, 2 usage, 130 stopped.
//!
//! [ADR 0009]: ../../../adrs/0009-using-videbimus.md

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use videbimus_core::{Frame, Interval};
use videbimus_runtime::color::write_png;
use videbimus_scheduler::{
    EarlyCutoff, Event, EventKind, Machine, Outcome, Profile, RunReport, Scheduler, Target,
    stop_on_sigint,
};
use videbimus_store::{LockHolder, Shape, Status, Store, machine_name};

use crate::App;
use crate::project::Project;

/// The default config file.
pub const DEFAULT_CONFIG: &str = "video.kdl";

/// Where a preview's frames go by default, relative to the config file's
/// directory.
pub const DEFAULT_PREVIEW: &str = ".videbimus/preview";

const STOPPED: u8 = 130;

#[derive(Clone)]
enum Sink {
    Std { err: bool },
    Memory(Arc<Mutex<Vec<u8>>>),
}

impl Sink {
    fn line(&self, s: &str) {
        match self {
            Sink::Std { err: false } => {
                let mut o = std::io::stdout().lock();
                let _ = writeln!(o, "{s}");
            }
            Sink::Std { err: true } => {
                let mut o = std::io::stderr().lock();
                let _ = writeln!(o, "{s}");
            }
            Sink::Memory(buf) => {
                let mut b = buf
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = writeln!(b, "{s}");
            }
        }
    }

    fn text(&self) -> String {
        match self {
            Sink::Std { .. } => String::new(),
            Sink::Memory(buf) => String::from_utf8_lossy(
                &buf.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
            .into_owned(),
        }
    }
}

/// Where the command line writes: standard output and error, or memory
/// (for tests).
#[derive(Clone)]
pub struct Console {
    out: Sink,
    err: Sink,
}

impl std::fmt::Debug for Console {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Console")
    }
}

impl Console {
    /// Standard output and error.
    #[must_use]
    pub fn std() -> Self {
        Self {
            out: Sink::Std { err: false },
            err: Sink::Std { err: true },
        }
    }

    /// Two buffers in memory, read back with [`out`](Self::out) and
    /// [`err`](Self::err).
    #[must_use]
    pub fn memory() -> Self {
        Self {
            out: Sink::Memory(Arc::default()),
            err: Sink::Memory(Arc::default()),
        }
    }

    /// What was written to output, if it is in memory.
    #[must_use]
    pub fn out(&self) -> String {
        self.out.text()
    }

    /// What was written to error, if it is in memory.
    #[must_use]
    pub fn err(&self) -> String {
        self.err.text()
    }

    fn say(&self, s: &str) {
        self.out.line(s);
    }

    fn warn(&self, s: &str) {
        self.err.line(s);
    }
}

/// The parsed command line.
#[derive(Debug, Default)]
struct Args {
    config: Option<PathBuf>,
    store: Option<PathBuf>,
    profile: Option<String>,
    out: Option<PathBuf>,
    open: bool,
    help: bool,
    positional: Vec<String>,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = args.peekable();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_owned(), Some(v.to_owned())),
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| it.next())
                .ok_or_else(|| format!("`{name}` needs a value"))
        };
        match flag.as_str() {
            "-c" | "--config" => a.config = Some(value("--config")?.into()),
            "--store" => a.store = Some(value("--store")?.into()),
            "--profile" => a.profile = Some(value("--profile")?),
            "--out" => a.out = Some(value("--out")?.into()),
            "--open" => a.open = true,
            "-h" | "--help" => a.help = true,
            "--" => a.positional.extend(it.by_ref()),
            f if f.starts_with('-')
                && f.len() > 1
                && !f[1..].starts_with(|c: char| c.is_ascii_digit()) =>
            {
                return Err(format!("there is no option `{f}`"));
            }
            _ => a.positional.push(arg),
        }
    }
    Ok(a)
}

fn usage(name: &str) -> String {
    format!(
        "usage: {name} [--config FILE] [--store DIR] <command>

commands:
  build [TARGET…] [--profile P]   build what the scene reads and the output, or the named
                                  assets or groups
  preview FRAMES [--out DIR] [--profile P]
                                  build what those frames need and render them to PNG
                                  (FRAMES: N, A..B or A..=B)
  status                          the latest build of every asset
  review [--out FILE] [--open]    assemble the built segments, with placeholders for the
                                  rest, into a review file (and open it); builds nothing
  stop                            stop the build running on the store

options:
  --config FILE   the config (default {DEFAULT_CONFIG})
  --store DIR     the asset store (default: the config's `store`, else {store} beside it)
  --profile P     interactive, background or overnight
  --out PATH      where preview writes frames (default {DEFAULT_PREVIEW} beside the config),
                  or review its file (default <output>.review.<ext> beside the output)
  --open          open the review file with the system's player",
        store = crate::project::DEFAULT_STORE
    )
}

/// Runs the command line `args` (without the program name) for `app`.
/// With `sigint`, Ctrl-C stops builds gracefully.
pub(crate) fn run(
    app: &App,
    args: impl Iterator<Item = String>,
    console: &Console,
    sigint: bool,
) -> ExitCode {
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            console.warn(&format!("error: {e}\n\n{}", usage(app.name())));
            return ExitCode::from(2);
        }
    };
    let Some(command) = a.positional.first().cloned() else {
        if a.help {
            console.say(&usage(app.name()));
            return ExitCode::SUCCESS;
        }
        console.warn(&format!("error: no command given\n\n{}", usage(app.name())));
        return ExitCode::from(2);
    };
    if a.help || command == "help" {
        console.say(&usage(app.name()));
        return ExitCode::SUCCESS;
    }
    let cx = Cx {
        app,
        console,
        sigint,
        args: &a,
    };
    match command.as_str() {
        "build" => cx.build(&a.positional[1..]),
        "preview" => cx.preview(&a.positional[1..]),
        "status" => cx.status(),
        "review" => cx.review(&a.positional[1..]),
        "stop" => cx.stop(),
        other => {
            console.warn(&format!(
                "error: there is no command `{other}`\n\n{}",
                usage(app.name())
            ));
            ExitCode::from(2)
        }
    }
}

struct Cx<'a> {
    app: &'a App,
    console: &'a Console,
    sigint: bool,
    args: &'a Args,
}

impl Cx<'_> {
    fn config_path(&self) -> PathBuf {
        self.args
            .config
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG))
    }

    fn load(&self) -> Option<Project> {
        match self
            .app
            .load(&self.config_path(), self.args.store.as_deref())
        {
            Ok(p) => Some(p),
            Err(e) => {
                self.console.warn(e.to_string().trim_end());
                None
            }
        }
    }

    fn profile(&self, default: &str) -> Result<Profile, ExitCode> {
        let name = self.args.profile.as_deref().unwrap_or(default);
        Profile::named(name, &Machine::detect()).ok_or_else(|| {
            self.console.warn(&format!(
                "error: there is no profile `{name}`; the profiles are interactive, background and overnight"
            ));
            ExitCode::from(2)
        })
    }

    fn build(&self, names: &[String]) -> ExitCode {
        if self.args.out.is_some() {
            self.console
                .warn("error: `--out` is for preview and review");
            return ExitCode::from(2);
        }
        let profile = match self.profile("background") {
            Ok(p) => p,
            Err(code) => return code,
        };
        let Some(project) = self.load() else {
            return ExitCode::FAILURE;
        };
        if project.graph().is_empty() {
            self.console
                .say("nothing to build: the config declares no assets");
            return ExitCode::SUCCESS;
        }
        let targets = if names.is_empty() {
            if project.root().is_none() {
                Vec::new() // every asset
            } else {
                let mut t = project.scene_targets(None);
                if let Some(o) = project.output() {
                    t.push(o.target());
                }
                if t.is_empty() {
                    self.console.say(
                        "nothing to build: the scene reads no assets (name assets to build them)",
                    );
                    return ExitCode::SUCCESS;
                }
                t
            }
        } else {
            match project.named_targets(names) {
                Ok(t) => t,
                Err(e) => {
                    self.console.warn(&format!("error: {e}"));
                    return ExitCode::from(2);
                }
            }
        };
        let Some(report) = self.schedule_report(&project, &targets, profile) else {
            return ExitCode::FAILURE;
        };
        match report.outcome {
            Outcome::Complete => self.export(&project, &report),
            Outcome::Stopped => ExitCode::from(STOPPED),
            Outcome::Failed => ExitCode::FAILURE,
        }
    }

    /// Copies the joined output, if this run built or found it, to its
    /// file (ticket 0007).
    fn export(&self, project: &Project, report: &RunReport) -> ExitCode {
        let Some(o) = project.output() else {
            return ExitCode::SUCCESS;
        };
        let Some(key) = report.task(o.join_name()).and_then(|t| t.key) else {
            return ExitCode::SUCCESS; // not requested
        };
        let store = match Store::open_read_only(project.store()) {
            Ok(s) => s,
            Err(e) => {
                self.console.warn(&format!("error: {e}"));
                return ExitCode::FAILURE;
            }
        };
        match o.export(&store, key) {
            Ok(path) => {
                self.console.say(&format!("wrote {}", path.display()));
                ExitCode::SUCCESS
            }
            Err(e) => {
                self.console.warn(&format!("error: {e}"));
                ExitCode::FAILURE
            }
        }
    }

    fn review(&self, rest: &[String]) -> ExitCode {
        if !rest.is_empty() || self.args.profile.is_some() {
            self.console.warn(&format!(
                "error: review takes no arguments but --out FILE and --open\n\n{}",
                usage(self.app.name())
            ));
            return ExitCode::from(2);
        }
        let Some(project) = self.load() else {
            return ExitCode::FAILURE;
        };
        let Some(o) = project.output() else {
            self.console.warn(
                "error: the config declares no output to review; add output \"film.mp4\" at the top",
            );
            return ExitCode::FAILURE;
        };
        let dest = self
            .args
            .out
            .clone()
            .unwrap_or_else(|| o.default_review_path());
        let review = match o.review(project.store(), &dest) {
            Ok(r) => r,
            Err(e) => {
                self.console.warn(&format!("error: {e}"));
                return ExitCode::FAILURE;
            }
        };
        let missing: i64 = review.missing.iter().map(|iv| iv.len()).sum();
        self.console.say(&format!(
            "wrote {}: {} of {} segments built{}{}",
            review.path.display(),
            review.segments - review.missing.len(),
            review.segments,
            if missing > 0 {
                format!(", {missing} frames shown as placeholders")
            } else {
                String::new()
            },
            if o.spec().audio.is_some() && !review.audio {
                ", no audio yet"
            } else {
                ""
            }
        ));
        if self.args.open {
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else if cfg!(windows) {
                "explorer"
            } else {
                "xdg-open"
            };
            if let Err(e) = std::process::Command::new(opener).arg(&review.path).spawn() {
                self.console
                    .warn(&format!("warning: couldn't open it with {opener}: {e}"));
            }
        }
        ExitCode::SUCCESS
    }

    /// Runs the scheduler on `targets`, printing a line per event.
    fn schedule(&self, project: &Project, targets: &[Target], profile: Profile) -> Option<Outcome> {
        self.schedule_report(project, targets, profile)
            .map(|r| r.outcome)
    }

    /// As [`schedule`](Self::schedule), with the whole report.
    fn schedule_report(
        &self,
        project: &Project,
        targets: &[Target],
        profile: Profile,
    ) -> Option<RunReport> {
        let (store, warnings) = match Store::open(project.store()) {
            Ok(s) => s,
            Err(e) => {
                self.console.warn(&format!("error: {e}"));
                return None;
            }
        };
        for w in warnings {
            self.console.warn(&format!("warning: {w}"));
        }
        if self.sigint
            && let Err(e) = stop_on_sigint()
        {
            self.console.warn(&format!(
                "warning: Ctrl-C won't stop the build gracefully: {e}"
            ));
        }
        let console = self.console.clone();
        let sink = move |e: &Event| console.say(&event_line(&e.kind));
        let scheduler = Scheduler::new(store, profile).with_sink(Arc::new(sink));
        match scheduler.run(project.graph(), targets) {
            Ok(report) => Some(report),
            Err(e) => {
                self.console.warn(&format!("error: {e}"));
                None
            }
        }
    }

    fn preview(&self, rest: &[String]) -> ExitCode {
        let [range] = rest else {
            self.console.warn(&format!(
                "error: preview takes one frame range: N, A..B or A..=B\n\n{}",
                usage(self.app.name())
            ));
            return ExitCode::from(2);
        };
        let frames = match parse_frames(range) {
            Ok(f) => f,
            Err(e) => {
                self.console.warn(&format!("error: {e}"));
                return ExitCode::from(2);
            }
        };
        let profile = match self.profile("interactive") {
            Ok(p) => p,
            Err(code) => return code,
        };
        let Some(project) = self.load() else {
            return ExitCode::FAILURE;
        };
        if project.root().is_none() {
            self.console.warn(
                "error: the config has no scene to preview; add a top-level component (and root=\"…\" on `video` if there are several)",
            );
            return ExitCode::FAILURE;
        }
        let targets = project.scene_targets(Some(frames));
        let mut failed = false;
        if !targets.is_empty() {
            match self.schedule(&project, &targets, profile) {
                Some(Outcome::Complete) => {}
                Some(Outcome::Stopped) => return ExitCode::from(STOPPED),
                Some(Outcome::Failed) | None => {
                    self.console.warn(
                        "warning: some assets failed to build; their frames show placeholders",
                    );
                    failed = true;
                }
            }
        }
        let mut scene = match project.scene(self.app) {
            Ok(Some(s)) => s,
            Ok(None) => return ExitCode::FAILURE,
            Err(e) => {
                self.console.warn(e.to_string().trim_end());
                return ExitCode::FAILURE;
            }
        };
        let dir = self.args.out.clone().unwrap_or_else(|| {
            project
                .path()
                .parent()
                .unwrap_or(Path::new(""))
                .join(DEFAULT_PREVIEW)
        });
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.console
                .warn(&format!("error: can't create {}: {e}", dir.display()));
            return ExitCode::FAILURE;
        }
        for f in frames.start.0..frames.end.0 {
            let image = scene.render(Frame(f));
            if let Err(e) = write_png(&image, &dir.join(frame_file(f))) {
                self.console.warn(&format!("error: {e}"));
                return ExitCode::FAILURE;
            }
        }
        self.console.say(&format!(
            "wrote {} frame{} to {}",
            frames.len(),
            if frames.len() == 1 { "" } else { "s" },
            dir.display()
        ));
        if failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }

    fn status(&self) -> ExitCode {
        let Some(project) = self.load() else {
            return ExitCode::FAILURE;
        };
        let store = if project.store().is_dir() {
            match Store::open_read_only(project.store()) {
                Ok(s) => Some(s),
                Err(e) => {
                    self.console.warn(&format!("error: {e}"));
                    return ExitCode::FAILURE;
                }
            }
        } else {
            None
        };
        self.console.say(&format!(
            "latest builds in {} (the live view is ticket 0005)",
            project.store().display()
        ));
        if let Some(root) = project.root() {
            let mut read: Vec<&str> = project
                .wired()
                .iter()
                .map(|(_, r)| r.name.as_str())
                .collect();
            read.sort_unstable();
            read.dedup();
            self.console.say(&format!(
                "scene {} reads: {}",
                root.describe(),
                if read.is_empty() {
                    "no assets".to_owned()
                } else {
                    read.join(", ")
                }
            ));
        }
        let width = project
            .assets()
            .iter()
            .map(|a| a.name.len())
            .max()
            .unwrap_or(0);
        for a in project.assets() {
            let state = store
                .as_ref()
                .map_or_else(|| Ok("not built".to_owned()), |s| latest_state(s, &a.name));
            let state = state.unwrap_or_else(|e| format!("unreadable: {e}"));
            self.console
                .say(&format!("{:width$}  {}  {state}", a.name, a.recipe));
        }
        ExitCode::SUCCESS
    }

    fn stop(&self) -> ExitCode {
        let Some(project) = self.load() else {
            return ExitCode::FAILURE;
        };
        let lock = project.store().join("lock");
        let holder: LockHolder = match std::fs::read(&lock) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.console.say(&format!(
                    "nothing is building in {}",
                    project.store().display()
                ));
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                self.console
                    .warn(&format!("error: can't read {}: {e}", lock.display()));
                return ExitCode::FAILURE;
            }
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(h) => h,
                Err(e) => {
                    self.console
                        .warn(&format!("error: {} isn't a lock: {e}", lock.display()));
                    return ExitCode::FAILURE;
                }
            },
        };
        let here = machine_name();
        if holder.host != here {
            self.console.warn(&format!(
                "error: the build ({holder}) runs on {}, not here ({here}); stop it there",
                holder.host
            ));
            return ExitCode::FAILURE;
        }
        match interrupt(holder.pid) {
            Ok(()) => {
                self.console.say(&format!(
                    "asked the build ({holder}) to stop: running chunks are discarded and committed ones kept; build again to resume"
                ));
                ExitCode::SUCCESS
            }
            Err(e) => {
                self.console
                    .warn(&format!("error: can't stop pid {}: {e}", holder.pid));
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(unix)]
fn interrupt(pid: u32) -> std::io::Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "pid out of range"))?;
    // SAFETY: kill has no memory-safety preconditions.
    if unsafe { libc::kill(pid, libc::SIGINT) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn interrupt(_: u32) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stop is Unix-only; press Ctrl-C in the build's terminal",
    ))
}

fn latest_state(store: &Store, name: &str) -> Result<String, videbimus_store::Error> {
    let Some(key) = store.latest(name)? else {
        return Ok("not built".to_owned());
    };
    let Some(p) = store.progress(key)? else {
        return Ok("not built".to_owned());
    };
    let frames = match p.shape {
        Shape::Single => String::new(),
        Shape::Frames(_) => format!(", {}/{} frames", p.done_frames, p.total_frames),
    };
    Ok(match p.status {
        Status::Complete => format!("complete{frames} ({key})"),
        Status::Building => format!("partial{frames} ({key})"),
        Status::Failed { message } => format!("failed{frames} ({key}): {message}"),
    })
}

/// The file a preview writes video frame `f` to.
#[must_use]
pub fn frame_file(f: i64) -> String {
    format!("{f:06}.png")
}

/// `N`, `A..B` (B excluded) or `A..=B`.
fn parse_frames(s: &str) -> Result<Interval, String> {
    let num = |t: &str| {
        t.trim()
            .parse::<i64>()
            .map_err(|_| format!("`{s}` isn't a frame range: write N, A..B or A..=B"))
    };
    let (a, b) = if let Some((a, b)) = s.split_once("..=") {
        (num(a)?, num(b)? + 1)
    } else if let Some((a, b)) = s.split_once("..") {
        (num(a)?, num(b)?)
    } else {
        let n = num(s)?;
        (n, n + 1)
    };
    if a < 0 || b <= a {
        return Err(format!(
            "`{s}` has no frames: frames count from 0, and a range A..B needs B after A"
        ));
    }
    Ok(Interval::new(Frame(a), Frame(b)))
}

fn shape_text(shape: Shape) -> String {
    match shape {
        Shape::Single => "one output".to_owned(),
        Shape::Frames(iv) => format!("frames {}..{}", iv.start.0, iv.end.0),
    }
}

/// One plain line for a scheduler event.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn event_line(e: &EventKind) -> String {
    match e {
        EventKind::RunStarted {
            machine,
            profile,
            targets,
        } => format!(
            "run started on {machine}, profile {} ({} cores): {}",
            profile.name,
            profile.cores,
            if targets.is_empty() {
                "everything".to_owned()
            } else {
                targets.join(", ")
            }
        ),
        EventKind::Planned {
            task,
            shape,
            estimate_s,
            from_history,
            ..
        } => format!(
            "{}: planned, {}, ~{estimate_s:.1} s{}",
            task.name,
            shape_text(*shape),
            if *from_history { " (measured)" } else { "" }
        ),
        EventKind::KeyResolved { task, key } => format!("{}: key {key}", task.name),
        EventKind::Cached { task, key } => format!("{}: cached ({key})", task.name),
        EventKind::Begun {
            task,
            key,
            why,
            chunks,
            done_frames,
            total_frames,
            log,
        } => format!(
            "{}: building ({why}), {chunks} chunk{}{}; key {key}, log {}",
            task.name,
            if *chunks == 1 { "" } else { "s" },
            if *done_frames > 0 {
                format!(", {done_frames}/{total_frames} frames already built")
            } else {
                String::new()
            },
            log.display()
        ),
        EventKind::Queued {
            task,
            chunk,
            estimate_s,
        } => format!(
            "{} {}: queued, ~{estimate_s:.1} s",
            task.name,
            chunk.dir_name()
        ),
        EventKind::Started {
            task,
            chunk,
            machine,
            cores,
            niceness,
            alone,
        } => format!(
            "{} {}: started on {machine}, {cores} core{} at nice {niceness}{}",
            task.name,
            chunk.dir_name(),
            if *cores == 1 { "" } else { "s" },
            if *alone {
                ", alone: it is bigger than the budget"
            } else {
                ""
            }
        ),
        EventKind::Progress {
            task,
            chunk,
            done,
            total,
        } => format!("{} {}: {done}/{total}", task.name, chunk.dir_name()),
        EventKind::ChunkCommitted {
            task, chunk, ms, ..
        } => {
            format!("{} {}: committed in {ms} ms", task.name, chunk.dir_name())
        }
        EventKind::Finished {
            task, key, cutoff, ..
        } => format!(
            "{}: finished ({key}){}",
            task.name,
            if *cutoff == EarlyCutoff::Same {
                ", output unchanged"
            } else {
                ""
            }
        ),
        EventKind::Partial { task, key } => {
            format!("{}: the requested frames are built ({key})", task.name)
        }
        EventKind::Failed {
            task,
            chunk,
            message,
            last_lines,
            log,
        } => {
            let mut s = format!(
                "{}{}: failed: {message}",
                task.name,
                chunk
                    .map(|c| format!(" {}", c.dir_name()))
                    .unwrap_or_default()
            );
            for l in last_lines {
                let _ = write!(s, "\n    | {l}");
            }
            if let Some(log) = log {
                let _ = write!(s, "\n    log: {}", log.display());
            }
            s
        }
        EventKind::Cancelled { task, chunk } => {
            format!("{} {}: cancelled", task.name, chunk.dir_name())
        }
        EventKind::Blocked { task, by } => format!("{}: blocked: {by} failed", task.name),
        EventKind::NotNeeded { task } => {
            format!("{}: not needed, what reads it is cached", task.name)
        }
        EventKind::ProfileChanged { profile } => format!("profile changed to {}", profile.name),
        EventKind::Preview { targets } => format!("preview first: {}", targets.join(", ")),
        EventKind::Warning { task, message } => match task {
            Some(t) => format!("warning: {}: {message}", t.name),
            None => format!("warning: {message}"),
        },
        EventKind::StopRequested => {
            "stopping: running chunks are discarded; build again to resume".to_owned()
        }
        EventKind::RunFinished {
            outcome,
            built,
            cached,
            failed,
            chunks,
            ms,
        } => {
            #[allow(clippy::cast_precision_loss)]
            let s = *ms as f64 / 1000.0;
            format!(
                "run {}: {built} built, {cached} cached, {failed} failed, {chunks} chunk{} in {s:.1} s",
                match outcome {
                    Outcome::Complete => "complete",
                    Outcome::Failed => "failed",
                    Outcome::Stopped => "stopped",
                },
                if *chunks == 1 { "" } else { "s" },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Result<Args, String> {
        parse(s.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn parses() {
        let a = args("--config v.kdl build s07 --profile overnight").unwrap();
        assert_eq!(a.config.as_deref(), Some(Path::new("v.kdl")));
        assert_eq!(a.positional, ["build", "s07"]);
        assert_eq!(a.profile.as_deref(), Some("overnight"));
        let a = args("preview 10..20 --out=x --store s").unwrap();
        assert_eq!(a.out.as_deref(), Some(Path::new("x")));
        assert_eq!(a.store.as_deref(), Some(Path::new("s")));
        assert_eq!(
            args("build --bogus").unwrap_err(),
            "there is no option `--bogus`"
        );
        assert_eq!(
            args("build --profile").unwrap_err(),
            "`--profile` needs a value"
        );
    }

    #[test]
    fn frame_ranges() {
        let iv = |a, b| Interval::new(Frame(a), Frame(b));
        assert_eq!(parse_frames("7"), Ok(iv(7, 8)));
        assert_eq!(parse_frames("10..20"), Ok(iv(10, 20)));
        assert_eq!(parse_frames("10..=20"), Ok(iv(10, 21)));
        assert!(parse_frames("20..10").is_err());
        assert!(parse_frames("x").is_err());
    }
}
