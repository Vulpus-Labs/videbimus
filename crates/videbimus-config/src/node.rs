//! The resolved tree, and typed, located access to it.

use videbimus_core::{Frame, Interval};

use crate::error::{Error, Origin, suggest};
use crate::expand::describe;
use crate::value::{Time, Timebase, Value};

/// A value and where it came from.
#[derive(Clone, Debug)]
pub struct Located<T> {
    /// The value.
    pub value: T,
    /// Where it is written, and the templates, includes and `each`
    /// expansions that put it here.
    pub origin: Origin,
}

/// A property: its key and value.
#[derive(Clone, Debug)]
pub struct Prop {
    /// The key.
    pub key: String,
    /// The resolved value.
    pub value: Located<Value>,
}

/// A resolved node: templates applied, references and arithmetic
/// resolved, `let`, `include`, `template` and `each` gone.
#[derive(Clone, Debug)]
pub struct Node {
    pub(crate) kind: String,
    pub(crate) origin: Origin,
    pub(crate) args: Vec<Located<Value>>,
    pub(crate) props: Vec<Prop>,
    pub(crate) children: Vec<Node>,
}

impl Node {
    /// The node's kind: `shot`, `sequence`, `key`.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Its name: the first argument, if that is a string.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.args.first()?.value.as_str()
    }

    /// `shot "s02"`, or `key`: how messages name it.
    #[must_use]
    pub fn describe(&self) -> String {
        describe(&self.kind, self.name())
    }

    /// Where the node is written.
    #[must_use]
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// Its arguments, in order (the name is the first).
    #[must_use]
    pub fn args(&self) -> &[Located<Value>] {
        &self.args
    }

    /// Its properties, in order: a template's first, then its own.
    #[must_use]
    pub fn props(&self) -> &[Prop] {
        &self.props
    }

    /// The property `key`.
    #[must_use]
    pub fn prop(&self, key: &str) -> Option<&Located<Value>> {
        self.props.iter().find(|p| p.key == key).map(|p| &p.value)
    }

    /// Its children, in order.
    #[must_use]
    pub fn children(&self) -> &[Node] {
        &self.children
    }

    /// Its children of `kind`.
    pub fn children_of<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.kind == kind)
    }
}

/// Why a value isn't the type asked for.
#[derive(Clone, Debug)]
pub struct Mismatch {
    /// What was expected and what was found.
    pub message: String,
    /// The fix, if there's an obvious one.
    pub help: Option<String>,
}

impl Mismatch {
    fn expected(what: &str, v: &Value, help: Option<String>) -> Self {
        Self {
            message: format!("expected {what}, found {}", v.describe()),
            help,
        }
    }
}

/// A type a config value can be read as.
pub trait FromValue: Sized {
    /// Reads `value`, converting times through `timebase` when needed.
    ///
    /// # Errors
    ///
    /// If `value` isn't this type.
    fn from_value(value: &Value, timebase: Option<&Timebase>) -> Result<Self, Mismatch>;
}

fn unit_help(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(format!(
            "give it a unit: (bar){n}, (beat){n}, (s){n} or (f){n}"
        )),
        _ => None,
    }
}

impl FromValue for Value {
    fn from_value(value: &Value, _: Option<&Timebase>) -> Result<Self, Mismatch> {
        Ok(value.clone())
    }
}

impl FromValue for f64 {
    fn from_value(value: &Value, _: Option<&Timebase>) -> Result<Self, Mismatch> {
        match value {
            Value::Number(n) => Ok(*n),
            Value::Time(_) => Err(Mismatch::expected(
                "a plain number",
                value,
                Some("drop the unit".into()),
            )),
            Value::String(s) if s.parse::<f64>().is_ok() => Err(Mismatch::expected(
                "a number",
                value,
                Some(format!("remove the quotes: {s}")),
            )),
            _ => Err(Mismatch::expected("a number", value, None)),
        }
    }
}

