//! The first stage: read the root file and its includes, collect
//! templates, and expand `include`, `template`/`use`, `let` and `each` into
//! a tree of raw nodes whose values are not yet resolved.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use crate::error::{Error, Origin, Via, suggest};
use crate::expr::valid_path;
use crate::source::{Loader, Location, SourceFile, normalize};
use crate::value::{Time, Unit, Value};

pub(crate) type ScopeId = usize;

/// A value's resolution state, for lazy resolution with cycle detection.
#[derive(Clone, Debug)]
pub(crate) enum Slot {
    Pending,
    Busy,
    Done(Value),
}

/// The names bound in one block: its `let`s, or one `each` iteration's
/// item and index.
pub(crate) struct Scope {
    pub parent: Option<ScopeId>,
    pub bindings: Vec<Binding>,
}

pub(crate) struct Binding {
    pub name: String,
    pub entry: RawEntry,
    pub state: RefCell<Slot>,
}

#[derive(Clone, Debug)]
pub(crate) struct RawEntry {
    pub value: RawValue,
    pub origin: Origin,
}

#[derive(Clone, Debug)]
pub(crate) enum RawValue {
    Lit(Value),
    /// A reference, without its `$`.
    Ref(String),
    /// An expression; `content` is the file offset of its first character,
    /// when the source spells the string without escapes.
    Expr {
        src: String,
        content: Option<usize>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct RawProp {
    pub key: String,
    pub entry: RawEntry,
}

#[derive(Clone, Debug)]
pub(crate) struct RawNode {
    pub kind: String,
    pub origin: Origin,
    pub args: Vec<RawEntry>,
    pub props: Vec<RawProp>,
    pub children: Vec<RawNode>,
    /// The scope its values resolve in.
    pub scope: ScopeId,
}

impl RawNode {
    /// The first argument, if it is a literal string.
    pub fn name(&self) -> Option<&str> {
        match &self.args.first()?.value {
            RawValue::Lit(Value::String(s)) => Some(s),
            _ => None,
        }
    }

    /// `shot "s02"`, or `key`.
    pub fn describe(&self) -> String {
        describe(&self.kind, self.name())
    }
}

pub(crate) fn describe(kind: &str, name: Option<&str>) -> String {
    match name {
        Some(n) => format!("{kind} {n:?}"),
        None => kind.to_owned(),
    }
}

pub(crate) struct Expanded {
    pub nodes: Vec<RawNode>,
    pub scopes: Vec<Scope>,
}

struct Template {
    node: KdlNode,
    file: Arc<SourceFile>,
    at: Location,
}

/// A top-level node, after top-level includes are spliced in.
struct Item {
    node: KdlNode,
    file: Arc<SourceFile>,
    via: Vec<Via>,
}

struct Expander<'l> {
    loader: &'l dyn Loader,
    scopes: Vec<Scope>,
    templates: HashMap<String, Rc<Template>>,
    /// Files being read, outermost first, for include cycles.
    including: Vec<PathBuf>,
    /// Templates being applied, outermost first, for template cycles.
    applying: Vec<String>,
}

pub(crate) fn expand(root: &Path, loader: &dyn Loader) -> Result<Expanded, Error> {
    let mut x = Expander {
        loader,
        scopes: Vec::new(),
        templates: HashMap::new(),
        including: Vec::new(),
        applying: Vec::new(),
    };
    let root = normalize(root);
    let text = loader.load(&root).map_err(|e| {
        Error::new(format!("can't read the config `{}`: {e}", root.display()))
            .help("check the path; it is relative to the working directory")
    })?;
    let file = Arc::new(SourceFile::new(root.clone(), text));
    let doc = parse(&file)?;
    x.including.push(root);
    let mut items = Vec::new();
    x.flatten(&doc, &file, &[], &mut items)?;
    x.including.pop();

    for item in &items {
        if item.node.name().value() == "template" {
            x.define_template(item)?;
        }
    }
    let scope = x.new_scope(None);
    let mut nodes = Vec::new();
    for item in &items {
        x.expand_one(&item.node, &item.file, &item.via, scope, true, &mut nodes)?;
    }
    Ok(Expanded {
        nodes,
        scopes: x.scopes,
    })
}

fn parse(file: &Arc<SourceFile>) -> Result<KdlDocument, Error> {
    KdlDocument::parse_v2(file.text()).map_err(|e| {
        let Some(d) = e.diagnostics.first() else {
            return Error::new(format!("{} isn't valid KDL", file.path().display()));
        };
        let message = d.message.clone().unwrap_or_else(|| "invalid KDL".into());
        let label = d.label.clone();
        let at = Location::new(file, d.span.offset(), d.span.len());
        let help = if message.contains("type annotation") {
            Some(format!(
                "a type annotation is a name: (bar), (beat), (s), (f) or (expr); expressions are written (expr)\"…\"{}",
                if at.text().starts_with('=') { ", not (=)\"…\", which isn't valid KDL v2" } else { "" }
            ))
        } else {
            d.help.clone().or_else(|| {
                label.map(|l| format!("{l}; videbimus configs are KDL v2 (https://kdl.dev)"))
            })
        };
        Error::new(message).at(Origin::new(at)).maybe_help(help)
    })
}

fn loc(file: &Arc<SourceFile>, span: miette::SourceSpan) -> Location {
    Location::new(file, span.offset(), span.len())
}

fn origin(file: &Arc<SourceFile>, span: miette::SourceSpan, via: &[Via]) -> Origin {
    Origin {
        at: loc(file, span),
        via: via.to_vec(),
    }
}

fn with_via(head: Via, rest: &[Via]) -> Vec<Via> {
    let mut v = vec![head];
    v.extend_from_slice(rest);
    v
}

/// A node's own location: its name, which is short and always on one line.
fn node_origin(node: &KdlNode, file: &Arc<SourceFile>, via: &[Via]) -> Origin {
    origin(file, node.name().span(), via)
}

impl Expander<'_> {
    fn new_scope(&mut self, parent: Option<ScopeId>) -> ScopeId {
        self.scopes.push(Scope {
            parent,
            bindings: Vec::new(),
        });
        self.scopes.len() - 1
    }

