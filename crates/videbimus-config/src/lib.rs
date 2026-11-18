//! videbimus's config resolver ([ADR 0005]): KDL v2 in, a resolved tree of
//! typed, located values out.
//!
//! A config is a KDL v2 document. The resolver gives meaning to:
//!
//! - **`video`**, at the top level: the frame rate, and the song's tempo
//!   for times in bars and beats.
//!
//!   ```kdl
//!   video rate=30 {               // or rate="30000/1001"
//!       tempo bpm=104 beats-per-bar=4
//!   }
//!   ```
//!
//!   `video` stays in the tree, so its other properties (size, say) are
//!   there for whoever reads them.
//! - **Units**, as type annotations: `(bar)12`, `(beat)2`, `(s)0.5`,
//!   `(f)15`. A time counts from the start of the video; bars from bar 0.
//! - **References**: `$name` (a `let` or `each` binding, looked up from
//!   the innermost block out), `$self.key`, `$parent.key`, `$prev.key` (the
//!   previous sibling), and paths through them: `$parent.parent.key`,
//!   `$prev.camera.end` (a child of a resolved node, by kind or by name).
//!   Write `$$` for a string that starts with a literal `$`.
//! - **Run-time references**: a string starting with `$in`, `$out`,
//!   `$chunk`, `$cores`, `$script` or `$source` ([`RUN_TIME`]) is kept as
//!   written, for the recipe that runs a command to fill in when it runs
//!   (`run "blender" "$in.shot" "$out"`), and so is one starting with
//!   `$$` and one of them (the recipe's own escape: `"$$out"` is a
//!   literal `$out` there). Those names can't be bound with `let`.
//! - **Expressions**: `(expr)"$self.length + 0.5s"`, with `+ - * /` (or
//!   `− × ÷`), parentheses, `min`, `max`, `clamp` and `frames`; see
//!   [ADR 0005] for the unit rules. A `-` inside a name (`$beats-per-bar`)
//!   is part of the name when a letter follows it, so put spaces around
//!   subtraction.
//! - **`let`**: `let tail=(s)0.5 width=1920` binds names for its block and
//!   the blocks inside it.
//! - **Templates**: `template "close-up" key=value … { children }` at the
//!   top level of a file, applied with `use="close-up"`. The node's own
//!   properties override the template's; a child with the same kind and
//!   name as a template child merges into it; a template may itself
//!   `use` another.
//! - **Includes**: `include "shots.kdl"`, relative to the including file,
//!   spliced in place.
//! - **`each`**: `each "synth" "vxn1c" "vxn2b" index="i" { band $synth }`
//!   repeats its body once per literal item, binding `$synth` (and `$i`,
//!   from 0).
//!
//! Values resolve in document order, lazily: a property may refer to a
//! later property of its own node. Cycles are errors, as is every other
//! problem, and each [`Error`] names the file, line and column, the
//! element, the chain of templates, includes and `each`es that produced
//! the value, and the fix.
//!
//! Kinds read the resolved tree through a [`Resolver`]: typed, located
//! reads such as `resolver.get::<Frame>(node, "from")`.
//!
//! [ADR 0005]: ../../../adrs/0005-configuration.md

mod error;
mod expand;
mod expr;
mod node;
mod resolve;
mod source;
mod value;

use std::path::Path;

use videbimus_core::{Rate, Tempo};

pub use error::{Error, Origin, Via};
pub use node::{FromValue, Located, Mismatch, Node, Prop, Resolver};
pub use source::{FsLoader, Loader, Location, MemLoader, SourceFile};
pub use value::{NoTempo, Time, Timebase, Unit, Value};

use expand::RawNode;
use resolve::Resolving;

/// Reference names the config leaves as written, for a recipe to
/// substitute when it runs (ADR 0005 §2, ADR 0009 §3): a string that
/// starts with `$` (or `$$`) and one of these, then anything but a
/// letter, digit, `_` or `-`, is a plain string, kept as written.
pub const RUN_TIME: [&str; 6] = ["in", "out", "chunk", "cores", "script", "source"];

/// A resolved config: its top-level nodes, and the [`Resolver`] to read
/// them with.
#[derive(Clone, Debug)]
pub struct Config {
    nodes: Vec<Node>,
    resolver: Resolver,
}

