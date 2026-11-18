//! The second stage: resolve references and arithmetic, in order, lazily,
//! with cycle detection.

use std::cell::RefCell;

use crate::error::{Error, Origin, suggest};
use crate::expand::{RawEntry, RawNode, RawValue, Scope, ScopeId, Slot};
use crate::expr::{self, BinOp, Expr, ExprKind, Func, Span};
use crate::node::{Located, Node, Prop};
use crate::value::{NoTempo, Time, Timebase, Unit, Value};

pub(crate) struct Resolving<'a> {
    pub scopes: &'a [Scope],
    pub timebase: Option<Timebase>,
    /// What is being resolved, outermost first, to name cycles.
    stack: RefCell<Vec<String>>,
}

/// A node whose properties are being resolved.
struct Live<'a> {
    raw: &'a RawNode,
    slots: Vec<RefCell<Slot>>,
    parent: Option<&'a Live<'a>>,
    /// Its earlier siblings, resolved.
    siblings: &'a [Node],
}

/// A node reached by a reference path.
#[derive(Clone)]
enum NodeRef<'a> {
    Live(&'a Live<'a>),
    Done {
        node: &'a Node,
        siblings: &'a [Node],
        index: usize,
        parent: Option<Box<NodeRef<'a>>>,
    },
}

impl NodeRef<'_> {
    fn describe(&self) -> String {
        match self {
            NodeRef::Live(l) => l.raw.describe(),
            NodeRef::Done { node, .. } => node.describe(),
        }
    }
}

/// Where an expression is being evaluated.
struct Cx<'a> {
    scope: ScopeId,
    live: Option<&'a Live<'a>>,
    entry: &'a RawEntry,
}

impl Cx<'_> {
    /// The location of `span` within the expression, or the whole entry
    /// when the source can't be mapped exactly.
    fn origin(&self, span: Option<Span>) -> Origin {
        let mut o = self.entry.origin.clone();
        if let (
            Some(span),
            RawValue::Expr {
                content: Some(start),
                ..
            },
        ) = (span, &self.entry.value)
        {
            o.at = o.at.with_span(start + span.start, span.end - span.start);
        }
        o
    }
}

impl<'a> Resolving<'a> {
    pub fn new(scopes: &'a [Scope], timebase: Option<Timebase>) -> Self {
        Self {
            scopes,
            timebase,
            stack: RefCell::new(Vec::new()),
        }
    }

    pub fn block(&self, raw: &[RawNode]) -> Result<Vec<Node>, Error> {
        self.block_in(raw, None)
    }

    fn block_in(&self, raw: &[RawNode], parent: Option<&Live<'_>>) -> Result<Vec<Node>, Error> {
        let mut done = Vec::with_capacity(raw.len());
        for r in raw {
            let n = self.node(r, parent, &done)?;
            done.push(n);
        }
        Ok(done)
    }

    /// A top-level node on its own.
    pub fn lone(&self, raw: &RawNode) -> Result<Node, Error> {
        self.node(raw, None, &[])
    }

    fn node(
        &self,
        raw: &RawNode,
        parent: Option<&Live<'_>>,
        siblings: &[Node],
    ) -> Result<Node, Error> {
        let live = Live {
            raw,
            slots: raw
                .props
                .iter()
                .map(|_| RefCell::new(Slot::Pending))
                .collect(),
            parent,
            siblings,
        };
        let mut props = Vec::with_capacity(raw.props.len());
        for (i, p) in raw.props.iter().enumerate() {
            let value = self.prop(&live, i, None)?;
            props.push(Prop {
                key: p.key.clone(),
                value: Located {
                    value,
                    origin: p.entry.origin.clone(),
                },
            });
        }
        let mut args = Vec::with_capacity(raw.args.len());
        for (i, a) in raw.args.iter().enumerate() {
            let cx = Cx {
                scope: raw.scope,
                live: Some(&live),
                entry: a,
            };
            let value = self
                .entry(&cx)
                .map_err(|e| e.element_or(|| format!("{}, argument {}", raw.describe(), i + 1)))?;
            args.push(Located {
                value,
                origin: a.origin.clone(),
            });
        }
        let children = self.block_in(&raw.children, Some(&live))?;
        Ok(Node {
            kind: raw.kind.clone(),
            origin: raw.origin.clone(),
            args,
            props,
            children,
        })
    }

