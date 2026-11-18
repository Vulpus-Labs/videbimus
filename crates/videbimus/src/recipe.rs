//! Recipe kinds: how config declares assets ([ADR 0009] §4).
//!
//! An asset is declared in config with the recipe kind that builds it:
//!
//! ```kdl
//! asset "s07/blender" recipe="blender-shot" from=(f)0 to=(f)277 {
//!     input "panel" "s07/panel"
//!     samples 64
//! }
//! ```
//!
//! The recipe kind reads the node through [`RecipeKind::from_config`] and
//! returns a scheduler [`Task`]. Two things on the node are the
//! project's, not the kind's: the `recipe` property, and `input` children
//! (`input "<label>" "<asset name>"`), which become graph edges. The task
//! finds an input's output at run time by its label
//! ([`TaskContext::input_dirs`](videbimus_scheduler::TaskContext::input_dirs)),
//! and the scheduler adds each input's identity to the key under its
//! label.
//!
//! The project also assigns the asset's name (see [`crate::project`])
//! and owns the task's name and code identity: whatever the task returns
//! from [`Task::name`] is replaced by the assigned name, and the kind's
//! [`CodeIdentity`] is put into every key it makes
//! ([`KeyBuilder::code`]).
//!
//! [ADR 0009]: ../../../adrs/0009-using-videbimus.md

use std::collections::BTreeMap;
use std::fmt::Write as _;

use videbimus_config::{Error, Node, Resolver, Value};
use videbimus_runtime::CodeIdentity;
use videbimus_runtime::registry::did_you_mean;
use videbimus_scheduler::{Cost, Resources, Task, TaskContext, TaskError};
use videbimus_store::{KeyBuilder, Shape};

/// Properties of an `asset` node that the project reads, not the kind.
pub const ASSET_PROPS: [&str; 1] = ["recipe"];

/// Children of an `asset` node that the project reads: its inputs.
pub const INPUT: &str = "input";

/// A recipe kind: how config makes an asset's task ([ADR 0009] §4).
///
/// ```
/// use videbimus::config::{Error, Node, Resolver};
/// use videbimus::scheduler::{Cost, Resources, Task, TaskContext, TaskError};
/// use videbimus::store::{KeyBuilder, Shape};
/// use videbimus::{KeyStart, RecipeContext, RecipeKind};
///
/// /// Writes a greeting to `hello.txt`.
/// struct Hello { key: KeyStart, text: String }
///
/// impl Task for Hello {
///     fn name(&self) -> &str { "" } // the project assigns it
///     fn shape(&self) -> Shape { Shape::Single }
///     fn resources(&self) -> Resources { Resources::new(1, 0) }
///     fn cost(&self) -> Cost { Cost::Seconds(0.1) }
///     fn key(&self) -> KeyBuilder { self.key.builder() }
///     fn run(&self, cx: &TaskContext<'_>) -> Result<(), TaskError> {
///         std::fs::write(cx.out_dir().join("hello.txt"), &self.text)?;
///         Ok(())
///     }
/// }
///
/// impl RecipeKind for Hello {
///     const KIND: &'static str = "hello";
///     fn from_config(
///         node: &Node,
///         r: &Resolver,
///         cx: &RecipeContext,
///     ) -> Result<Box<dyn Task>, Error> {
///         cx.check_props(node, r, &["text"])?;
///         let text: String = r.get(node, "text")?;
///         Ok(Box::new(Hello { key: cx.key(node, r), text }))
///     }
/// }
/// ```
///
/// [ADR 0009]: ../../../adrs/0009-using-videbimus.md
pub trait RecipeKind {
    /// Its name in config: `asset … recipe="<KIND>"`.
    const KIND: &'static str;

    /// Its code identity: see [`ComponentKind::CODE`](videbimus_runtime::ComponentKind::CODE).
    const CODE: &'static str = "";

    /// Bumped by hand to force a rebuild of everything this kind built:
    /// see [`ComponentKind::VERSION`](videbimus_runtime::ComponentKind::VERSION).
    const VERSION: &'static str = "";

    /// The task that builds the asset declared by `node`, read through
    /// `r`. [`RecipeContext::key`] starts its key (kind, code identity
    /// and canonical config); the task adds what else it reads (sources,
    /// scripts, tools). Does no heavy work: the scheduler runs the task.
    ///
    /// # Errors
    ///
    /// A located config error for anything wrong with the node.
    fn from_config(node: &Node, r: &Resolver, cx: &RecipeContext) -> Result<Box<dyn Task>, Error>;
}

/// Makes an asset's task from its node: [`RecipeKind::from_config`].
pub type RecipeConstructor = fn(&Node, &Resolver, &RecipeContext) -> Result<Box<dyn Task>, Error>;

