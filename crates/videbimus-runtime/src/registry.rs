//! Building components from config ([ADR 0009] §4).
//!
//! A component kind implements [`ComponentKind`]: its name in config, and
//! `from_config`, which reads its node through the [`Resolver`] and
//! returns the component. A [`ComponentRegistry`] maps kind names to
//! kinds; the built-in kinds are registered the same way as a project's
//! own.
//!
//! # Time and placement
//!
//! Times in config count from the start of the video (ADR 0005 §6), so
//! every boundary snaps to a frame from its own absolute position (ADR
//! 0001 §4). A component's interval, though, is in its parent's frames
//! (ADR 0002 §1). The [`Builder`] passed to `from_config` knows where the
//! parent starts, and [`Builder::interval`] reads a node's placement:
//!
//! - `from`: its first frame, a time. Defaults to the start of the span
//!   the parent offers (a sequence offers the end of the previous child).
//! - `to`: the frame after its last, a time; or `length`, a duration.
//!   Defaults to the end of the span the parent offers, if it offers one.
//!
//! A child may not start before its parent.
//!
//! # Properties a parent reads from its children
//!
//! A stack reads `opacity`, `blend`, `fade-in` and the like from each
//! child's own node. [`Builder::check_props`] accepts the placement
//! properties and the ones the parent declares, as well as the kind's
//! own, so kinds should check their properties through it rather than
//! [`Resolver::check_props`].
//!
//! [ADR 0009]: ../../../../adrs/0009-using-videbimus.md

use std::collections::BTreeMap;
use std::path::PathBuf;

use videbimus_config::{Error, Node, Resolver};
use videbimus_core::{Component, Frame, Interval};

use crate::drive::{Child, DriveOptions};

/// A component kind: how config makes one ([ADR 0009] §4).
///
/// ```
/// use videbimus_config::{Error, Node, Resolver};
/// use videbimus_core::{Component, Context, Image, Interval, Frame};
/// use videbimus_runtime::{Builder, ComponentKind};
///
/// /// A flat colour.
/// struct Flat { interval: Interval, grey: f32 }
///
/// impl Component for Flat {
///     fn interval(&self) -> Interval { self.interval }
///     fn reset(&mut self, _: &Context) {}
///     fn render(&mut self, _: Frame, _: &Context, out: &mut Image) {
///         out.fill([self.grey, self.grey, self.grey, 1.0]);
///     }
/// }
///
/// impl ComponentKind for Flat {
///     const KIND: &'static str = "flat";
///     fn from_config(
///         node: &Node,
///         r: &Resolver,
///         b: &Builder,
///     ) -> Result<Box<dyn Component>, Error> {
///         b.check_props(node, r, &["grey"])?;
///         Ok(Box::new(Flat {
///             interval: b.interval(node, r)?,
///             grey: r.opt::<f64>(node, "grey")?.unwrap_or(0.5) as f32,
///         }))
///     }
/// }
/// ```
///
/// [ADR 0009]: ../../../adrs/0009-using-videbimus.md
pub trait ComponentKind {
    /// Its name in config: the KDL node name.
    const KIND: &'static str;

    /// Builds one from its resolved `node`, reading it through `r`. A
    /// container builds its children through `b`, which also places the
    /// node in its parent's timeline ([`Builder::interval`]). Does no
    /// heavy work: assets are declared through
    /// [`Component::needs`] and built by the scheduler.
    ///
    /// # Errors
    ///
    /// A located config error for anything wrong with the node.
    fn from_config(node: &Node, r: &Resolver, b: &Builder) -> Result<Box<dyn Component>, Error>;
}

/// Makes a component from its node: [`ComponentKind::from_config`].
pub type Constructor = fn(&Node, &Resolver, &Builder) -> Result<Box<dyn Component>, Error>;

/// Component kinds by name.
#[derive(Clone, Debug, Default)]
pub struct ComponentRegistry {
    kinds: BTreeMap<&'static str, Constructor>,
}

impl ComponentRegistry {
    /// A registry with no kinds.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// A registry with the built-in kinds: `sequence`, `stack`,
    /// `crossfade`, `filter`, `frame-player`, `image` and `caption`.
    #[must_use]
    pub fn with_builtins() -> Self {
        use crate::kinds::{Caption, Crossfade, Filter, FramePlayer, Picture, Sequence, Stack};
        Self::empty()
            .with::<Sequence>()
            .with::<Stack>()
            .with::<Crossfade>()
            .with::<Filter>()
            .with::<FramePlayer>()
            .with::<Picture>()
            .with::<Caption>()
    }

    /// Adds kind `K`.
    ///
    /// # Panics
    ///
    /// If a kind of that name is already registered: two kinds can't share
    /// a name in config.
    pub fn register<K: ComponentKind>(&mut self) -> &mut Self {
        self.register_fn(K::KIND, K::from_config)
    }