    /// Property `i` of `live`, resolving it now if it hasn't been.
    /// `needed_by` is the reference that asked for it, if any.
    #[allow(clippy::many_single_char_names)]
    fn prop(&self, live: &Live<'_>, i: usize, needed_by: Option<&Origin>) -> Result<Value, Error> {
        let p = &live.raw.props[i];
        let name = format!("{}.{}", live.raw.describe(), p.key);
        let state = live.slots[i].borrow().clone();
        match state {
            Slot::Done(v) => Ok(v),
            Slot::Busy => Err(self.cycle(&name, needed_by.unwrap_or(&p.entry.origin))),
            Slot::Pending => {
                *live.slots[i].borrow_mut() = Slot::Busy;
                self.stack.borrow_mut().push(name);
                let cx = Cx {
                    scope: live.raw.scope,
                    live: Some(live),
                    entry: &p.entry,
                };
                let r = self.entry(&cx);
                self.stack.borrow_mut().pop();
                let v = r.map_err(|e| {
                    let e =
                        e.element_or(|| format!("{}, property `{}`", live.raw.describe(), p.key));
                    match needed_by {
                        Some(o) => e.note(format!("needed by the reference at {}", o.at)),
                        None => e,
                    }
                })?;
                *live.slots[i].borrow_mut() = Slot::Done(v.clone());
                Ok(v)
            }
        }
    }

    fn cycle(&self, name: &str, at: &Origin) -> Error {
        let stack = self.stack.borrow();
        let start = stack.iter().position(|s| s == name).unwrap_or(0);
        let mut chain: Vec<&str> = stack[start..].iter().map(String::as_str).collect();
        chain.push(name);
        Error::new(format!(
            "these values refer to each other in a cycle: {}",
            chain.join(" → ")
        ))
        .at(at.clone())
        .help("give one of them a value that doesn't depend on the others")
    }

    fn entry(&self, cx: &Cx<'_>) -> Result<Value, Error> {
        match &cx.entry.value {
            RawValue::Lit(v) => Ok(v.clone()),
            RawValue::Ref(path) => self.reference(path, cx, None),
            RawValue::Expr { src, .. } => {
                let e = expr::parse(src).map_err(|s| {
                    Error::new(s.message)
                        .at(cx.origin(Some(s.span)))
                        .maybe_help(s.help)
                })?;
                self.eval(&e, cx)
            }
        }
    }

    /// Resolves `$path` (given without its `$`).
    fn reference(&self, path: &str, cx: &Cx<'_>, span: Option<Span>) -> Result<Value, Error> {
        let at = cx.origin(span);
        let fail = |message: String, help: String| Error::new(message).at(at.clone()).help(help);
        let mut segs = path.split('.');
        let head = segs.next().unwrap_or_default();
        let rest: Vec<&str> = segs.collect();
        let start = match head {
            "self" | "parent" | "prev" => {
                let Some(live) = cx.live else {
                    return Err(fail(
                        format!(
                            "`${head}` only works in a node's values; a `let` belongs to no node"
                        ),
                        "compute it where it's used, in the node's properties".into(),
                    ));
                };
                Self::step(NodeRef::Live(live), head, &at)?
            }
            name => {
                if let Some(seg) = rest.first() {
                    return Err(fail(
                        format!("`${name}` is a value, not a node, so `.{seg}` can't follow it"),
                        "node paths start with $self, $parent or $prev".into(),
                    ));
                }
                return self.binding(name, cx.scope, &at);
            }
        };
        let Some((last, walk)) = rest.split_last() else {
            return Err(fail(
                format!("`${path}` is a node; name one of its properties"),
                format!("for example ${path}.start"),
            ));
        };
        let mut cur = start;
        for seg in walk {
            cur = Self::step(cur, seg, &at)?;
        }
        self.property(&cur, last, &at)
    }

