//! The `command` recipe: any external program as an asset ([ADR 0009] §3).
//!
//! Blender, ffmpeg, Python scripts and existing binaries are all
//! configured in KDL, with no Rust:
//!
//! ```kdl
//! video rate=30
//!
//! asset "s14/blender" recipe="command" {
//!     input "shot" "s14/shot"
//!     tool "blender" path="/Applications/Blender.app/Contents/MacOS/Blender" \
//!         version-from="blender --version" version-match="Blender ([0-9.]+)"
//!     script "hwscene.py" name="scene"
//!     env PHYS=1
//!     run "blender" "-b" "-t" "$cores" "-P" "$script.scene" "--" \
//!         "$in.shot" "$out" "--start" "$chunk.start" "--end" "$chunk.last"
//!     output frames="####.png" from=(f)1 to=(f)279 chunk=8
//!     cost per-frame=20 cores=4 mem="3G" gpu=#true remote=#true
//! }
//! ```
//!
//! # The node
//!
//! The asset node has no properties of its own (beyond the project's
//! `recipe`); everything is in its children:
//!
//! - **`run "<program>" "<arg>"…`** (one): the command line, run
//!   directly, not through a shell (write `run "sh" "-c" "…"` for one).
//!   A program that names a declared `tool` runs that tool's `path`.
//!   Each argument may use the substitutions below.
//! - **`tool "<name>"`**: a program it runs, whose version goes into the
//!   key. `version-from="<command>"` is run once per process (split like
//!   a shell would; a first word that is the tool's name runs its
//!   `path`), and its first non-empty line of output is the version, or
//!   with `version-match="<regex>"` the first match (its first group, if
//!   it has one). `version="<v>"` gives the version instead. `path` is
//!   the executable (relative to the config file), else `name` is looked
//!   up on `PATH` (as is a `path` without a `/`). Neither `path` nor the probe is in the key, only the
//!   version: machines with the same tool at different paths share
//!   outputs (ADR 0006). A tool that can't be probed is an error at load,
//!   naming the tool and the probe. Programs that aren't declared tools
//!   run too, unkeyed.
//! - **`script "<path>"`** and **`source "<path>"`**: files (or
//!   directories) the command reads, relative to the config file, hashed
//!   into the key under their name: `name="…"`, by default the file name
//!   without its extension. Scripts are code, sources data; both
//!   rebuild what reads them when they change, and the path itself
//!   isn't keyed.
//! - **`output`**: what the command writes into `$out`, checked after it
//!   exits:
//!   - `output file="<name>"` or `output dir="<name>"`: a single output
//!     (several `output` nodes for several files);
//!   - `output frames="####.png" from=(f)1 to=(f)279`: a frame sequence,
//!     one file per frame named by the pattern (a run of `#` is the
//!     zero-padded frame number), frames `from` up to but not including
//!     `to`. It is built in chunks, each a separate run of the command
//!     that writes exactly `$chunk`'s frames, committed atomically
//!     (ADR 0008 §1): `chunk=<frames>` sizes them, else the scheduler
//!     sizes them from the cost (about two minutes each).
//! - **`cost`**: `per-frame=<seconds>` or `per-run=<seconds>` (numbers
//!   or `(s)` times; default `per-run=60`), `cores` (default 1), `mem`
//!   (bytes, or `"512M"`, `"3G"`), `gpu` and `remote` (whether it may run
//!   on another machine; recorded, not yet used). Measured history
//!   replaces the estimates (ADR 0004 §2). None of it is in the key, nor
//!   is `output`'s `chunk`.
//! - **`env NAME="value"…`**: environment variables, added to the
//!   inherited ones (plus the thread caps of ADR 0004 §3).
//! - **`cwd "<dir>"`**: the working directory, relative to the config
//!   file; by default the config file's directory.
//!
//! # Substitutions
//!
//! In `run`, `env` and `cwd` values:
//!
//! | | |
//! |---|---|
//! | `$in.<label>` | the input's output: the directory of a single output; for a frame sequence, a directory holding every frame (links into its chunks) |
//! | `$out` | the directory to write the chunk's output into |
//! | `$chunk.start` | the chunk's first frame |
//! | `$chunk.last` | its last frame, inclusive (Blender's `--end`) |
//! | `$chunk.end` | one past its last frame, as config's `to` |
//! | `$chunk.count` | its number of frames |
//! | `$cores` | the cores the scheduler granted |
//! | `$script.<name>`, `$source.<name>` | the declared file's absolute path |
//!
//! They work anywhere in a string (`"$in.shot/panel.mp4"`,
//! `"--out=$out"`). `$$` is a literal `$` (`"$$out"` passes `$out`). A
//! `$` before any other name (`$HOME`, `$1`) is passed through as
//! written, for a shell. The config language keeps strings that start
//! with these names, or `$$` and one of them, as written (ADR 0005 §2),
//! so it never mistakes them for its own references. A string that
//! starts with another `$name` is a config reference (a `let`, which
//! works here too), so a shell variable at the start of an argument is
//! written `"$$HOME"`.
//!
//! # Running
//!
//! Each chunk's command runs through
//! [`TaskContext::command`](videbimus_scheduler::TaskContext::command):
//! at the profile's niceness, with thread caps, in its own process group
//! (killed on stop), with its full stdout and stderr in the task log. A
//! non-zero exit fails the chunk with its last lines of output. So does
//! a missing output: after a successful exit, every declared file, or
//! every frame of the chunk, must exist.
//!
//! [ADR 0009]: ../../../adrs/0009-using-videbimus.md