    /// Reads the file an `include` names, relative to the including file.
    fn read_include(
        &mut self,
        node: &KdlNode,
        file: &Arc<SourceFile>,
        via: &[Via],
    ) -> Result<(Arc<SourceFile>, KdlDocument), Error> {
        let at = node_origin(node, file, via);
        let usage = "write `include \"other.kdl\"`, with the path relative to this file";
        let mut args = node.entries().iter().filter(|e| e.name().is_none());
        let path = match (args.next(), args.next()) {
            (Some(e), None) if e.ty().is_none() => e.value().as_string().map(str::to_owned),
            _ => None,
        };
        let Some(path) = path else {
            return Err(Error::new("`include` takes one path, as a string")
                .at(at)
                .element("include")
                .help(usage));
        };
        if node.entries().iter().any(|e| e.name().is_some()) || node.children().is_some() {
            return Err(Error::new("`include` takes only a path")
                .at(at)
                .element("include")
                .help(usage));
        }
        let dir = file.path().parent().unwrap_or(Path::new(""));
        let target = normalize(&dir.join(&path));
        if let Some(i) = self.including.iter().position(|p| *p == target) {
            let chain: Vec<String> = self.including[i..]
                .iter()
                .chain(std::iter::once(&target))
                .map(|p| p.display().to_string())
                .collect();
            return Err(Error::new(format!("include cycle: {}", chain.join(" → ")))
                .at(at)
                .element(format!("include {path:?}"))
                .help(
                    "remove one of these includes; a file can't include itself, even indirectly",
                ));
        }
        let text = self.loader.load(&target).map_err(|e| {
            Error::new(format!("can't read `{}`: {e}", target.display()))
                .at(at.clone())
                .element(format!("include {path:?}"))
                .help(format!(
                    "the path is relative to the including file's directory (`{}`)",
                    dir.display()
                ))
        })?;
        let included = Arc::new(SourceFile::new(target, text));
        let doc = parse(&included)?;
        Ok((included, doc))
    }

