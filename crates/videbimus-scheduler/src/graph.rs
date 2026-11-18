//! The task graph: tasks, their labelled inputs, and where they sit in
//! the scene's hierarchy.

use std::fmt;
use std::sync::Arc;

use serde::Serialize;
use videbimus_core::Interval;

use crate::task::Task;

/// A task's place in a [`Graph`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct NodeId(pub(crate) usize);

impl NodeId {
    /// Its index in the graph, in the order tasks were added.
    #[must_use]
    pub fn index(self) -> usize {
        self.0
    }
}

/// A task with its inputs and group, to [`add`](Graph::add) to a graph.
#[must_use]
pub struct Node {
    task: Arc<dyn Task>,
    inputs: Vec<(String, NodeId)>,
    group: Option<String>,
}

impl Node {
    /// A task with no inputs.
    pub fn new(task: impl Task) -> Self {
        Self::from_arc(Arc::new(task))
    }

    /// A shared task with no inputs.
    pub fn from_arc(task: Arc<dyn Task>) -> Self {
        Self {
            task,
            inputs: Vec::new(),
            group: None,
        }
    }

    /// Adds another task's output as an input, under the role `label`
    /// (the label it has in the key).
    pub fn input(mut self, label: &str, node: NodeId) -> Self {
        self.inputs.push((label.to_owned(), node));
        self
    }

    /// Sets the scene path the task belongs under (`scenes/s07`), for the
    /// progress tree (ADR 0007 §2).
    pub fn group(mut self, path: &str) -> Self {
        self.group = Some(path.to_owned());
        self
    }
}

pub(crate) struct NodeDef {
    pub task: Arc<dyn Task>,
    pub name: String,
    pub inputs: Vec<(String, NodeId)>,
    pub group: Option<String>,
    pub dependants: Vec<NodeId>,
}

/// Tasks and their dependencies. Inputs must be added before the tasks
/// that read them, so a graph is acyclic by construction and its node
/// order is a topological order.
#[derive(Default)]
pub struct Graph {
    pub(crate) nodes: Vec<NodeDef>,
}

/// Why a node couldn't be added to a [`Graph`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphError {
    /// The task's name is empty.
    EmptyName,
    /// Another task already has this name.
    DuplicateName {
        /// The name.
        name: String,
    },
    /// An input isn't a node of this graph.
    UnknownInput {
        /// The task.
        task: String,
        /// The input's label.
        label: String,
    },
    /// Two inputs share a label.
    DuplicateLabel {
        /// The task.
        task: String,
        /// The label.
        label: String,
    },
    /// An input's label is empty.
    EmptyLabel {
        /// The task.
        task: String,
    },
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphError::EmptyName => f.write_str(
                "a task has an empty name; give every task its scene slot, e.g. `s07/blender`",
            ),
            GraphError::DuplicateName { name } => write!(
                f,
                "two tasks are named `{name}`; names are store slots and must be unique"
            ),
            GraphError::UnknownInput { task, label } => write!(
                f,
                "task `{task}`: input `{label}` isn't in this graph; add the input's task first"
            ),
            GraphError::DuplicateLabel { task, label } => write!(
                f,
                "task `{task}`: two inputs are labelled `{label}`; give each its own role"
            ),
            GraphError::EmptyLabel { task } => {
                write!(f, "task `{task}`: an input has an empty label")
            }
        }
    }
}

impl std::error::Error for GraphError {}

impl Graph {
    /// An empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a task.
    ///
    /// # Errors
    ///
    /// An empty or duplicate name, or an input that is unknown, unlabelled
    /// or labelled twice.
    pub fn add(&mut self, node: Node) -> Result<NodeId, GraphError> {
        let name = node.task.name().to_owned();
        if name.is_empty() {
            return Err(GraphError::EmptyName);
        }
        if self.nodes.iter().any(|n| n.name == name) {
            return Err(GraphError::DuplicateName { name });
        }
        for (i, (label, id)) in node.inputs.iter().enumerate() {
            if label.is_empty() {
                return Err(GraphError::EmptyLabel { task: name });
            }
            if id.0 >= self.nodes.len() {
                return Err(GraphError::UnknownInput {
                    task: name,
                    label: label.clone(),
                });
            }
            if node.inputs[..i].iter().any(|(l, _)| l == label) {
                return Err(GraphError::DuplicateLabel {
                    task: name,
                    label: label.clone(),
                });
            }
        }
        let id = NodeId(self.nodes.len());
        for (_, input) in &node.inputs {
            let deps = &mut self.nodes[input.0].dependants;
            if !deps.contains(&id) {
                deps.push(id);
            }
        }
        self.nodes.push(NodeDef {
            task: node.task,
            name,
            inputs: node.inputs,
            group: node.group,
            dependants: Vec::new(),
        });
        Ok(id)
    }

    /// How many tasks it has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether it has none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The node named `name`.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<NodeId> {
        self.nodes.iter().position(|n| n.name == name).map(NodeId)
    }

    /// The task at `id`.
    ///
    /// # Panics
    ///
    /// If `id` isn't from this graph.
    #[must_use]
    pub fn task(&self, id: NodeId) -> &Arc<dyn Task> {
        &self.nodes[id.0].task
    }

    /// The nodes nothing depends on: the final outputs.
    #[must_use]
    pub fn sinks(&self) -> Vec<NodeId> {
        (0..self.nodes.len())
            .filter(|&i| self.nodes[i].dependants.is_empty())
            .map(NodeId)
            .collect()
    }
}

/// An output a run is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    /// The task.
    pub node: NodeId,
    /// Only these frames of it (a preview), or all of it. A partial target
    /// builds the chunks covering its frames and leaves the asset
    /// incomplete; its inputs are built whole.
    pub frames: Option<Interval>,
}

impl Target {
    /// All of `node`.
    #[must_use]
    pub fn all(node: NodeId) -> Self {
        Self { node, frames: None }
    }

    /// Only `frames` of `node`.
    #[must_use]
    pub fn frames(node: NodeId, frames: Interval) -> Self {
        Self {
            node,
            frames: Some(frames),
        }
    }
}

impl From<NodeId> for Target {
    fn from(node: NodeId) -> Self {
        Self::all(node)
    }
}