mod template;
mod tool;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use regex::Regex;
use videbimus_config::{Error, Located, Node, Origin, Resolver, Unit, Value};
use videbimus_core::Interval;
use videbimus_runtime::registry::did_you_mean;
use videbimus_scheduler::{Cost, Resources, Task, TaskContext, TaskError};
use videbimus_store::{ChunkId, KeyBuilder, Shape};

use crate::recipe::{KeyStart, RecipeContext, RecipeKind};
use template::{Names, Problem, Template, Vars};

pub use tool::forget_tool_versions;

mod code {
    crate::include_code!();
}

/// Children of a `command` asset that aren't keyed as config: tools and
/// files are keyed by version and contents, cost and chunking can't
/// change the output.
const UNKEYED: [&str; 5] = ["tool", "script", "source", "cost", "output.chunk"];

const CHILDREN: [&str; 8] = [
    "run", "tool", "script", "source", "output", "cost", "env", "cwd",
];

/// The `command` recipe kind: see the [module](self).
#[derive(Debug)]
pub struct CommandRecipe;

impl RecipeKind for CommandRecipe {
    const KIND: &'static str = "command";
    const CODE: &'static str = code::COMMAND;

    fn from_config(node: &Node, r: &Resolver, cx: &RecipeContext) -> Result<Box<dyn Task>, Error> {
        Ok(Box::new(CommandTask::from_config(node, r, cx)?))
    }
}

#[derive(Clone, Debug)]
struct Tool {
    name: String,
    program: String,
    version: String,
}

#[derive(Clone, Debug)]
struct File {
    script: bool,
    name: String,
    path: PathBuf,
    digest: [u8; 32],
}

#[derive(Clone, Debug)]
enum Expect {
    File(String),
    Dir(String),
}

#[derive(Clone, Debug)]
enum Output {
    Single(Vec<Expect>),
    Frames {
        pattern: (String, usize, String),
        shown: String,
        frames: Interval,
        chunk: Option<u32>,
    },
}

/// A `command` asset's task.
#[derive(Debug)]
pub struct CommandTask {
    key: KeyStart,
    tools: Vec<Tool>,
    files: Vec<File>,
    run: Vec<Template>,
    env: Vec<(String, Template)>,
    cwd: Option<Template>,
    base: PathBuf,
    output: Output,
    cost: Cost,
    resources: Resources,
    remote: bool,
}

impl CommandTask {
    /// Whether it may run on another machine (`cost remote=#true`).
    /// Recorded; remote execution isn't implemented yet.
    #[must_use]
    pub fn remote(&self) -> bool {
        self.remote
    }

    /// The tools it runs, with their probed versions: name and version.
    pub fn tools(&self) -> impl Iterator<Item = (&str, &str)> {
        self.tools
            .iter()
            .map(|t| (t.name.as_str(), t.version.as_str()))
    }