    /// Splices top-level includes, recursively, so that templates anywhere
    /// at the top level of any included file are known before expansion.
    fn flatten(
        &mut self,
        doc: &KdlDocument,
        file: &Arc<SourceFile>,
        via: &[Via],
        out: &mut Vec<Item>,
    ) -> Result<(), Error> {
        for node in doc.nodes() {
            if node.name().value() == "include" {
                let (included, inner) = self.read_include(node, file, via)?;
                let via = with_via(
                    Via::Include {
                        at: loc(file, node.name().span()),
                    },
                    via,
                );
                self.including.push(included.path().to_path_buf());
                self.flatten(&inner, &included, &via, out)?;
                self.including.pop();
            } else {
                out.push(Item {
                    node: node.clone(),
                    file: Arc::clone(file),
                    via: via.to_vec(),
                });
            }
        }
        Ok(())
    }

    fn define_template(&mut self, item: &Item) -> Result<(), Error> {
        let node = &item.node;
        let at = node_origin(node, &item.file, &item.via);
        let name = match node.entries().first() {
            Some(e) if e.name().is_none() && e.ty().is_none() => e.value().as_string(),
            _ => None,
        };
        let Some(name) = name else {
            return Err(Error::new("a template needs a name")
                .at(at)
                .element("template")
                .help("write `template \"close-up\" prop=value … { children }`, then `shot use=\"close-up\"`"));
        };
        if let Some(extra) = node.entries().iter().skip(1).find(|e| e.name().is_none()) {
            return Err(Error::new(format!(
                "template \"{name}\" has an argument after its name"
            ))
            .at(origin(&item.file, extra.span(), &item.via))
            .element(format!("template {name:?}"))
            .help("templates carry properties and children; give the value a name: key=value"));
        }
        if let Some(prev) = self.templates.get(name) {
            return Err(Error::new(format!("template \"{name}\" is defined twice"))
                .at(at)
                .element(format!("template {name:?}"))
                .note(format!("first defined at {}", prev.at))
                .help("rename one of them; template names are global across included files"));
        }
        self.templates.insert(
            name.to_owned(),
            Rc::new(Template {
                node: node.clone(),
                file: Arc::clone(&item.file),
                at: at.at,
            }),
        );
        Ok(())
    }

    fn expand_block(
        &mut self,
        nodes: &[KdlNode],
        file: &Arc<SourceFile>,
        via: &[Via],
        scope: ScopeId,
        out: &mut Vec<RawNode>,
    ) -> Result<(), Error> {
        for node in nodes {
            self.expand_one(node, file, via, scope, false, out)?;
        }
        Ok(())
    }

    fn expand_one(
        &mut self,
        node: &KdlNode,
        file: &Arc<SourceFile>,
        via: &[Via],
        scope: ScopeId,
        top: bool,
        out: &mut Vec<RawNode>,
    ) -> Result<(), Error> {
        match node.name().value() {
            "let" => self.bind_lets(node, file, via, scope),
            "template" if top => Ok(()),
            "template" => Err(Error::new("templates are defined at the top level")
                .at(node_origin(node, file, via))
                .element("template")
                .help("move this template to the top level of a file (included files count, when they are included at the top level)")),
            "include" => {
                let (included, doc) = self.read_include(node, file, via)?;
                if let Some(t) = doc.nodes().iter().find(|n| n.name().value() == "template") {
                    return Err(Error::new("templates are defined at the top level, but this file is included inside a node")
                        .at(node_origin(t, &included, &[]))
                        .element("template")
                        .note(format!("included at {}", loc(file, node.name().span())))
                        .help("include this file at the top level, or move its templates to one that is"));
                }
                let via = with_via(
                    Via::Include {
                        at: loc(file, node.name().span()),
                    },
                    via,
                );
                self.including.push(included.path().to_path_buf());
                self.expand_block(doc.nodes(), &included, &via, scope, out)?;
                self.including.pop();
                Ok(())
            }
            "each" => self.each(node, file, via, scope, out),
            _ => {
                let n = self.node(node, file, via, scope)?;
                out.push(n);
                Ok(())
            }
        }
    }