impl FromValue for i64 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn from_value(value: &Value, tb: Option<&Timebase>) -> Result<Self, Mismatch> {
        let n = f64::from_value(value, tb).map_err(|m| Mismatch {
            message: m.message.replace("a number", "a whole number"),
            ..m
        })?;
        if n.fract() != 0.0 || n.abs() > (1u64 << 53) as f64 {
            return Err(Mismatch::expected(
                "a whole number",
                value,
                Some(format!("round it: {}", n.round())),
            ));
        }
        Ok(n as i64)
    }
}

impl FromValue for u32 {
    fn from_value(value: &Value, tb: Option<&Timebase>) -> Result<Self, Mismatch> {
        let n = i64::from_value(value, tb)?;
        u32::try_from(n).map_err(|_| {
            Mismatch::expected(
                &format!("a whole number from 0 to {}", u32::MAX),
                value,
                None,
            )
        })
    }
}

impl FromValue for bool {
    fn from_value(value: &Value, _: Option<&Timebase>) -> Result<Self, Mismatch> {
        match value {
            Value::Bool(b) => Ok(*b),
            Value::String(s) if s == "true" || s == "false" => Err(Mismatch::expected(
                "#true or #false",
                value,
                Some(format!("KDL v2 writes booleans with a #: #{s}")),
            )),
            _ => Err(Mismatch::expected("#true or #false", value, None)),
        }
    }
}

impl FromValue for String {
    fn from_value(value: &Value, _: Option<&Timebase>) -> Result<Self, Mismatch> {
        match value {
            Value::String(s) => Ok(s.clone()),
            Value::Number(n) => Err(Mismatch::expected(
                "a string",
                value,
                Some(format!("quote it: \"{n}\"")),
            )),
            _ => Err(Mismatch::expected("a string", value, None)),
        }
    }
}

impl FromValue for Time {
    fn from_value(value: &Value, _: Option<&Timebase>) -> Result<Self, Mismatch> {
        match value {
            Value::Time(t) => Ok(*t),
            _ => Err(Mismatch::expected("a time", value, unit_help(value))),
        }
    }
}

impl FromValue for Frame {
    fn from_value(value: &Value, tb: Option<&Timebase>) -> Result<Self, Mismatch> {
        let t = Time::from_value(value, tb)?;
        let Some(tb) = tb else {
            return Err(Mismatch {
                message: format!("{t} can't become a frame: the config has no frame rate"),
                help: Some("add `video rate=30 { tempo bpm=120 beats-per-bar=4 }` at the top of the config".into()),
            });
        };
        tb.frame(t).map_err(|e| Mismatch {
            message: format!("{t} can't become a frame: {e}"),
            help: Some("add `tempo bpm=… beats-per-bar=…` inside `video`".into()),
        })
    }
}

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(value: &Value, tb: Option<&Timebase>) -> Result<Self, Mismatch> {
        match value {
            Value::Null => Ok(None),
            v => T::from_value(v, tb).map(Some),
        }
    }
}

/// Typed, located reads of resolved nodes: what a kind's
/// `from_config(&Node, &Resolver)` uses (ADR 0009 §4). Every failure is
/// an [`Error`] naming the file, line, node, property and fix.
#[derive(Clone, Debug, Default)]
pub struct Resolver {
    pub(crate) timebase: Option<Timebase>,
}

impl Resolver {
    /// The frame rate and tempo, if the config gives them.
    #[must_use]
    pub fn timebase(&self) -> Option<&Timebase> {
        self.timebase.as_ref()
    }

    /// Property `key`, which must be present.
    ///
    /// # Errors
    ///
    /// If it is missing or isn't a `T`.
    pub fn get<T: FromValue>(&self, node: &Node, key: &str) -> Result<T, Error> {
        match node.prop(key) {
            Some(v) => self.read(node, &format!("property `{key}`"), v),
            None => Err(Self::missing(node, key)),
        }
    }