    fn from_config(node: &Node, r: &Resolver, cx: &RecipeContext) -> Result<Self, Error> {
        cx.check_props(node, r, &[])?;
        let base = dir_of(node.origin());
        let children: Vec<&Node> = cx.children(node).collect();
        check_children(&children, r)?;
        let of = |kind: &'static str| children.iter().copied().filter(move |c| c.kind() == kind);
        let one = |kind: &'static str| only(&children, kind, r);

        let (tools, files) = read_declarations(&children, r)?;
        let output = read_output(node, &of("output").collect::<Vec<_>>(), r)?;
        let (cost, resources, remote) = match one("cost")? {
            Some(c) => read_cost(c, r)?,
            None => (Cost::Seconds(60.0), Resources::new(1, 0), false),
        };

        let inputs: Vec<String> = cx.inputs().iter().map(|(l, _)| l.clone()).collect();
        let names_of = |script: bool| -> Vec<String> {
            files
                .iter()
                .filter(|f| f.script == script)
                .map(|f| f.name.clone())
                .collect()
        };
        let (scripts, sources) = (names_of(true), names_of(false));
        let names = Names {
            inputs: &inputs,
            scripts: &scripts,
            sources: &sources,
            frames: matches!(output, Output::Frames { .. }),
        };
        let template = |c: &Node, v: &Located<Value>, what: &str| -> Result<Template, Error> {
            let s = text(c, v, r, what)?;
            Template::parse(&s, &names)
                .map_err(|Problem { message, help }| r.error(c, &v.origin, message).help(help))
        };

        let Some(run_node) = one("run")? else {
            return Err(r
                .error(node, node.origin(), "a command needs `run`: the command line")
                .help("add run \"<program>\" \"<arg>\"…, e.g. run \"blender\" \"-b\" \"-P\" \"$script.scene\""));
        };
        r.check_props(run_node, &[])?;
        if run_node.args().is_empty() {
            return Err(r
                .error(run_node, run_node.origin(), "`run` has no program")
                .help("run \"<program>\" \"<arg>\"…"));
        }
        let mut run = run_node
            .args()
            .iter()
            .map(|a| template(run_node, a, "argument"))
            .collect::<Result<Vec<_>, _>>()?;
        resolve_program(&mut run[0], &tools, &run_node.args()[0].origin);

        let mut env = Vec::new();
        for e in of("env") {
            if !e.args().is_empty() {
                return Err(r
                    .error(e, e.origin(), "`env` takes NAME=\"value\" properties")
                    .help("env PHYS=1 BLENDER_USER_SCRIPTS=\"scripts\""));
            }
            for p in e.props() {
                let what = format!("property `{}`", p.key);
                env.push((p.key.clone(), template(e, &p.value, &what)?));
            }
        }
        let cwd = match one("cwd")? {
            Some(c) => {
                r.check_props(c, &[])?;
                if c.args().len() != 1 {
                    return Err(r
                        .error(c, c.origin(), "`cwd` takes one directory")
                        .help("cwd \"work\", relative to the config file"));
                }
                Some(template(c, &c.args()[0], "argument")?)
            }
            None => None,
        };

        Ok(Self {
            key: cx.key_skipping(node, r, &UNKEYED),
            tools,
            files,
            run,
            env,
            cwd,
            base,
            output,
            cost,
            resources,
            remote,
        })
    }

    /// Where the chunk's inputs are, linking a frame sequence's chunks
    /// into one directory.
    fn input_paths(
        &self,
        ctx: &TaskContext<'_>,
        links: &mut Vec<LinkDir>,
    ) -> Result<BTreeMap<String, PathBuf>, TaskError> {
        let used: BTreeSet<&str> = self
            .run
            .iter()
            .chain(self.env.iter().map(|(_, t)| t))
            .chain(&self.cwd)
            .flat_map(Template::inputs)
            .collect();
        let mut out = BTreeMap::new();
        for label in used {
            let key = ctx
                .input(label)
                .ok_or_else(|| TaskError::new(format!("no input labelled `{label}`")))?;
            let chunks = ctx.store().committed(key)?;
            let path = match chunks.as_slice() {
                [] => {
                    return Err(TaskError::new(format!(
                        "input `{label}` has no committed output"
                    )));
                }
                [c] if c.id() == ChunkId::Whole => ctx.store().chunk_dir(key, c.id()),
                _ => {
                    let dirs: Vec<PathBuf> = chunks
                        .iter()
                        .map(|c| ctx.store().chunk_dir(key, c.id()))
                        .collect();
                    let dir = LinkDir::new(label, &dirs).map_err(|e| {
                        TaskError::new(format!("linking input `{label}`'s frames: {e}"))
                    })?;
                    let p = dir.0.clone();
                    links.push(dir);
                    p
                }
            };
            out.insert(label.to_owned(), path);
        }
        Ok(out)
    }