    fn bind_lets(
        &mut self,
        node: &KdlNode,
        file: &Arc<SourceFile>,
        via: &[Via],
        scope: ScopeId,
    ) -> Result<(), Error> {
        let usage =
            "write `let name=value`, for example `let tail=(s)0.5`; several may share a line";
        if let Some(arg) = node.entries().iter().find(|e| e.name().is_none()) {
            return Err(Error::new("`let` binds named values; this one has no name")
                .at(origin(file, arg.span(), via))
                .element("let")
                .help(usage));
        }
        if node.children().is_some() || node.entries().is_empty() {
            return Err(Error::new("`let` takes properties and no children")
                .at(node_origin(node, file, via))
                .element("let")
                .help(usage));
        }
        for e in node.entries() {
            let name = e.name().map(|n| n.value().to_owned()).unwrap_or_default();
            let entry = convert_entry(e, file, via)?;
            if crate::RUN_TIME.contains(&name.as_str()) {
                return Err(Error::new(format!(
                    "`{name}` can't be bound: `${name}` is filled in when a command runs"
                ))
                .at(entry.origin)
                .element(format!("let {name}"))
                .help(format!(
                    "choose another name; {} are reserved",
                    crate::RUN_TIME.map(|n| format!("${n}")).join(", ")
                )));
            }
            if let Some(prev) = self.scopes[scope].bindings.iter().find(|b| b.name == name) {
                return Err(
                    Error::new(format!("`{name}` is bound twice in the same block"))
                        .at(entry.origin)
                        .element(format!("let {name}"))
                        .note(format!("first bound at {}", prev.entry.origin.at))
                        .help("rename one, or remove one; a nested block may shadow an outer name"),
                );
            }
            self.scopes[scope].bindings.push(Binding {
                name,
                entry,
                state: RefCell::new(Slot::Pending),
            });
        }
        Ok(())
    }

    fn each(
        &mut self,
        node: &KdlNode,
        file: &Arc<SourceFile>,
        via: &[Via],
        scope: ScopeId,
        out: &mut Vec<RawNode>,
    ) -> Result<(), Error> {
        let at = node_origin(node, file, via);
        let usage = "write `each \"synth\" \"vxn1c\" \"vxn2b\" { band name=$synth }`: the name to bind, then the items";
        let mut args = node.entries().iter().filter(|e| e.name().is_none());
        let binding = match args.next() {
            Some(e) if e.ty().is_none() => e.value().as_string().map(str::to_owned),
            _ => None,
        };
        let Some(binding) = binding else {
            return Err(
                Error::new("`each` starts with the name to bind, as a string")
                    .at(at)
                    .element("each")
                    .help(usage),
            );
        };
        let element = format!("each {binding:?}");
        let mut index_name = None;
        for e in node.entries().iter().filter(|e| e.name().is_some()) {
            let key = e.name().map_or("", |n| n.value());
            match (key, e.value()) {
                ("index", KdlValue::String(s)) if e.ty().is_none() => index_name = Some(s.clone()),
                _ => {
                    return Err(Error::new(format!("`each` has no property `{key}`"))
                        .at(origin(file, e.span(), via))
                        .element(element)
                        .help("the only property is `index=\"i\"`, which binds $i to 0, 1, 2…"));
                }
            }
        }
        let items: Vec<&KdlEntry> = args.collect();
        if items.is_empty() {
            return Err(Error::new(format!("`each \"{binding}\"` has no items"))
                .at(at)
                .element(element)
                .help(usage));
        }
        let Some(body) = node.children() else {
            return Err(Error::new(format!("`each \"{binding}\"` has no body"))
                .at(at)
                .element(element)
                .help("put the nodes to repeat in braces: each \"x\" 1 2 { … }"));
        };
        for (i, item) in items.into_iter().enumerate() {
            let entry = convert_entry(item, file, via)?;
            let RawValue::Lit(value) = &entry.value else {
                return Err(Error::new("`each` expands over a literal list; this item is computed")
                    .at(entry.origin)
                    .element(element)
                    .help("write the items out as values; data-driven structure belongs to a generator component (ADR 0005 §4)"));
            };
            let item_text = value.to_string();
            let inner = self.new_scope(Some(scope));
            self.scopes[inner].bindings.push(Binding {
                name: binding.clone(),
                entry: entry.clone(),
                state: RefCell::new(Slot::Pending),
            });
            if let Some(index_name) = &index_name {
                #[allow(clippy::cast_precision_loss)]
                let index = Value::Number(i as f64);
                self.scopes[inner].bindings.push(Binding {
                    name: index_name.clone(),
                    entry: RawEntry {
                        value: RawValue::Lit(index),
                        origin: entry.origin.clone(),
                    },
                    state: RefCell::new(Slot::Pending),
                });
            }
            let via = with_via(
                Via::Each {
                    binding: binding.clone(),
                    item: item_text,
                    at: at.at.clone(),
                },
                via,
            );
            self.expand_block(body.nodes(), file, &via, inner, out)?;
        }
        Ok(())
    }