#[derive(Clone, Debug)]
struct Entry {
    make: RecipeConstructor,
    code: CodeIdentity,
}

/// Recipe kinds by name, with their code identities.
#[derive(Clone, Debug, Default)]
pub struct RecipeRegistry {
    kinds: BTreeMap<&'static str, Entry>,
}

impl RecipeRegistry {
    /// A registry with no kinds.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// A registry with the built-in recipe kinds. None yet: the `command`
    /// recipe (ticket 0011), file inputs and encode/mux (ticket 0007) are
    /// registered here when they land.
    #[must_use]
    pub fn with_builtins() -> Self {
        Self::empty()
    }

    /// Adds kind `K`.
    ///
    /// # Panics
    ///
    /// If a recipe kind of that name is already registered.
    pub fn register<K: RecipeKind>(&mut self) -> &mut Self {
        self.register_with_code(
            K::KIND,
            CodeIdentity::new(K::CODE, K::VERSION),
            K::from_config,
        )
    }

    /// Adds kind `K`, by value.
    ///
    /// # Panics
    ///
    /// As [`register`](Self::register).
    #[must_use]
    pub fn with<K: RecipeKind>(mut self) -> Self {
        self.register::<K>();
        self
    }

    /// Adds a kind by name, code identity and constructor, for kinds that
    /// aren't types.
    ///
    /// # Panics
    ///
    /// As [`register`](Self::register).
    pub fn register_with_code(
        &mut self,
        kind: &'static str,
        code: CodeIdentity,
        make: RecipeConstructor,
    ) -> &mut Self {
        assert!(
            self.kinds.insert(kind, Entry { make, code }).is_none(),
            "recipe kind `{kind}` is registered twice; kinds need distinct names"
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

    /// The code identity of `kind`, if it is registered.
    #[must_use]
    pub fn code(&self, kind: &str) -> Option<&CodeIdentity> {
        self.kinds.get(kind).map(|e| &e.code)
    }

    /// The task for the asset `node`, of recipe kind `kind`, named and
    /// keyed with the kind's code identity.
    ///
    /// # Errors
    ///
    /// If `kind` isn't registered (with a near miss, if any, at `node`'s
    /// `recipe` property), or the kind's own errors.
    pub(crate) fn build(
        &self,
        kind: &str,
        node: &Node,
        r: &Resolver,
        cx: RecipeContext,
    ) -> Result<Box<dyn Task>, Error> {
        let Some(entry) = self.kinds.get(kind) else {
            let known: Vec<&str> = self.kinds().collect();
            let at = videbimus_runtime::registry::origin_of(node, "recipe");
            let help = match did_you_mean(kind, &known) {
                Some(k) => format!("did you mean `{k}`?"),
                None if known.is_empty() => "this binary has no recipe kinds; if a project binary provides it, say so with `requires` and `binary` at the top of the config".to_owned(),
                None => format!(
                    "the recipe kinds are {}; if a project binary provides it, say so with `requires` and `binary` at the top of the config",
                    known.join(", ")
                ),
            };
            return Err(r
                .error(node, &at, format!("there is no recipe kind `{kind}`"))
                .help(help));
        };
        let cx = RecipeContext {
            code: entry.code.clone(),
            ..cx
        };
        let inner = (entry.make)(node, r, &cx)?;
        Ok(Box::new(KindTask {
            name: cx.name,
            code: cx.code.bytes(),
            inner,
        }))
    }
}

/// What a recipe kind's `from_config` is told about the asset it makes.
#[derive(Clone, Debug)]
pub struct RecipeContext {
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) group: Option<String>,
    pub(crate) inputs: Vec<(String, String)>,
    pub(crate) code: CodeIdentity,
}

impl RecipeContext {
    /// The recipe kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The asset's name, as the project assigned it (`s07/blender`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The group path it is declared under (`s07`), if any.
    #[must_use]
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// Its inputs: each label and the name of the asset it refers to.
    #[must_use]
    pub fn inputs(&self) -> &[(String, String)] {
        &self.inputs
    }

    /// The kind's code identity.
    #[must_use]
    pub fn code(&self) -> &CodeIdentity {
        &self.code
    }

    /// Fails on any property of `node` that isn't in `own` or the
    /// project's ([`ASSET_PROPS`]).
    ///
    /// # Errors
    ///
    /// On the first unknown property, suggesting the near miss.
    pub fn check_props(&self, node: &Node, r: &Resolver, own: &[&str]) -> Result<(), Error> {
        let known: Vec<&str> = own.iter().chain(&ASSET_PROPS).copied().collect();
        r.check_props(node, &known)
    }