    /// The outputs missing from `out` after the command: `Err` with what
    /// is missing.
    fn check_outputs(&self, out: &Path, frames: Option<Interval>) -> Result<(), String> {
        match &self.output {
            Output::Single(expect) => {
                let mut missing = Vec::new();
                for e in expect {
                    match e {
                        Expect::File(f) if !out.join(f).is_file() => {
                            missing.push(format!("file `{f}`"));
                        }
                        Expect::Dir(d) if !out.join(d).is_dir() => {
                            missing.push(format!("directory `{d}`"));
                        }
                        _ => {}
                    }
                }
                if missing.is_empty() {
                    return Ok(());
                }
                Err(format!(
                    "the command exited successfully but didn't write {} into $out{}",
                    missing.join(" or "),
                    wrote(out)
                ))
            }
            Output::Frames { pattern, shown, .. } => {
                let iv = frames.ok_or("a frame output's chunk has frames")?;
                let missing: Vec<i64> = (iv.start.0..iv.end.0)
                    .filter(|&f| !out.join(frame_name(pattern, f)).is_file())
                    .collect();
                if missing.is_empty() {
                    return Ok(());
                }
                Err(format!(
                    "the command exited successfully but didn't write {} of the chunk's {} frames ({}–{}): frames {} (\"{shown}\": expected {}){}",
                    missing.len(),
                    iv.len(),
                    iv.start.0,
                    iv.end.0 - 1,
                    ranges(&missing),
                    frame_name(pattern, missing[0]),
                    wrote(out)
                ))
            }
        }
    }
}

impl Task for CommandTask {
    fn name(&self) -> &'static str {
        ""
    }

    fn shape(&self) -> Shape {
        match &self.output {
            Output::Single(_) => Shape::Single,
            Output::Frames { frames, .. } => Shape::Frames(*frames),
        }
    }

    fn resources(&self) -> Resources {
        self.resources
    }

    fn cost(&self) -> Cost {
        self.cost
    }

    fn key(&self) -> KeyBuilder {
        let mut k = self.key.builder();
        for t in &self.tools {
            k = k.tool(&t.name, &t.version);
        }
        for f in &self.files {
            k = if f.script {
                k.script(&f.name, f.digest)
            } else {
                k.source(&f.name, f.digest)
            };
        }
        k
    }

    fn chunk_frames(&self) -> Option<u32> {
        match &self.output {
            Output::Frames { chunk, .. } => *chunk,
            Output::Single(_) => None,
        }
    }

    fn run(&self, ctx: &TaskContext<'_>) -> Result<(), TaskError> {
        let frames = ctx.frames();
        let mut links = Vec::new();
        let inputs = self.input_paths(ctx, &mut links)?;
        let paths = |script: bool| -> BTreeMap<String, PathBuf> {
            self.files
                .iter()
                .filter(|f| f.script == script)
                .map(|f| (f.name.clone(), f.path.clone()))
                .collect()
        };
        let (scripts, sources) = (paths(true), paths(false));
        let vars = Vars {
            inputs: &inputs,
            out: std::path::absolute(ctx.out_dir()).unwrap_or_else(|_| ctx.out_dir().to_owned()),
            chunk: frames,
            cores: ctx.cores(),
            scripts: &scripts,
            sources: &sources,
        };
        let argv: Vec<String> = self.run.iter().map(|t| t.fill(&vars)).collect();
        let cwd = self
            .cwd
            .as_ref()
            .map_or_else(|| self.base.clone(), |t| self.base.join(t.fill(&vars)));
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..]).current_dir(&cwd);
        for (k, v) in &self.env {
            cmd.env(k, v.fill(&vars));
        }
        for t in &self.tools {
            ctx.log(&format!("tool {}: {}", t.name, t.version));
        }
        if !self.env.is_empty() {
            let shown: Vec<String> = self
                .env
                .iter()
                .map(|(k, v)| format!("{k}={:?}", v.fill(&vars)))
                .collect();
            ctx.log(&format!("env {}", shown.join(" ")));
        }
        ctx.log(&format!("cwd {}", cwd.display()));

        let done = AtomicBool::new(false);
        let (result, reported) = std::thread::scope(|s| {
            let watcher = match (&self.output, frames) {
                (Output::Frames { pattern, .. }, Some(iv)) => {
                    let done = &done;
                    let out = ctx.out_dir();
                    Some(s.spawn(move || watch_frames(ctx, out, pattern, iv, done)))
                }
                _ => None,
            };
            let r = ctx.command(&mut cmd);
            done.store(true, Ordering::SeqCst);
            (r, watcher.and_then(|w| w.join().ok()).unwrap_or(0))
        });
        let output = result?;
        self.check_outputs(ctx.out_dir(), frames)
            .map_err(|m| TaskError::new(m).with_last_lines(output.last_lines))?;
        if let Some(iv) = frames {
            let n = u64::try_from(iv.len()).unwrap_or(0);
            if reported != n {
                ctx.progress(n, n);
            }
        }
        Ok(())
    }
}