    /// An ordinary node, with its template applied.
    fn node(
        &mut self,
        node: &KdlNode,
        file: &Arc<SourceFile>,
        via: &[Via],
        scope: ScopeId,
    ) -> Result<RawNode, Error> {
        let kind = node.name().value().to_owned();
        let origin_ = node_origin(node, file, via);
        if let Some(ty) = node.ty() {
            return Err(Error::new(format!(
                "a node can't carry a type annotation: ({})",
                ty.value()
            ))
            .at(origin(file, ty.span(), via))
            .element(kind)
            .help("units go on values: from=(bar)4"));
        }
        let mut args = Vec::new();
        let mut props: Vec<RawProp> = Vec::new();
        let mut template_use = None;
        for e in node.entries() {
            let entry = convert_entry(e, file, via)?;
            match e.name().map(kdl::KdlIdentifier::value) {
                None => args.push(entry),
                Some("use") => match entry.value {
                    RawValue::Lit(Value::String(t)) => template_use = Some((t, entry.origin.at)),
                    _ => {
                        return Err(Error::new("`use` names a template, as a plain string")
                            .at(entry.origin)
                            .element(describe(&kind, node_name(node)))
                            .help("write use=\"close-up\""));
                    }
                },
                Some(key) => set_prop(&mut props, key, entry),
            }
        }
        let child_scope = self.new_scope(Some(scope));
        let mut children = Vec::new();
        if let Some(doc) = node.children() {
            self.expand_block(doc.nodes(), file, via, child_scope, &mut children)?;
        }
        let own = RawNode {
            kind,
            origin: origin_,
            args,
            props,
            children,
            scope,
        };
        let Some((template, use_at)) = template_use else {
            return Ok(own);
        };
        let (props, children) = self.template_body(&template, &use_at, via, child_scope, &own)?;
        let base = RawNode {
            props,
            children,
            ..own.clone()
        };
        Ok(merge(base, own))
    }

    /// A template's properties and children, with the templates it uses
    /// applied first. Its children are expanded in the using node's child
    /// scope, so `$name`s in them resolve where the template is used.
    fn template_body(
        &mut self,
        name: &str,
        use_at: &Location,
        via: &[Via],
        child_scope: ScopeId,
        user: &RawNode,
    ) -> Result<(Vec<RawProp>, Vec<RawNode>), Error> {
        let at = Origin {
            at: use_at.clone(),
            via: via.to_vec(),
        };
        if let Some(i) = self.applying.iter().position(|t| t == name) {
            let mut chain: Vec<&str> = self.applying[i..].iter().map(String::as_str).collect();
            chain.push(name);
            return Err(Error::new(format!(
                "templates use each other in a cycle: {}",
                chain.join(" → ")
            ))
            .at(at)
            .element(user.describe())
            .help(
                "a template may build on another with `use`, but not on itself, even indirectly",
            ));
        }
        let Some(t) = self.templates.get(name).cloned() else {
            let help = suggest(name, "templates", self.templates.keys().map(String::as_str));
            return Err(Error::new(format!("there is no template \"{name}\""))
                .at(at)
                .element(user.describe())
                .help(format!(
                    "{help}. Templates are defined at the top level: template \"{name}\" … {{ … }}"
                )));
        };
        self.applying.push(name.to_owned());
        let via = with_via(
            Via::Template {
                name: name.to_owned(),
                defined: t.at.clone(),
                used: use_at.clone(),
            },
            via,
        );
        let mut props = Vec::new();
        let mut children = Vec::new();
        let mut parent = None;
        for e in t.node.entries().iter().skip(1) {
            let entry = convert_entry(e, &t.file, &via)?;
            match e.name().map(kdl::KdlIdentifier::value) {
                Some("use") => match entry.value {
                    RawValue::Lit(Value::String(p)) => parent = Some((p, entry.origin.at)),
                    _ => {
                        return Err(Error::new("`use` names a template, as a plain string")
                            .at(entry.origin)
                            .element(format!("template {name:?}"))
                            .help("write use=\"close-up\""));
                    }
                },
                Some(key) => set_prop(&mut props, key, entry),
                None => unreachable!("checked when the template was defined"),
            }
        }
        if let Some((parent, parent_used)) = parent {
            let (base_props, base_children) =
                self.template_body(&parent, &parent_used, &via, child_scope, user)?;
            let mut merged = base_props;
            for p in props {
                set_prop(&mut merged, &p.key, p.entry);
            }
            props = merged;
            children = base_children;
        }
        if let Some(doc) = t.node.children() {
            let mut own = Vec::new();
            self.expand_block(doc.nodes(), &t.file, &via, child_scope, &mut own)?;
            children = merge_children(children, own);
        }
        self.applying.pop();
        Ok((props, children))
    }
}