    /// Adds kind `K`, by value.
    ///
    /// # Panics
    ///
    /// As [`register`](Self::register).
    #[must_use]
    pub fn with<K: ComponentKind>(mut self) -> Self {
        self.register::<K>();
        self
    }

    /// Adds a kind by name and constructor, for kinds that aren't types.
    ///
    /// # Panics
    ///
    /// As [`register`](Self::register).
    pub fn register_fn(&mut self, kind: &'static str, make: Constructor) -> &mut Self {
        assert!(
            self.kinds.insert(kind, make).is_none(),
            "component kind `{kind}` is registered twice; kinds need distinct names"
        );
        self
    }

    /// Whether `kind` is registered.
    #[must_use]
    pub fn contains(&self, kind: &str) -> bool {
        self.kinds.contains_key(kind)
    }

    /// The registered kinds' names, sorted.
    pub fn kinds(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.kinds.keys().copied()
    }

    /// A builder for top-level components: their parent is the video,
    /// starting at frame 0, with no end.
    #[must_use]
    pub fn builder(&self, options: DriveOptions) -> Builder<'_> {
        Builder {
            registry: self,
            options,
            parent_start: Frame(0),
            from: Frame(0),
            to: None,
            parent_props: &[],
        }
    }

    /// Builds the top-level component `node`, with default drive options.
    ///
    /// # Errors
    ///
    /// As [`Builder::build`].
    pub fn build(&self, node: &Node, r: &Resolver) -> Result<Box<dyn Component>, Error> {
        self.builder(DriveOptions::default()).build(node, r)
    }
}

/// What a kind's `from_config` is given to place itself and build its
/// children.
#[derive(Clone, Copy, Debug)]
pub struct Builder<'a> {
    registry: &'a ComponentRegistry,
    options: DriveOptions,
    /// The parent's first frame, absolute.
    parent_start: Frame,
    /// The default `from`, absolute.
    from: Frame,
    /// The default `to`, absolute.
    to: Option<Frame>,
    /// Properties the parent reads from its children's nodes.
    parent_props: &'static [&'static str],
}

/// The properties every component node may have: its placement.
pub const PLACEMENT_PROPS: [&str; 3] = ["from", "to", "length"];