/// The tools, scripts and sources, each name unique.
fn read_declarations(children: &[&Node], r: &Resolver) -> Result<(Vec<Tool>, Vec<File>), Error> {
    let of = |kind: &'static str| children.iter().copied().filter(move |c| c.kind() == kind);
    let tools = of("tool")
        .map(|t| read_tool(t, r))
        .collect::<Result<Vec<_>, _>>()?;
    check_unique(
        tools.iter().map(|t| (&t.name, "tool")),
        &of("tool").collect::<Vec<_>>(),
        r,
    )?;
    let mut files = Vec::new();
    for f in of("script").chain(of("source")) {
        files.push(read_file(f, r)?);
    }
    check_unique(
        files
            .iter()
            .map(|f| (&f.name, if f.script { "script" } else { "source" })),
        &of("script").chain(of("source")).collect::<Vec<_>>(),
        r,
    )?;
    Ok((tools, files))
}

fn check_children(children: &[&Node], r: &Resolver) -> Result<(), Error> {
    for c in children {
        if !CHILDREN.contains(&c.kind()) {
            let help = match did_you_mean(c.kind(), &CHILDREN) {
                Some(k) => format!("did you mean `{k}`?"),
                None => format!("a command's children are {}", CHILDREN.join(", ")),
            };
            return Err(r
                .error(c, c.origin(), format!("a command has no `{}`", c.kind()))
                .help(help));
        }
    }
    Ok(())
}

/// The child of `kind`, if any; a second is an error.
fn only<'n>(children: &[&'n Node], kind: &str, r: &Resolver) -> Result<Option<&'n Node>, Error> {
    let mut it = children.iter().copied().filter(|c| c.kind() == kind);
    let first = it.next();
    if let Some(second) = it.next() {
        return Err(r
            .error(
                second,
                second.origin(),
                format!("a command has one `{kind}`, and this is a second"),
            )
            .help("merge them into one"));
    }
    Ok(first)
}

/// The program to run: a declared tool runs its path; a relative path
/// is the config file's.
fn resolve_program(program: &mut Template, tools: &[Tool], at: &Origin) {
    let Some(p) = program.literal() else { return };
    let resolved = if let Some(t) = tools.iter().find(|t| t.name == p) {
        t.program.clone()
    } else if p.contains('/') && Path::new(&p).is_relative() {
        dir_of(at).join(&p).to_string_lossy().into_owned()
    } else {
        return;
    };
    *program = Template(vec![template::Piece::Lit(resolved)]);
}

/// Reports the chunk's frames as they appear, until `done`; returns the
/// last count reported.
fn watch_frames(
    ctx: &TaskContext<'_>,
    out: &Path,
    pattern: &(String, usize, String),
    iv: Interval,
    done: &AtomicBool,
) -> u64 {
    let total = u64::try_from(iv.len()).unwrap_or(0);
    let mut last = 0;
    while !done.load(Ordering::SeqCst) {
        let n = (iv.start.0..iv.end.0)
            .filter(|&f| out.join(frame_name(pattern, f)).is_file())
            .count() as u64;
        if n != last {
            ctx.progress(n, total);
            last = n;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    last
}

/// A temporary directory of links to a frame sequence's files, removed
/// when dropped.
struct LinkDir(PathBuf);

impl LinkDir {
    fn new(label: &str, dirs: &[PathBuf]) -> std::io::Result<Self> {
        use std::sync::atomic::AtomicU64;
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "videbimus-in-{}-{}-{label}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let made = Self(dir);
        for d in dirs {
            for e in std::fs::read_dir(d)? {
                let e = e?;
                link(&e.path(), &made.0.join(e.file_name()))?;
            }
        }
        Ok(made)
    }
}

impl Drop for LinkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn link(from: &Path, to: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(from, to)
}

#[cfg(not(unix))]
fn link(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::hard_link(from, to).or_else(|_| std::fs::copy(from, to).map(|_| ()))
}

fn frame_name((pre, width, post): &(String, usize, String), f: i64) -> String {
    format!("{pre}{f:0width$}{post}")
}

/// `3–5, 9`.
fn ranges(frames: &[i64]) -> String {
    let mut parts = Vec::new();
    let mut i = 0;
    while i < frames.len() {
        let mut j = i;
        while j + 1 < frames.len() && frames[j + 1] == frames[j] + 1 {
            j += 1;
        }
        parts.push(if i == j {
            frames[i].to_string()
        } else {
            format!("{}–{}", frames[i], frames[j])
        });
        i = j + 1;
    }
    parts.join(", ")
}

/// `; it wrote a, b, c` (or `; $out is empty`), for a missing-output error.
fn wrote(out: &Path) -> String {
    let mut names: Vec<String> = std::fs::read_dir(out)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    match names.len() {
        0 => "; $out is empty".to_owned(),
        n if n <= 5 => format!("; it wrote {}", names.join(", ")),
        n => format!("; it wrote {}, … ({n} entries)", names[..5].join(", ")),
    }
}

/// The directory of the file `origin` is written in.
fn dir_of(origin: &Origin) -> PathBuf {
    let dir = origin
        .at
        .path()
        .parent()
        .unwrap_or(Path::new(""))
        .to_owned();
    std::path::absolute(&dir).unwrap_or(dir)
}

fn resolve_path(origin: &Origin, p: &str) -> PathBuf {
    let p = PathBuf::from(p);
    if p.is_absolute() {
        p
    } else {
        dir_of(origin).join(p)
    }
}

/// A value as command-line text: strings, and numbers as written.
fn text(node: &Node, v: &Located<Value>, r: &Resolver, what: &str) -> Result<String, Error> {
    match &v.value {
        Value::String(s) => Ok(s.clone()),
        #[allow(clippy::cast_possible_truncation)]
        Value::Number(n) if n.fract() == 0.0 && n.abs() < 9e15 => Ok((*n as i64).to_string()),
        Value::Number(n) => Ok(n.to_string()),
        other => Err(r
            .error(
                node,
                &v.origin,
                format!("{} {what} is {other:?}, not text", node.kind()),
            )
            .help("quote it: \"…\"")),
    }
}

fn check_unique<'a>(
    names: impl Iterator<Item = (&'a String, &'static str)>,
    nodes: &[&Node],
    r: &Resolver,
) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    for ((name, what), node) in names.zip(nodes) {
        if !seen.insert(name.clone()) {
            return Err(r
                .error(
                    node,
                    node.origin(),
                    format!("a second {what} is named `{name}`"),
                )
                .help("give each its own name=\"…\""));
        }
    }
    Ok(())
}