fn node_name(node: &KdlNode) -> Option<&str> {
    node.entries()
        .first()
        .filter(|e| e.name().is_none())
        .and_then(|e| e.value().as_string())
}

/// Sets `key`, replacing an earlier value in place (KDL: the last one
/// wins; templates: the user's value wins, in the template's order).
fn set_prop(props: &mut Vec<RawProp>, key: &str, entry: RawEntry) {
    if let Some(p) = props.iter_mut().find(|p| p.key == key) {
        p.entry = entry;
    } else {
        props.push(RawProp {
            key: key.to_owned(),
            entry,
        });
    }
}

/// `over` applied on top of `base`: its properties win, its arguments
/// replace `base`'s if it has any, and its children merge.
fn merge(base: RawNode, over: RawNode) -> RawNode {
    let mut props = base.props;
    for p in over.props {
        set_prop(&mut props, &p.key, p.entry);
    }
    RawNode {
        kind: over.kind,
        origin: over.origin,
        args: if over.args.is_empty() {
            base.args
        } else {
            over.args
        },
        props,
        children: merge_children(base.children, over.children),
        scope: over.scope,
    }
}

/// A child of `over` with the same kind and name as a child of `base`
/// merges into it, in `base`'s place; the rest follow, in order.
fn merge_children(mut base: Vec<RawNode>, over: Vec<RawNode>) -> Vec<RawNode> {
    for child in over {
        let slot = child.name().and_then(|name| {
            base.iter()
                .position(|b| b.kind == child.kind && b.name() == Some(name))
        });
        match slot {
            Some(i) => {
                let b = base.remove(i);
                base.insert(i, merge(b, child));
            }
            None => base.push(child),
        }
    }
    base
}

/// Where the string's text starts in the file, if the source spells it
/// plainly (no escapes, not multi-line), so expression errors can point
/// inside it.
fn content_offset(file: &SourceFile, entry: &KdlEntry, s: &str) -> Option<usize> {
    let span = entry.span();
    let text = &file.text()[span.offset()..span.offset() + span.len()];
    let close = text.find(')')? + 1;
    let rest = &text[close..];
    let lead = rest.len() - rest.trim_start().len();
    let mut i = close + lead;
    while text[i..].starts_with('#') {
        i += 1;
    }
    if !text[i..].starts_with('"') || text[i..].starts_with("\"\"\"") {
        return None;
    }
    i += 1;
    (text.get(i..i + s.len()) == Some(s)).then_some(span.offset() + i)
}

/// Whether `s` starts with a run-time reference ([`crate::RUN_TIME`]),
/// or an escaped one: `$out`, `$in.shot/panel.mp4`, `$$out`, but not
/// `$outer`.
fn is_run_time(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("$$").or_else(|| s.strip_prefix('$')) else {
        return false;
    };
    let head_len = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    crate::RUN_TIME.contains(&&rest[..head_len])
}

