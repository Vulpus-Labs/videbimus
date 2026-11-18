//! [`App`]: a binary's kinds, and its command line ([ADR 0009] §1, §4).
//!
//! [ADR 0009]: ../../../adrs/0009-using-videbimus.md

use std::path::Path;
use std::process::ExitCode;

use videbimus_config::Error;
use videbimus_runtime::{ComponentKind, ComponentRegistry};

use crate::cli::{self, Console};
use crate::project::Project;
use crate::recipe::{RecipeKind, RecipeRegistry};

/// A videbimus binary: its name, its component and recipe kinds (the
/// built-in ones, and the project's), and the command line.
///
/// ```no_run
/// fn main() -> std::process::ExitCode {
///     videbimus::App::new("videbimus").run()
/// }
/// ```
#[derive(Clone, Debug)]
#[must_use]
pub struct App {
    name: String,
    components: ComponentRegistry,
    recipes: RecipeRegistry,
}

impl App {
    /// The binary called `name` (the name `requires` errors and `binary`
    /// hints use), with the built-in kinds.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            components: ComponentRegistry::with_builtins(),
            recipes: RecipeRegistry::with_builtins(),
        }
    }

    /// Registers component kind `K`.
    ///
    /// # Panics
    ///
    /// If a component kind of that name is already registered (built-in
    /// or not): two kinds can't share a name in config.
    pub fn component<K: ComponentKind>(mut self) -> Self {
        self.components.register::<K>();
        self
    }

    /// Registers recipe kind `K`.
    ///
    /// # Panics
    ///
    /// If a recipe kind of that name is already registered.
    pub fn recipe<K: RecipeKind>(mut self) -> Self {
        self.recipes.register::<K>();
        self
    }

    /// Its name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its component kinds.
    #[must_use]
    pub fn components(&self) -> &ComponentRegistry {
        &self.components
    }

    /// Its component kinds, to register kinds that aren't types.
    pub fn components_mut(&mut self) -> &mut ComponentRegistry {
        &mut self.components
    }

    /// Its recipe kinds.
    #[must_use]
    pub fn recipes(&self) -> &RecipeRegistry {
        &self.recipes
    }

    /// Its recipe kinds, to register kinds that aren't types.
    pub fn recipes_mut(&mut self) -> &mut RecipeRegistry {
        &mut self.recipes
    }

    /// Loads the config at `config` ([`Project::load`]).
    ///
    /// # Errors
    ///
    /// As [`Project::load`].
    pub fn load(&self, config: &Path, store: Option<&Path>) -> Result<Project, Error> {
        Project::load(self, config, store)
    }

    /// The command line: parses the process's arguments, runs the
    /// command, and returns the exit code. Ctrl-C stops a build
    /// gracefully; a second kills it.
    #[must_use]
    pub fn run(self) -> ExitCode {
        cli::run(&self, std::env::args().skip(1), &Console::std(), true)
    }

    /// The command line with `args` (without the program name), writing
    /// to `console`, without taking over Ctrl-C: for tests and embedding.
    #[must_use]
    pub fn run_from<I, S>(&self, args: I, console: &Console) -> ExitCode
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        cli::run(self, args.into_iter().map(Into::into), console, false)
    }
}