impl<'a> Builder<'a> {
    /// The registry it builds from.
    #[must_use]
    pub fn registry(&self) -> &'a ComponentRegistry {
        self.registry
    }

    /// How children are driven.
    #[must_use]
    pub fn options(&self) -> DriveOptions {
        self.options
    }

    /// The span offered to the node being built, in absolute frames:
    /// its default `from` and `to`.
    #[must_use]
    pub fn span(&self) -> (Frame, Option<Frame>) {
        (self.from, self.to)
    }

    /// Builds `node` with the kind its name gives.
    ///
    /// # Errors
    ///
    /// If the kind isn't registered (suggesting the nearest that is), or
    /// the kind's own errors.
    pub fn build(&self, node: &Node, r: &Resolver) -> Result<Box<dyn Component>, Error> {
        match self.registry.kinds.get(node.kind()) {
            Some(make) => make(node, r, self),
            None => Err(self.unknown(node)),
        }
    }

    /// Builds `node` and wraps it to be driven ([`Child`]).
    ///
    /// # Errors
    ///
    /// As [`build`](Self::build).
    pub fn build_child(&self, node: &Node, r: &Resolver) -> Result<Child, Error> {
        Ok(Child::new(self.build(node, r)?, self.options))
    }

    fn unknown(&self, node: &Node) -> Error {
        let known: Vec<&str> = self.registry.kinds().collect();
        let help = match did_you_mean(node.kind(), &known) {
            Some(k) => format!("did you mean `{k}`?"),
            None if known.is_empty() => "no component kinds are registered".to_owned(),
            None => format!("the component kinds are {}", known.join(", ")),
        };
        Error::new(format!("there is no component kind `{}`", node.kind()))
            .at(node.origin().clone())
            .element(node.describe())
            .help(help)
    }

    /// The builder for the children of a component placed at `interval`
    /// (in this builder's parent's frames): their parent starts there,
    /// and they are offered its whole span.
    #[must_use]
    pub fn inside(&self, interval: Interval) -> Self {
        let start = self.parent_start + interval.start.0;
        Self {
            parent_start: start,
            from: start,
            to: Some(self.parent_start + interval.end.0),
            parent_props: &[],
            ..*self
        }
    }

    /// The same, offering the span from `from` (in the parent's frames,
    /// that is, relative to the component this builder builds children
    /// for) to `to`, if given, else to where it ended.
    #[must_use]
    pub fn offering(&self, from: Frame, to: Option<Frame>) -> Self {
        Self {
            from: self.parent_start + from.0,
            to: to.map(|t| self.parent_start + t.0).or(self.to),
            ..*self
        }
    }

    /// The same, with `props` accepted on each child's node for the
    /// parent to read.
    #[must_use]
    pub fn with_parent_props(&self, props: &'static [&'static str]) -> Self {
        Self {
            parent_props: props,
            ..*self
        }
    }

    /// Fails on any property of `node` that isn't in `own`, the placement
    /// properties ([`PLACEMENT_PROPS`]) or the parent's.
    ///
    /// # Errors
    ///
    /// On the first unknown property, suggesting the near miss.
    pub fn check_props(&self, node: &Node, r: &Resolver, own: &[&str]) -> Result<(), Error> {
        let known: Vec<&str> = own
            .iter()
            .chain(&PLACEMENT_PROPS)
            .chain(self.parent_props)
            .copied()
            .collect();
        r.check_props(node, &known)
    }

    /// `node`'s interval, in its parent's frames, from `from` and `to` or
    /// `length` (see the [module docs](self)).
    ///
    /// # Errors
    ///
    /// If a value isn't a time, both `to` and `length` are given, neither
    /// is and the parent offers no end, it ends before it starts, or it
    /// starts before its parent.
    pub fn interval(&self, node: &Node, r: &Resolver) -> Result<Interval, Error> {
        let from: Frame = r.opt(node, "from")?.unwrap_or(self.from);
        let to = match (node.prop("to"), node.prop("length")) {
            (Some(_), Some(len)) => {
                return Err(r
                    .error(node, &len.origin, "both `to` and `length` are given")
                    .help("give one: `to` is where it ends, `length` how long it lasts"));
            }
            (Some(_), None) => r.get::<Frame>(node, "to")?,
            (None, Some(_)) => {
                let len: Frame = r.get(node, "length")?;
                if len.0 < 0 {
                    return Err(r
                        .error(
                            node,
                            &len_origin(node),
                            format!("a length of {len} is negative"),
                        )
                        .help("give a length of zero or more"));
                }
                from + len.0
            }
            (None, None) => self.to.ok_or_else(|| {
                r.error(node, node.origin(), "it has no end")
                    .help("add to=… (where it ends) or length=… (how long it lasts)")
            })?,
        };
        if to < from {
            let at = node
                .prop("to")
                .map_or_else(|| node.origin().clone(), |v| v.origin.clone());
            return Err(r
                .error(
                    node,
                    &at,
                    format!("it ends ({to}) before it starts ({from})"),
                )
                .help("check `from` and `to`"));
        }
        if from < self.parent_start {
            let at = node
                .prop("from")
                .map_or_else(|| node.origin().clone(), |v| v.origin.clone());
            return Err(r
                .error(
                    node,
                    &at,
                    format!(
                        "it starts ({from}) before its parent does ({})",
                        self.parent_start
                    ),
                )
                .help("times count from the start of the video, not of the parent; start it at or after the parent"));
        }
        Ok(Interval::new(from, to).shift(-self.parent_start.0))
    }
}

fn len_origin(node: &Node) -> videbimus_config::Origin {
    origin_of(node, "length")
}

/// Where property `key` of `node` is written, or where the node is if it
/// has no such property: for errors about a property.
#[must_use]
pub fn origin_of(node: &Node, key: &str) -> videbimus_config::Origin {
    node.prop(key)
        .map_or_else(|| node.origin().clone(), |v| v.origin.clone())
}

/// A string property naming a file, resolved against the directory of the
/// config file the property is written in. `None` if absent.
///
/// # Errors
///
/// If it is present and isn't a string.
pub fn path_prop(node: &Node, r: &Resolver, key: &str) -> Result<Option<PathBuf>, Error> {
    let Some(s) = r.opt::<String>(node, key)? else {
        return Ok(None);
    };
    let path = PathBuf::from(&s);
    if path.is_absolute() {
        return Ok(Some(path));
    }
    let origin = &crate::registry::origin_of(node, key);
    let dir = origin
        .at
        .path()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_default();
    Ok(Some(dir.join(path)))
}

/// The registered name nearest to `name`, if one is near enough to be a
/// likely misspelling.
#[must_use]
pub fn did_you_mean<'k>(name: &str, known: &[&'k str]) -> Option<&'k str> {
    known
        .iter()
        .map(|k| (levenshtein(name, k), *k))
        .filter(|&(d, k)| d <= (k.len().max(name.len()) / 3).max(1))
        .min()
        .map(|(_, k)| k)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let next = (prev + usize::from(ca != cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_misses() {
        let known = ["sequence", "stack", "crossfade", "frame-player"];
        assert_eq!(did_you_mean("sequnce", &known), Some("sequence"));
        assert_eq!(did_you_mean("stak", &known), Some("stack"));
        assert_eq!(did_you_mean("frameplayer", &known), Some("frame-player"));
        assert_eq!(did_you_mean("banana", &known), None);
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn names_are_unique() {
        let _ = ComponentRegistry::with_builtins().with::<crate::kinds::Stack>();
    }
}