/// A KDL entry's value, typed: units, `(expr)` and `$` references.
#[allow(clippy::many_single_char_names)]
pub(crate) fn convert_entry(
    e: &KdlEntry,
    file: &Arc<SourceFile>,
    via: &[Via],
) -> Result<RawEntry, Error> {
    let origin = origin(file, e.span(), via);
    let element = || match e.name() {
        Some(n) => format!("property `{}`", n.value()),
        None => "an argument".to_owned(),
    };
    let fail = |message: String, help: &str| {
        Error::new(message)
            .at(origin.clone())
            .element(element())
            .help(help)
    };
    let ty = e.ty().map(kdl::KdlIdentifier::value);
    let value = match (ty, e.value()) {
        (None, KdlValue::String(s)) if is_run_time(s) => RawValue::Lit(Value::String(s.clone())),
        (None, KdlValue::String(s)) if s.starts_with("$$") => {
            RawValue::Lit(Value::String(s[1..].to_owned()))
        }
        (None, KdlValue::String(s)) if s.starts_with('$') => {
            if !valid_path(&s[1..]) {
                return Err(fail(
                    format!("`{s}` isn't a valid reference"),
                    "references look like $tail, $self.start, $parent.length or $prev.camera.end; write `$$` for a literal `$`",
                ));
            }
            RawValue::Ref(s[1..].to_owned())
        }
        (None, v) => RawValue::Lit(plain(v).map_err(|m| fail(m, "write numbers within ±2⁵³"))?),
        (Some("expr"), KdlValue::String(s)) => RawValue::Expr {
            src: s.clone(),
            content: content_offset(file, e, s),
        },
        (Some("expr"), v) => {
            return Err(fail(
                format!("(expr) takes a string, not {v}"),
                "quote the expression: (expr)\"$self.length + 0.5s\"",
            ));
        }
        (Some(u), v) => {
            let Some(unit) = Unit::from_suffix(u) else {
                let close = crate::error::did_you_mean(u, ["bar", "beat", "s", "f", "expr"]);
                let base = "videbimus knows (bar), (beat), (s), (f) and (expr)";
                return Err(fail(
                    format!("unknown type annotation ({u})"),
                    &match close {
                        Some(c) => format!("did you mean ({c})? {base}"),
                        None => base.to_owned(),
                    },
                ));
            };
            let amount = match v {
                KdlValue::Float(f) => *f,
                KdlValue::Integer(i) => match plain(v) {
                    Ok(Value::Number(n)) => n,
                    _ => {
                        return Err(fail(
                            format!("{i} is too large"),
                            "write numbers within ±2⁵³",
                        ));
                    }
                },
                KdlValue::String(s) => {
                    return Err(fail(
                        format!("({u}) takes a number, not the string {s:?}"),
                        &format!("write ({u})4 for a literal, or (expr)\"…\" to compute one"),
                    ));
                }
                other => {
                    return Err(fail(
                        format!("({u}) takes a number, not {other}"),
                        &format!("write ({u})4"),
                    ));
                }
            };
            if unit == Unit::Frame && amount.fract() != 0.0 {
                return Err(fail(
                    format!("(f){amount} isn't a whole number of frames"),
                    "write a whole number of frames, or use (s) for a time between frames",
                ));
            }
            RawValue::Lit(Value::Time(Time::new(amount, unit)))
        }
    };
    Ok(RawEntry { value, origin })
}

/// An unannotated KDL value.
fn plain(v: &KdlValue) -> Result<Value, String> {
    const EXACT: i128 = 1 << 53;
    Ok(match v {
        KdlValue::String(s) => Value::String(s.clone()),
        KdlValue::Integer(i) if (-EXACT..=EXACT).contains(i) =>
        {
            #[allow(clippy::cast_precision_loss)]
            Value::Number(*i as f64)
        }
        KdlValue::Integer(i) => return Err(format!("{i} is too large to hold exactly")),
        KdlValue::Float(f) => Value::Number(*f),
        KdlValue::Bool(b) => Value::Bool(*b),
        KdlValue::Null => Value::Null,
    })
}