fn read_tool(t: &Node, r: &Resolver) -> Result<Tool, Error> {
    r.check_props(t, &["path", "version-from", "version-match", "version"])?;
    if t.args().len() != 1 {
        return Err(r
            .error(t, t.origin(), "a tool has one name")
            .help("tool \"blender\" version-from=\"blender --version\""));
    }
    let name: String = r.arg(t, 0)?;
    let path: Option<String> = r.opt(t, "path")?;
    // A bare program name is looked up on PATH; a path is the config
    // file's.
    let program = match &path {
        Some(p) if p.contains('/') => resolve_path(&t.prop("path").expect("read").origin, p)
            .to_string_lossy()
            .into_owned(),
        Some(p) => p.clone(),
        None => name.clone(),
    };
    let at = |k: &str| videbimus_runtime::registry::origin_of(t, k);
    let fixed: Option<String> = r.opt(t, "version")?;
    let from: Option<String> = r.opt(t, "version-from")?;
    let pattern = match r.opt::<String>(t, "version-match")? {
        Some(p) => Some(Regex::new(&p).map_err(|e| {
            r.error(
                t,
                &at("version-match"),
                format!("`version-match` isn't a regex: {e}"),
            )
            .help("a regex whose first group is the version: \"Blender ([0-9.]+)\"")
        })?),
        None => None,
    };
    let version = match (fixed, from) {
        (Some(_), Some(_)) => {
            return Err(r
                .error(
                    t,
                    &at("version"),
                    "a tool has `version` or `version-from`, not both",
                )
                .help("keep `version-from` to probe it, or `version` to state it"));
        }
        (Some(v), None) => v,
        (None, Some(probe)) => {
            let mut argv = shlex::split(&probe)
                .filter(|a| !a.is_empty())
                .ok_or_else(|| {
                    r.error(
                        t,
                        &at("version-from"),
                        format!("`version-from` isn't a command line: {probe:?}"),
                    )
                    .help("version-from=\"blender --version\"")
                })?;
            if argv.is_empty() {
                return Err(r
                    .error(t, &at("version-from"), "`version-from` is empty")
                    .help("version-from=\"blender --version\""));
            }
            if argv[0] == name {
                argv[0].clone_from(&program);
            }
            tool::version(&name, &probe, &argv, pattern.as_ref(), path.is_some()).map_err(|e| {
                let mut err = r.error(t, &at("version-from"), e.message).help(e.help);
                for n in e.notes {
                    err = err.note(n);
                }
                err
            })?
        }
        (None, None) => {
            return Err(r
                .error(t, t.origin(), format!("tool `{name}` has no version"))
                .help(format!(
                    "add version-from=\"{name} --version\" to probe it, or version=\"…\""
                )));
        }
    };
    Ok(Tool {
        name,
        program,
        version,
    })
}