    /// One step along a path: `parent`, `prev`, or into a child.
    fn step<'n>(cur: NodeRef<'n>, seg: &str, at: &Origin) -> Result<NodeRef<'n>, Error> {
        let fail = |message: String, help: &str| Error::new(message).at(at.clone()).help(help);
        match (seg, &cur) {
            ("self", NodeRef::Live(_)) => Ok(cur),
            ("parent", NodeRef::Live(l)) => l.parent.map(NodeRef::Live).ok_or_else(|| {
                fail(
                    format!(
                        "{} is at the top level, so it has no parent",
                        l.raw.describe()
                    ),
                    "use a `let` for values shared at the top level",
                )
            }),
            ("parent", NodeRef::Done { parent, node, .. }) => {
                parent.as_deref().cloned().ok_or_else(|| {
                    fail(
                        format!(
                            "{} is at the top level, so it has no parent",
                            node.describe()
                        ),
                        "use a `let` for values shared at the top level",
                    )
                })
            }
            ("prev", _) => {
                let (siblings, index, parent) = match &cur {
                    NodeRef::Live(l) => (
                        l.siblings,
                        l.siblings.len(),
                        l.parent.map(|p| Box::new(NodeRef::Live(p))),
                    ),
                    NodeRef::Done {
                        siblings,
                        index,
                        parent,
                        ..
                    } => (*siblings, *index, parent.clone()),
                };
                if index == 0 {
                    return Err(fail(
                        format!(
                            "{} is the first in its block, so `prev` has nothing to refer to",
                            cur.describe()
                        ),
                        "`$prev` is the node just before this one in the same block",
                    ));
                }
                Ok(NodeRef::Done {
                    node: &siblings[index - 1],
                    siblings,
                    index: index - 1,
                    parent,
                })
            }
            (_, NodeRef::Live(l)) => Err(fail(
                format!(
                    "{} isn't resolved yet, so `.{seg}` can't look into its children",
                    l.raw.describe()
                ),
                "a node's values can read its own and its ancestors' properties, and anything in earlier siblings ($prev)",
            )),
            (_, NodeRef::Done { node, .. }) => {
                let by_kind: Vec<usize> = (0..node.children.len())
                    .filter(|&i| node.children[i].kind == seg)
                    .collect();
                let found: Vec<usize> = if by_kind.is_empty() {
                    (0..node.children.len())
                        .filter(|&i| node.children[i].name() == Some(seg))
                        .collect()
                } else {
                    by_kind
                };
                match found.as_slice() {
                    [i] => Ok(NodeRef::Done {
                        node: &node.children[*i],
                        siblings: &node.children,
                        index: *i,
                        parent: Some(Box::new(cur.clone())),
                    }),
                    [] => {
                        let kinds: Vec<&str> =
                            node.children.iter().map(|c| c.kind.as_str()).collect();
                        Err(fail(
                            format!("{} has no child `{seg}`", node.describe()),
                            &suggest(seg, "its children", kinds),
                        ))
                    }
                    many => Err(fail(
                        format!(
                            "{} has {} children `{seg}`, so `.{seg}` is ambiguous",
                            node.describe(),
                            many.len()
                        ),
                        "give the one you mean a name, `camera \"main\"`, and refer to it by name",
                    )),
                }
            }
        }
    }

    fn property(&self, node: &NodeRef<'_>, key: &str, at: &Origin) -> Result<Value, Error> {
        let found = match node {
            NodeRef::Live(l) => match l.raw.props.iter().position(|p| p.key == key) {
                Some(i) => return self.prop(l, i, Some(at)),
                None => None,
            },
            NodeRef::Done { node, .. } => node.prop(key).map(|v| v.value.clone()),
        };
        if let Some(v) = found {
            return Ok(v);
        }
        let (keys, name): (Vec<&str>, Option<&str>) = match node {
            NodeRef::Live(l) => (
                l.raw.props.iter().map(|p| p.key.as_str()).collect(),
                l.raw.name(),
            ),
            NodeRef::Done { node, .. } => (
                node.props.iter().map(|p| p.key.as_str()).collect(),
                node.name(),
            ),
        };
        if let ("name", Some(n)) = (key, name) {
            return Ok(Value::String(n.to_owned()));
        }
        Err(
            Error::new(format!("{} has no property `{key}`", node.describe()))
                .at(at.clone())
                .help(suggest(key, "its properties", keys)),
        )
    }

    /// A `let` or `each` binding, by name, from the innermost scope out.
    fn binding(&self, name: &str, scope: ScopeId, at: &Origin) -> Result<Value, Error> {
        let mut s = Some(scope);
        while let Some(id) = s {
            let sc = &self.scopes[id];
            if let Some(b) = sc.bindings.iter().find(|b| b.name == name) {
                let state = b.state.borrow().clone();
                let label = format!("${name}");
                return match state {
                    Slot::Done(v) => Ok(v),
                    Slot::Busy => Err(self.cycle(&label, at)),
                    Slot::Pending => {
                        *b.state.borrow_mut() = Slot::Busy;
                        self.stack.borrow_mut().push(label);
                        let cx = Cx {
                            scope: id,
                            live: None,
                            entry: &b.entry,
                        };
                        let r = self.entry(&cx);
                        self.stack.borrow_mut().pop();
                        let v = r.map_err(|e| {
                            e.element_or(|| format!("let {name}"))
                                .note(format!("needed by the reference at {}", at.at))
                        })?;
                        *b.state.borrow_mut() = Slot::Done(v.clone());
                        Ok(v)
                    }
                };
            }
            s = sc.parent;
        }
        let mut visible = Vec::new();
        let mut s = Some(scope);
        while let Some(id) = s {
            visible.extend(self.scopes[id].bindings.iter().map(|b| b.name.as_str()));
            s = self.scopes[id].parent;
        }
        Err(
            Error::new(format!("nothing called `${name}` is defined here"))
                .at(at.clone())
                .help(format!(
                    "{}. Define it with `let {name}=…` in this block or an enclosing one",
                    suggest(name, "names in scope", visible)
                )),
        )
    }

    fn timebase(&self, why: &str, cx: &Cx<'_>, span: Span) -> Result<&Timebase, Error> {
        self.timebase.as_ref().ok_or_else(|| {
            Error::new(format!(
                "{why} needs the video's frame rate, and the config has none"
            ))
            .at(cx.origin(Some(span)))
            .help("add `video rate=30 { tempo bpm=120 beats-per-bar=4 }` at the top of the config")
        })
    }

    fn no_tempo(cx: &Cx<'_>, span: Span) -> impl FnOnce(NoTempo) -> Error {
        let at = cx.origin(Some(span));
        move |e| {
            Error::new(e.to_string())
                .at(at)
                .help("add `tempo bpm=… beats-per-bar=…` inside `video`")
        }
    }

    /// `t` in `unit`.
    fn convert(&self, t: Time, unit: Unit, cx: &Cx<'_>, span: Span) -> Result<Time, Error> {
        if t.unit == unit {
            return Ok(t);
        }
        let tb = self.timebase(
            &format!("converting {} to {}", t.unit.suffix(), unit.suffix()),
            cx,
            span,
        )?;
        tb.convert(t, unit).map_err(Self::no_tempo(cx, span))
    }

    #[allow(clippy::many_single_char_names)]
    fn eval(&self, e: &Expr, cx: &Cx<'_>) -> Result<Value, Error> {
        let fail = |message: String, help: Option<String>| {
            Error::new(message)
                .at(cx.origin(Some(e.span)))
                .maybe_help(help)
        };
        match &e.kind {
            ExprKind::Number(n) => Ok(Value::Number(*n)),
            ExprKind::Time(t) => Ok(Value::Time(*t)),
            ExprKind::Ref(path) => self.reference(path, cx, Some(e.span)),
            ExprKind::Neg(x) => match self.eval(x, cx)? {
                Value::Number(n) => Ok(Value::Number(-n)),
                Value::Time(t) => Ok(Value::Time(Time::new(-t.amount, t.unit))),
                v => Err(fail(format!("can't negate {}", v.describe()), None)),
            },
            ExprKind::Binary(op, a, b) => {
                let x = self.eval(a, cx)?;
                let y = self.eval(b, cx)?;
                self.binary(*op, &x, &y, cx, e.span)
            }
            ExprKind::Call(f, args) => {
                let vals = args
                    .iter()
                    .map(|a| self.eval(a, cx))
                    .collect::<Result<Vec<_>, _>>()?;
                self.call(*f, &vals, cx, e.span)
            }
        }
    }

    fn binary(
        &self,
        op: BinOp,
        x: &Value,
        y: &Value,
        cx: &Cx<'_>,
        span: Span,
    ) -> Result<Value, Error> {
        let fail = |message: String, help: Option<String>| {
            Error::new(message)
                .at(cx.origin(Some(span)))
                .maybe_help(help)
        };
        let sym = op.symbol();
        match (op, x, y) {
            (_, Value::Number(a), Value::Number(b)) => match op {
                BinOp::Add => Ok(Value::Number(a + b)),
                BinOp::Sub => Ok(Value::Number(a - b)),
                BinOp::Mul => Ok(Value::Number(a * b)),
                BinOp::Div if *b == 0.0 => Err(fail("division by zero".into(), None)),
                BinOp::Div => Ok(Value::Number(a / b)),
            },
            (BinOp::Add | BinOp::Sub, Value::Time(a), Value::Time(b)) => {
                let b = self.convert(*b, a.unit, cx, span)?;
                let amount = if op == BinOp::Add {
                    a.amount + b.amount
                } else {
                    a.amount - b.amount
                };
                Ok(Value::Time(Time::new(amount, a.unit)))
            }
            (BinOp::Add | BinOp::Sub, Value::Time(t), Value::Number(n))
            | (BinOp::Add | BinOp::Sub, Value::Number(n), Value::Time(t)) => Err(fail(
                format!(
                    "can't {} a time and a plain number ({t} {sym} {n})",
                    if op == BinOp::Add { "add" } else { "subtract" }
                ),
                Some(format!(
                    "give the number a unit: {n}{}, or {n}s, {n}bar, {n}beat or {n}f",
                    t.unit.suffix()
                )),
            )),
            (BinOp::Mul, Value::Time(t), Value::Number(n))
            | (BinOp::Mul, Value::Number(n), Value::Time(t)) => {
                Ok(Value::Time(Time::new(t.amount * n, t.unit)))
            }
            (BinOp::Mul, Value::Time(_), Value::Time(_)) => Err(fail(
                format!("can't multiply two times ({x} * {y})"),
                Some("scale a time by a plain number; divide two times to get a ratio".into()),
            )),
            (BinOp::Div, Value::Time(t), Value::Number(n)) => {
                if *n == 0.0 {
                    return Err(fail("division by zero".into(), None));
                }
                Ok(Value::Time(Time::new(t.amount / n, t.unit)))
            }
            (BinOp::Div, Value::Time(a), Value::Time(b)) => {
                let b = self.convert(*b, a.unit, cx, span)?;
                if b.amount == 0.0 {
                    return Err(fail("division by a zero time".into(), None));
                }
                Ok(Value::Number(a.amount / b.amount))
            }
            (BinOp::Div, Value::Number(_), Value::Time(_)) => Err(fail(
                format!("can't divide a plain number by a time ({x} / {y})"),
                Some("divide a time by a time for a ratio, or a time by a number".into()),
            )),
            _ => {
                let bad = if matches!(x, Value::Number(_) | Value::Time(_)) {
                    y
                } else {
                    x
                };
                let help = match bad {
                    Value::String(s) if s.trim().parse::<f64>().is_ok() => Some(format!(
                        "{bad} is a string; write it without quotes, with a unit if it is a time: (bar){}",
                        s.trim()
                    )),
                    _ => None,
                };
                Err(fail(
                    format!("`{sym}` works on numbers and times, not {}", bad.describe()),
                    help,
                ))
            }
        }
    }

    fn call(&self, f: Func, args: &[Value], cx: &Cx<'_>, span: Span) -> Result<Value, Error> {
        let name = f.name();
        let fail =
            |message: String, help: &str| Error::new(message).at(cx.origin(Some(span))).help(help);
        match f {
            Func::Frames => {
                let [Value::Time(t)] = args else {
                    return Err(fail(
                        format!("frames() takes one time, and got {}", describe_args(args)),
                        "frames($self.to - $self.from) is the whole number of frames nearest that time",
                    ));
                };
                let tb = self.timebase("frames()", cx, span)?;
                let n = tb.frame(*t).map_err(Self::no_tempo(cx, span))?;
                #[allow(clippy::cast_precision_loss)]
                Ok(Value::Time(Time::new(n.0 as f64, Unit::Frame)))
            }
            Func::Min | Func::Max | Func::Clamp => {
                if f == Func::Clamp && args.len() != 3 {
                    return Err(fail(
                        format!("clamp() takes three values, and got {}", args.len()),
                        "clamp(x, low, high)",
                    ));
                }
                if args.is_empty() {
                    return Err(fail(
                        format!("{name}() needs at least one value"),
                        "min(a, b)",
                    ));
                }
                // Everything in the first value's unit, if it has one.
                let unit = match &args[0] {
                    Value::Time(t) => Some(t.unit),
                    Value::Number(_) => None,
                    v => {
                        return Err(fail(
                            format!("{name}() works on numbers and times, not {}", v.describe()),
                            "",
                        ));
                    }
                };
                let mut xs = Vec::with_capacity(args.len());
                for v in args {
                    match (unit, v) {
                        (None, Value::Number(n)) => xs.push(*n),
                        (Some(u), Value::Time(t)) => xs.push(self.convert(*t, u, cx, span)?.amount),
                        _ => {
                            return Err(fail(
                                format!(
                                    "{name}() mixes times and plain numbers: {}",
                                    describe_args(args)
                                ),
                                "give every value a unit, or none",
                            ));
                        }
                    }
                }
                let r = match f {
                    Func::Min => xs.iter().copied().fold(f64::INFINITY, f64::min),
                    Func::Max => xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                    _ => {
                        let (x, lo, hi) = (xs[0], xs[1], xs[2]);
                        if lo > hi {
                            return Err(fail(
                                format!(
                                    "clamp()'s low bound ({}) is above its high bound ({})",
                                    args[1], args[2]
                                ),
                                "clamp(x, low, high): swap the bounds",
                            ));
                        }
                        x.clamp(lo, hi)
                    }
                };
                Ok(match unit {
                    Some(u) => Value::Time(Time::new(r, u)),
                    None => Value::Number(r),
                })
            }
        }
    }
}

fn describe_args(args: &[Value]) -> String {
    if args.is_empty() {
        return "nothing".into();
    }
    let v: Vec<String> = args.iter().map(ToString::to_string).collect();
    v.join(", ")
}
