//! videbimus: a build system for rendered video ([ADR 0009]).
//!
//! This is the crate a project depends on. A project with no custom code
//! is a directory of KDL run by the stock `videbimus` binary. A project
//! with its own kinds is a small crate whose `main` registers them and
//! hands over to the same command line:
//!
//! ```no_run
//! # use videbimus::config::{Error, Node, Resolver};
//! # use videbimus::{Builder, Component, ComponentKind};
//! # struct FenBackdrop;
//! # impl ComponentKind for FenBackdrop {
//! #     const KIND: &'static str = "fen";
//! #     fn from_config(_: &Node, _: &Resolver, _: &Builder) -> Result<Box<dyn Component>, Error> { todo!() }
//! # }
//! fn main() -> std::process::ExitCode {
//!     videbimus::App::new("fenland")
//!         .component::<FenBackdrop>()
//!         // .recipe::<SongRender>()
//!         .run()
//! }
//! ```
//!
//! - [`App`]: the kind registry and the command line (`build`, `preview`,
//!   `status`, `review`, `stop`), the same in every binary.
//! - [`ComponentKind`] and [`RecipeKind`]: how config makes components
//!   and assets' tasks. Each kind has a code identity (`CODE`, from
//!   `videbimus-build` in the kind crate's build script, and `VERSION`),
//!   which goes into the key of everything it builds; see
//!   [`include_code!`].
//! - [`project`]: what a config's top level means: `requires`, `binary`,
//!   `store`, `asset`, `group`, the root, and how asset names are
//!   assigned.
//! - The building blocks, re-exported: [`core`], [`config`], [`store`],
//!   [`scheduler`] and [`runtime`].
//!
//! [ADR 0009]: ../../../adrs/0009-using-videbimus.md

pub mod app;
pub mod cli;
pub mod project;
pub mod recipe;

pub use app::App;
pub use cli::Console;
pub use project::{Asset, Project};
pub use recipe::{KeyStart, RecipeContext, RecipeKind, RecipeRegistry, canonical_config};

pub use videbimus_config as config;
pub use videbimus_core as core;
pub use videbimus_runtime as runtime;
pub use videbimus_scheduler as scheduler;
pub use videbimus_store as store;

pub use videbimus_core::{Checkpoint, Component, Context, Frame, Horizon, Image, Interval};
pub use videbimus_runtime::{AssetRead, Builder, CodeIdentity, ComponentKind, ReadBy};
pub use videbimus_scheduler::{Cost, Resources, Task, TaskContext, TaskError};
pub use videbimus_store::{KeyBuilder, Shape};

/// Includes the code identities `videbimus-build` generated in this
/// crate's build script: one `pub const` per declared kind, named after
/// it (`song-render` is `SONG_RENDER`). Use it inside a module:
///
/// ```ignore
/// mod code {
///     videbimus::include_code!();
/// }
/// // impl RecipeKind for SongRender { const CODE: &'static str = code::SONG_RENDER; … }
/// ```
#[macro_export]
macro_rules! include_code {
    () => {
        include!(concat!(env!("OUT_DIR"), "/videbimus_code.rs"));
    };
}