fn read_file(f: &Node, r: &Resolver) -> Result<File, Error> {
    r.check_props(f, &["name"])?;
    let script = f.kind() == "script";
    if f.args().len() != 1 {
        return Err(r
            .error(f, f.origin(), format!("a {} has one path", f.kind()))
            .help(format!(
                "{} \"hwscene.py\", relative to the config file",
                f.kind()
            )));
    }
    let given: String = r.arg(f, 0)?;
    let at = &f.args()[0].origin;
    let path = resolve_path(at, &given);
    let name = match r.opt::<String>(f, "name")? {
        Some(n) => n,
        None => path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(r
            .error(f, f.origin(), format!("`{name}` can't name a {}", f.kind()))
            .help("give it a name of letters, digits, `_` and `-`: name=\"scene\""));
    }
    let digest = hash_path(&path).map_err(|e| {
        r.error(
            f,
            at,
            format!("can't read {} `{}`: {e}", f.kind(), path.display()),
        )
        .help("paths are relative to the config file")
    })?;
    Ok(File {
        script,
        name,
        path,
        digest,
    })
}

/// A file's contents, or a directory's files' relative paths and
/// contents, hashed.
fn hash_path(path: &Path) -> std::io::Result<[u8; 32]> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for e in std::fs::read_dir(dir)? {
            let p = e?.path();
            if p.is_dir() {
                walk(root, &p, out)?;
            } else {
                out.push(p.strip_prefix(root).unwrap_or(&p).to_owned());
            }
        }
        Ok(())
    }
    let mut h = blake3::Hasher::new();
    if path.is_dir() {
        let mut files = Vec::new();
        walk(path, path, &mut files)?;
        files.sort();
        h.update(b"dir\0");
        for rel in files {
            let rel_s = rel.to_string_lossy().replace('\\', "/");
            let bytes = std::fs::read(path.join(&rel))?;
            h.update(rel_s.as_bytes());
            h.update(&[0]);
            h.update(&(bytes.len() as u64).to_le_bytes());
            h.update(&bytes);
        }
    } else {
        h.update(b"file\0");
        h.update(&std::fs::read(path)?);
    }
    Ok(*h.finalize().as_bytes())
}

fn read_output(asset: &Node, outs: &[&Node], r: &Resolver) -> Result<Output, Error> {
    let usage = "declare what it writes into $out: output file=\"shot.mp4\", output dir=\"stems\", or output frames=\"####.png\" from=(f)1 to=(f)100";
    let Some(first) = outs.first() else {
        return Err(r
            .error(asset, asset.origin(), "a command needs an `output`")
            .help(usage));
    };
    let mut single = Vec::new();
    let mut frames = None;
    for o in outs {
        r.check_props(o, &["file", "dir", "frames", "from", "to", "chunk"])?;
        if !o.args().is_empty() {
            return Err(r
                .error(o, o.origin(), "`output` takes properties")
                .help(usage));
        }
        let at = |k: &str| videbimus_runtime::registry::origin_of(o, k);
        let given: Vec<&str> = ["file", "dir", "frames"]
            .into_iter()
            .filter(|k| o.prop(k).is_some())
            .collect();
        if given.len() != 1 {
            return Err(r
                .error(
                    o,
                    o.origin(),
                    "an `output` is one of `file`, `dir` or `frames`",
                )
                .help(usage));
        }
        for k in ["from", "to", "chunk"] {
            if given[0] != "frames" && o.prop(k).is_some() {
                return Err(r
                    .error(o, &at(k), format!("`{k}` is for a frame output"))
                    .help("output frames=\"####.png\" from=(f)1 to=(f)100 chunk=10"));
            }
        }
        match given[0] {
            "file" => single.push(Expect::File(r.get(o, "file")?)),
            "dir" => single.push(Expect::Dir(r.get(o, "dir")?)),
            _ => {
                let shown: String = r.get(o, "frames")?;
                let pattern = split_pattern(&shown).ok_or_else(|| {
                    r.error(
                        o,
                        &at("frames"),
                        format!("the pattern \"{shown}\" has no place for the frame number"),
                    )
                    .help("mark it with #, one per digit: \"####.png\" is 0001.png")
                })?;
                let iv = r.interval(o, "from", "to")?;
                if iv.is_empty() {
                    return Err(r
                        .error(o, &at("to"), "the frame output has no frames")
                        .help(
                            "`to` is one past the last frame: from=(f)1 to=(f)101 is frames 1–100",
                        ));
                }
                let chunk: Option<u32> = r.opt(o, "chunk")?;
                if chunk == Some(0) {
                    return Err(r
                        .error(o, &at("chunk"), "a chunk has at least one frame")
                        .help("chunk=10"));
                }
                frames = Some(Output::Frames {
                    pattern,
                    shown,
                    frames: iv,
                    chunk,
                });
            }
        }
    }
    match frames {
        Some(f) if outs.len() == 1 => Ok(f),
        Some(_) => Err(r
            .error(
                first,
                first.origin(),
                "a frame output is a command's only output",
            )
            .help("split the other outputs into their own command asset")),
        None => Ok(Output::Single(single)),
    }
}