    /// `node`'s children that are the kind's: all but `input`.
    pub fn children<'n>(&self, node: &'n Node) -> impl Iterator<Item = &'n Node> + 'n {
        node.children().iter().filter(|c| c.kind() != INPUT)
    }

    /// The start of the task's key: the kind, its code identity, and
    /// `node`'s canonical config ([`canonical_config`]). The task keeps it
    /// and starts [`Task::key`] from [`KeyStart::builder`], adding the
    /// sources, scripts and tools it reads; inputs are added by the
    /// scheduler.
    #[must_use]
    pub fn key(&self, node: &Node, r: &Resolver) -> KeyStart {
        KeyStart {
            kind: self.kind.clone(),
            code: self.code.bytes(),
            config: canonical_config(node, r),
        }
    }
}

/// The start of a recipe's key, from [`RecipeContext::key`]: kind, code
/// identity and canonical config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyStart {
    kind: String,
    code: Vec<u8>,
    config: Vec<u8>,
}

impl KeyStart {
    /// A key builder with these inputs.
    pub fn builder(&self) -> KeyBuilder {
        KeyBuilder::new(&self.kind)
            .code(&self.code)
            .config(&self.config)
    }

    /// The canonical config bytes.
    #[must_use]
    pub fn config(&self) -> &[u8] {
        &self.config
    }
}

/// `node` as canonical bytes for its key ([`KeyBuilder::config`]):
/// everything the kind reads, and nothing the project reads (its name,
/// `recipe` and `input`s, which are keyed as the kind and inputs).
/// Properties are sorted by key, so writing them in another order changes
/// nothing; a time is keyed by its seconds where the config has a tempo
/// or it isn't musical, so `(s)1` and `(f)30` at 30 fps key alike.
#[must_use]
pub fn canonical_config(node: &Node, r: &Resolver) -> Vec<u8> {
    let mut out = String::new();
    let args = match node.args().first() {
        Some(a) if matches!(a.value, Value::String(_)) => &node.args()[1..],
        _ => node.args(),
    };
    write_node(&mut out, node.kind(), args, node, r, true);
    out.into_bytes()
}

fn write_node(
    out: &mut String,
    kind: &str,
    args: &[videbimus_config::Located<Value>],
    node: &Node,
    r: &Resolver,
    top: bool,
) {
    let _ = write!(out, "node {}:{kind} args {}", kind.len(), args.len());
    for a in args {
        out.push(' ');
        write_value(out, &a.value, r);
    }
    let mut props: Vec<_> = node
        .props()
        .iter()
        .filter(|p| !(top && ASSET_PROPS.contains(&p.key.as_str())))
        .collect();
    props.sort_by(|a, b| a.key.cmp(&b.key));
    let _ = write!(out, " props {}", props.len());
    for p in props {
        let _ = write!(out, " {}:{}=", p.key.len(), p.key);
        write_value(out, &p.value.value, r);
    }
    let children: Vec<&Node> = node
        .children()
        .iter()
        .filter(|c| !(top && c.kind() == INPUT))
        .collect();
    let _ = write!(out, " children {} {{", children.len());
    for c in children {
        write_node(out, c.kind(), c.args(), c, r, false);
    }
    out.push('}');
}

fn write_value(out: &mut String, v: &Value, r: &Resolver) {
    let _ = match v {
        Value::Null => write!(out, "null"),
        Value::Bool(b) => write!(out, "bool:{b}"),
        Value::Number(n) => write!(out, "num:{:016x}", n.to_bits()),
        Value::String(s) => write!(out, "str:{}:{s}", s.len()),
        Value::Time(t) => match r.timebase().and_then(|tb| tb.seconds(*t).ok()) {
            Some(s) => write!(out, "sec:{:016x}", s.to_bits()),
            None => write!(out, "time:{:016x}{}", t.amount.to_bits(), t.unit.suffix()),
        },
    };
}

/// A kind's task as the project hands it to the scheduler: named by the
/// project, keyed with the kind's code identity.
struct KindTask {
    name: String,
    code: Vec<u8>,
    inner: Box<dyn Task>,
}

impl Task for KindTask {
    fn name(&self) -> &str {
        &self.name
    }
    fn shape(&self) -> Shape {
        self.inner.shape()
    }
    fn resources(&self) -> Resources {
        self.inner.resources()
    }
    fn cost(&self) -> Cost {
        self.inner.cost()
    }
    fn key(&self) -> KeyBuilder {
        self.inner.key().code(&self.code)
    }
    fn hash_output(&self) -> bool {
        self.inner.hash_output()
    }
    fn chunk_frames(&self) -> Option<u32> {
        self.inner.chunk_frames()
    }
    fn run(&self, ctx: &TaskContext<'_>) -> Result<(), TaskError> {
        self.inner.run(ctx)
    }
}
