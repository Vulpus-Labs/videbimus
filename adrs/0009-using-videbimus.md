# ADR 0009: Using videbimus: the stock binary, project binaries and kind registration

- **Status:** Accepted
- **Date:** 2026-11-18

## Context

A project needs a way to use videbimus. Some videos need nothing beyond the
built-in components, plus external programs such as Blender, ffmpeg and
scripts. Others need custom code. Fenland's backdrop is a stateful Rust
renderer called once per frame on pixels. Its song render drives synth
engines that are Rust crates.

Loading custom code into one videbimus binary was considered:

- **Rust dynamic libraries:** there is no stable ABI, so a plugin must be
  built with the exact same compiler.
- **WebAssembly:** each frame's 8 MB of pixels must be copied across the
  boundary, and plugins can't run subprocesses.
- **Subprocesses:** every step of a per-frame component becomes a round
  trip.

All three are costly, or fragile, for per-frame components.

## Decision

1. **videbimus is a library with a CLI entry point.** `videbimus::App` holds a
   registry of component kinds and recipe kinds. `App::run()` is the whole
   CLI: `build`, `status`, `review`, `preview` and `stop`. It behaves the
   same in every binary that calls it.
2. **The stock `videbimus` binary** is `App::new("videbimus").run()` with only the
   built-in kinds:
   - components: sequence, stack, crossfade, filter, frame player, caption,
     image and text;
   - recipes: the `command` recipe (§3), file inputs, and encode/mux.

   A project with no custom code is a directory of KDL run by `videbimus`.
3. **The `command` recipe runs any external program as an asset.** Config
   declares:
   - its argument list, with `$in.*`, `$out`, `$chunk.*` and `$cores`
     substituted;
   - the tools it runs, and how to read their versions;
   - the scripts it reads, which are hashed into its key;
   - its output (a file, or frames by pattern) and its chunking;
   - its cost, cores, memory, and whether it may run remotely.

   Blender, ffmpeg, Python and existing binaries are all command recipes.
4. **A project binary registers its own kinds.** A crate with custom
   kinds has a `main` like:

   ```rust
   fn main() -> std::process::ExitCode {
       videbimus::App::new("fenland")
           .component::<FenBackdrop>()
           .recipe::<SongRender>()
           .run()
   }
   ```

   A kind implements `ComponentKind` or `RecipeKind`:
   - `KIND`: its name in config;
   - `from_config(&Node, &Resolver)`: typed parameters, with located
     errors. Component kinds also get a `&Builder` (ticket 0006), which
     places them in their parent's frames and builds their children;
   - the `Component` or `Recipe` methods.

   Built-in kinds are registered the same way.
5. **Code identity is per kind.** Each kind contributes a code hash to the
   key of everything it builds. A build script (`videbimus-build`) hashes the
   source files a kind declares: its module, plus any extra paths. A
   `VERSION` constant can be set to force a rebuild, for example after a
   dependency changes. Hashing the whole binary is ruled out: any edit
   would invalidate every asset (README goal 1).
6. **A config names the kinds it needs.** At the top:
   `requires "fen" "song-render"`. A binary that lacks one fails at load,
   before anything runs, and names the binary that has it (from a `binary`
   hint in the config): "kind `fen` isn't registered; this config needs
   the `fenland` binary (`cargo run -p fenland --`)".
7. **How it landed** (ticket 0010; crates `videbimus` and
   `videbimus-build`):
   - **Recipe kinds** return a scheduler `Task` from
     `from_config(&Node, &Resolver, &RecipeContext)`. The project owns
     the task's name (it replaces whatever the task reports) and puts the
     kind's code identity into every key the task makes.
     `RecipeContext::key` starts a key with the kind, its code identity
     and the node's canonical config (properties sorted, times as
     seconds; the name, `recipe` and `input`s left out, since they are
     keyed as the kind and the inputs). The built-in recipe registry is
     empty until the `command` recipe (ticket 0011).
   - **Config declares assets** at the top level, optionally inside
     `group "<name>"`:
     `asset "<name>" recipe="<kind>" { input "<label>" "<asset>"; … }`.
     The rest of the node is the kind's. Inputs become graph edges,
     keyed under their labels.
   - **Asset names** are the group path, then the asset's own name,
     which defaults to its recipe kind (its role):
     `group "s07" { asset recipe="blender" }` is `s07/blender`. Names
     are unique.
   - **Components reference assets by name.** A kind that reads an
     asset by name records it while the tree is built
     (`Builder::read_asset`, with the video frames it shows it over and
     the asset frame at the start). A frame player's `asset="…"` must
     name a declared asset; its `store=… name=…` is wired the same way
     when the store is the project's and the name is declared. Building
     the scene builds exactly the assets it reads; a preview builds only
     the asset frames its video frames show (`Target::frames`). A frame
     player's `dir` or `key` is unmanaged: read as it is, never built.
     `Component::needs` (by key) can't drive the graph, since keys are
     only known once inputs are built (ADR 0003 §4); it stays for kinds
     that read a fixed key.
   - **The root** is the top-level component `video`'s `root="…"` names,
     or else the only top-level node that isn't `video`, `requires`,
     `binary`, `store`, `asset` or `group`. Several and no `root` is an
     error naming them; none is a project of assets only.
   - **The store** is `store "<dir>"` (relative to the config file),
     else `.videbimus/store` beside the config; `--store` overrides.
   - **`binary "<name>" run="<command>"`**: `run` is optional. A missing
     kind's error is located at the `requires` argument, and notes which
     binary is running.
   - **Code identity**: `ComponentKind` and `RecipeKind` have
     `CODE` and `VERSION` constants, both empty by default; together they
     are the kind's `CodeIdentity`, hashed as the key's code input.
     `videbimus_build::Kinds` in a build script hashes each kind's
     declared files and directories (relative paths and contents) into
     `$OUT_DIR/videbimus_code.rs`, one constant per kind, included with
     `videbimus::include_code!()`. The built-in component kinds declare
     their own module plus the shared picture modules.
   - **The CLI**: `build [targets…] [--profile P]` builds what the scene
     reads (or named assets or groups; with no scene, every asset), with
     a plain line per scheduler event and Ctrl-C as a graceful stop;
     `preview <frames>` builds what those frames need and writes them as
     PNG; `status` lists each asset's latest build (the live view is
     ticket 0005); `stop` sends Ctrl-C to the build holding the store's
     lock on this machine; `review` waits for ticket 0007. Exit codes: 0,
     1 failed, 2 usage, 130 stopped.

8. **Projects stay outside videbimus.** A project crate depends on `videbimus` and
   keeps its configs alongside. Fenland is such a crate, outside videbimus's
   repository. Its silkscreen-specific kinds live there, never in videbimus.

## Consequences

- A video with no custom code needs no Rust.
- Custom per-frame code runs in-process with no copying, and editing it
  rebuilds only what that kind built.
- Two binaries can serve the same store: keys don't depend on which binary
  built them, only on kinds, config, inputs and tools.