fn split_pattern(p: &str) -> Option<(String, usize, String)> {
    let start = p.find('#')?;
    let width = p[start..].chars().take_while(|&c| c == '#').count();
    Some((p[..start].to_owned(), width, p[start + width..].to_owned()))
}

fn read_cost(c: &Node, r: &Resolver) -> Result<(Cost, Resources, bool), Error> {
    r.check_props(
        c,
        &["per-frame", "per-run", "cores", "mem", "gpu", "remote"],
    )?;
    let at = |k: &str| videbimus_runtime::registry::origin_of(c, k);
    let seconds = |k: &str| -> Result<Option<f64>, Error> {
        let Some(v) = c.prop(k) else { return Ok(None) };
        let s = match &v.value {
            Value::Number(n) => Some(*n),
            Value::Time(t) if t.unit == Unit::Second => Some(t.amount),
            Value::Time(t) => r.timebase().and_then(|tb| tb.seconds(*t).ok()),
            _ => None,
        };
        match s {
            Some(s) if s.is_finite() && s >= 0.0 => Ok(Some(s)),
            _ => Err(r
                .error(c, &at(k), format!("`{k}` is a number of seconds"))
                .help(format!("{k}=20, or {k}=(s)20"))),
        }
    };
    let cost = match (seconds("per-frame")?, seconds("per-run")?) {
        (Some(_), Some(_)) => {
            return Err(r
                .error(
                    c,
                    &at("per-run"),
                    "a cost is `per-frame` or `per-run`, not both",
                )
                .help("per-frame for a frame output, per-run for one run of the command"));
        }
        (Some(s), None) => Cost::PerFrame(s),
        (None, Some(s)) => Cost::Seconds(s),
        (None, None) => Cost::Seconds(60.0),
    };
    let cores: u32 = r.opt(c, "cores")?.unwrap_or(1);
    let mem = match c.prop("mem") {
        None => 0,
        Some(v) => parse_bytes(&v.value).ok_or_else(|| {
            r.error(c, &at("mem"), "`mem` is a size in bytes")
                .help("mem=\"512M\" or mem=\"3G\" (powers of 1024), or a number of bytes")
        })?,
    };
    let gpu: bool = r.opt(c, "gpu")?.unwrap_or(false);
    let remote: bool = r.opt(c, "remote")?.unwrap_or(false);
    let mut res = Resources::new(cores, mem);
    if gpu {
        res = res.with_gpu();
    }
    Ok((cost, res, remote))
}

/// `3G`, `512M`, `1.5GB`, `100MiB` or a number of bytes.
fn parse_bytes(v: &Value) -> Option<u64> {
    let (n, mult) = match v {
        Value::Number(n) => (*n, 1.0),
        Value::String(s) => {
            let s = s.trim();
            let split = s
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .unwrap_or(s.len());
            let n: f64 = s[..split].parse().ok()?;
            let unit = s[split..].trim().to_ascii_uppercase();
            let mult = match unit.trim_end_matches('B').trim_end_matches('I') {
                "" => 1.0,
                "K" => 1024.0,
                "M" => 1024.0 * 1024.0,
                "G" => 1024.0 * 1024.0 * 1024.0,
                "T" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
                _ => return None,
            };
            (n, mult)
        }
        _ => return None,
    };
    let bytes = n * mult;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (bytes.is_finite() && bytes >= 0.0).then_some(bytes as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes() {
        assert_eq!(parse_bytes(&Value::String("3G".into())), Some(3 << 30));
        assert_eq!(
            parse_bytes(&Value::String("512MiB".into())),
            Some(512 << 20)
        );
        assert_eq!(parse_bytes(&Value::String("1.5GB".into())), Some(3 << 29));
        assert_eq!(parse_bytes(&Value::Number(1000.0)), Some(1000));
        assert_eq!(parse_bytes(&Value::String("lots".into())), None);
    }

    #[test]
    fn frame_ranges() {
        assert_eq!(ranges(&[3, 4, 5, 9, 11, 12]), "3–5, 9, 11–12");
        assert_eq!(ranges(&[7]), "7");
    }
}
