//! `sequence`: children one after another.

use videbimus_config::{Error, Node, Resolver};
use videbimus_core::{AssetKey, Component, Context, Frame, Image, Interval};

use crate::drive::{Child, load_children, save_children};
use crate::registry::{Builder, ComponentKind};

/// Children in order, each frame shown by the child whose interval holds
/// it (ADR 0002 §5). Frames no child holds are transparent.
///
/// ```kdl
/// sequence from=(bar)0 to=(bar)16 {
///     frame-player "s01" length=(bar)4 dir="frames/s01"   // bars 0–4
///     frame-player "s02" length=(bar)4 dir="frames/s02"   // then 4–8
///     image "card" from=(bar)10 path="card.png"           // 10 to the end
/// }
/// ```
///
/// A child's `from` defaults to where the previous one ended (the
/// sequence's start, for the first), and its end to the sequence's end.
/// Children may leave gaps but not overlap: overlapping is a crossfade's
/// or a stack's job.
#[derive(Debug)]
pub struct Sequence {
    interval: Interval,
    children: Vec<Child>,
}

impl Sequence {
    /// A sequence of `children`, whose intervals are in its frames and
    /// must be in order without overlapping.
    ///
    /// # Panics
    ///
    /// If two children overlap or are out of order.
    #[must_use]
    pub fn new(interval: Interval, children: Vec<Child>) -> Self {
        for pair in children.windows(2) {
            assert!(
                pair[0].interval().end <= pair[1].interval().start,
                "sequence children overlap or are out of order"
            );
        }
        Self { interval, children }
    }

    /// Its children.
    #[must_use]
    pub fn children(&self) -> &[Child] {
        &self.children
    }
}

impl Component for Sequence {
    fn interval(&self) -> Interval {
        self.interval
    }

    fn needs(&self, ctx: &Context) -> Vec<AssetKey> {
        self.children.iter().flat_map(|c| c.needs(ctx)).collect()
    }

    fn reset(&mut self, _: &Context) {}

    fn render(&mut self, n: Frame, ctx: &Context, out: &mut Image) {
        // children are in order: find the one holding n
        let i = self.children.partition_point(|c| c.interval().end <= n);
        if let Some(c) = self.children.get_mut(i) {
            c.render(n, ctx, out);
        }
    }

    fn save_state(&self) -> Option<Vec<u8>> {
        save_children(&self.children)
    }

    fn load_state(&mut self, bytes: &[u8]) -> bool {
        load_children(&mut self.children, bytes)
    }
}

impl ComponentKind for Sequence {
    const KIND: &'static str = "sequence";
    const CODE: &'static str = crate::code::SEQUENCE;

    fn from_config(node: &Node, r: &Resolver, b: &Builder) -> Result<Box<dyn Component>, Error> {
        b.check_props(node, r, &[])?;
        let interval = b.interval(node, r)?;
        let inner = b.inside(interval);
        let mut children: Vec<Child> = Vec::new();
        let mut next = Frame(0);
        for c in node.children() {
            let child = inner.offering(next, None).build_child(c, r)?;
            let iv = child.interval();
            if iv.start < next {
                return Err(r
                    .error(
                        c,
                        c.origin(),
                        format!(
                            "it starts {} frames before the previous child ends",
                            next - iv.start
                        ),
                    )
                    .help(
                        "children of a sequence follow each other; to overlap them, use a crossfade or a stack",
                    ));
            }
            next = iv.end;
            children.push(child);
        }
        Ok(Box::new(Self::new(interval, children)))
    }
}