    /// Property `key`, if present.
    ///
    /// # Errors
    ///
    /// If it is present and isn't a `T`.
    pub fn opt<T: FromValue>(&self, node: &Node, key: &str) -> Result<Option<T>, Error> {
        node.prop(key)
            .map(|v| self.read(node, &format!("property `{key}`"), v))
            .transpose()
    }

    /// Argument `index` (0 is the name), which must be present.
    ///
    /// # Errors
    ///
    /// If it is missing or isn't a `T`.
    pub fn arg<T: FromValue>(&self, node: &Node, index: usize) -> Result<T, Error> {
        match node.args.get(index) {
            Some(v) => self.read(node, &format!("argument {}", index + 1), v),
            None => Err(Error::new(format!(
                "{} needs at least {} argument{}",
                node.describe(),
                index + 1,
                if index == 0 { "" } else { "s" }
            ))
            .at(node.origin.clone())
            .element(node.describe())),
        }
    }

    /// Every argument, each as a `T`.
    ///
    /// # Errors
    ///
    /// If one isn't a `T`.
    pub fn args<T: FromValue>(&self, node: &Node) -> Result<Vec<T>, Error> {
        node.args
            .iter()
            .enumerate()
            .map(|(i, v)| self.read(node, &format!("argument {}", i + 1), v))
            .collect()
    }

    /// The frames from property `from` up to property `to`, each converted
    /// from its own position (ADR 0001 §4).
    ///
    /// # Errors
    ///
    /// If either is missing or isn't a time, or `to` is before `from`.
    pub fn interval(&self, node: &Node, from: &str, to: &str) -> Result<Interval, Error> {
        let start: Frame = self.get(node, from)?;
        let end: Frame = self.get(node, to)?;
        if end < start {
            let at = node
                .prop(to)
                .map_or_else(|| node.origin.clone(), |v| v.origin.clone());
            return Err(
                Error::new(format!("`{to}` ({end}) is before `{from}` ({start})"))
                    .at(at)
                    .element(format!("{}, property `{to}`", node.describe()))
                    .help(format!(
                        "`{to}` is where it ends; check the two, or swap them"
                    )),
            );
        }
        Ok(Interval::new(start, end))
    }

    /// Fails on any property not in `known`, suggesting the near miss.
    ///
    /// # Errors
    ///
    /// On the first unknown property.
    pub fn check_props(&self, node: &Node, known: &[&str]) -> Result<(), Error> {
        for p in &node.props {
            if !known.contains(&p.key.as_str()) {
                return Err(
                    Error::new(format!("{} has no property `{}`", node.kind, p.key))
                        .at(p.value.origin.clone())
                        .element(format!("{}, property `{}`", node.describe(), p.key))
                        .help(
                            suggest(&p.key, "its properties are", known.iter().copied())
                                .replace("its properties are:", "its properties are"),
                        ),
                );
            }
        }
        Ok(())
    }

    /// An error at `origin` about `node`: for checks a kind makes itself.
    #[must_use]
    pub fn error(&self, node: &Node, origin: &Origin, message: impl Into<String>) -> Error {
        Error::new(message)
            .at(origin.clone())
            .element(node.describe())
    }

    fn read<T: FromValue>(&self, node: &Node, what: &str, v: &Located<Value>) -> Result<T, Error> {
        T::from_value(&v.value, self.timebase.as_ref()).map_err(|m| {
            Error::new(m.message)
                .at(v.origin.clone())
                .element(format!("{}, {what}", node.describe()))
                .maybe_help(m.help)
        })
    }

    fn missing(node: &Node, key: &str) -> Error {
        let keys: Vec<&str> = node.props.iter().map(|p| p.key.as_str()).collect();
        let near = crate::error::did_you_mean(key, keys.iter().copied());
        let help = match near {
            Some(k) => format!("is `{k}` a misspelling of `{key}`? Otherwise add {key}=…"),
            None => format!("add {key}=…"),
        };
        Error::new(format!("{} has no `{key}`", node.describe()))
            .at(node.origin.clone())
            .element(node.describe())
            .help(help)
    }
}
