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
     errors;
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
7. **Projects stay outside videbimus.** A project crate depends on `videbimus` and
   keeps its configs alongside. Fenland is such a crate, outside videbimus's
   repository. Its silkscreen-specific kinds live there, never in videbimus.

## Consequences

- A video with no custom code needs no Rust.
- Custom per-frame code runs in-process with no copying, and editing it
  rebuilds only what that kind built.
- Two binaries can serve the same store: keys don't depend on which binary
  built them, only on kinds, config, inputs and tools.