impl Config {
    /// Reads and resolves the config at `path`, and the files it includes.
    ///
    /// # Errors
    ///
    /// On the first problem: a file that can't be read, invalid KDL, an
    /// unknown template or unit, a bad reference, a cycle, a unit error.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::load_with(path, &FsLoader)
    }

    /// As [`Config::load`], reading files through `loader`.
    ///
    /// # Errors
    ///
    /// As [`Config::load`].
    pub fn load_with(path: impl AsRef<Path>, loader: &dyn Loader) -> Result<Self, Error> {
        let expanded = expand::expand(path.as_ref(), loader)?;
        let timebase = timebase(&expanded)?;
        let nodes = Resolving::new(&expanded.scopes, timebase).block(&expanded.nodes)?;
        Ok(Self {
            nodes,
            resolver: Resolver { timebase },
        })
    }

    /// The top-level nodes, in order.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The top-level nodes of `kind`.
    pub fn nodes_of<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.nodes.iter().filter(move |n| n.kind() == kind)
    }

    /// Typed, located reads of the nodes.
    #[must_use]
    pub fn resolver(&self) -> &Resolver {
        &self.resolver
    }

    /// The frame rate and tempo, if the config has a `video` node.
    #[must_use]
    pub fn timebase(&self) -> Option<&Timebase> {
        self.resolver.timebase()
    }
}

/// Resolves the `video` node on its own, before anything else, for the
/// frame rate and tempo that the rest needs.
fn timebase(x: &expand::Expanded) -> Result<Option<Timebase>, Error> {
    let videos: Vec<&RawNode> = x.nodes.iter().filter(|n| n.kind == "video").collect();
    let Some(raw) = videos.first() else {
        return Ok(None);
    };
    if let Some(second) = videos.get(1) {
        return Err(
            Error::new("a config has one `video` node, and this is a second")
                .at(second.origin.clone())
                .element("video")
                .note(format!("the first is at {}", raw.origin.at))
                .help("merge them into one"),
        );
    }
    let r = Resolver::default();
    let video = Resolving::new(&x.scopes, None).lone(raw)?;
    let rate = match video.prop("rate") {
        None => {
            return Err(Error::new("`video` has no frame rate")
                .at(video.origin().clone())
                .element("video")
                .help("add rate=30, or rate=\"30000/1001\" for a fractional rate"));
        }
        Some(v) => rate(&video, v, &r)?,
    };
    let tempos: Vec<&Node> = video.children_of("tempo").collect();
    let tempo = match tempos.as_slice() {
        [] => None,
        [t] => {
            r.check_props(t, &["bpm", "beats-per-bar"])?;
            let bpm: f64 = r.get(t, "bpm")?;
            let beats_per_bar: u32 = r.get(t, "beats-per-bar")?;
            if !(bpm.is_finite() && bpm > 0.0) {
                let at = t
                    .prop("bpm")
                    .map_or_else(|| t.origin().clone(), |v| v.origin.clone());
                return Err(r
                    .error(t, &at, format!("a tempo of {bpm} BPM can't be played"))
                    .help("give a positive tempo: bpm=120"));
            }
            if beats_per_bar == 0 {
                let at = t
                    .prop("beats-per-bar")
                    .map_or_else(|| t.origin().clone(), |v| v.origin.clone());
                return Err(r
                    .error(t, &at, "a bar needs at least one beat")
                    .help("beats-per-bar=4"));
            }
            Some(Tempo { bpm, beats_per_bar })
        }
        [_, second, ..] => {
            return Err(r
                .error(second, second.origin(), "`video` has two tempos")
                .help("one constant tempo is supported; remove one"));
        }
    };
    Ok(Some(Timebase { rate, tempo }))
}

#[allow(clippy::many_single_char_names)]
fn rate(video: &Node, v: &Located<Value>, r: &Resolver) -> Result<Rate, Error> {
    let fail = |message: String| {
        r.error(video, &v.origin, message)
            .element("video, property `rate`")
            .help("write a whole number of frames per second, rate=30, or a fraction as a string, rate=\"30000/1001\"")
    };
    let parse = |s: &str| s.trim().parse::<u32>().ok().filter(|&n| n > 0);
    match &v.value {
        Value::Number(_) => {
            let n: u32 = r.get(video, "rate")?;
            if n == 0 {
                return Err(fail("a frame rate of 0 shows nothing".into()));
            }
            Ok(Rate::new(n, 1))
        }
        Value::String(s) => match s.split_once('/') {
            Some((a, b)) => match (parse(a), parse(b)) {
                (Some(num), Some(den)) => Ok(Rate::new(num, den)),
                _ => Err(fail(format!("`{s}` isn't a frame rate"))),
            },
            None => Err(fail(format!("`{s}` isn't a frame rate"))),
        },
        other => Err(fail(format!(
            "expected a frame rate, found {}",
            other.describe()
        ))),
    }
}
