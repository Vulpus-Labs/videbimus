---
id: "0011"
title: "The command recipe: external programs as assets"
priority: high
created: 2026-11-18
epic: E001
depends: ["0004", "0010"]
---

## Summary

ADR 0009 §3. Blender, ffmpeg, Python scripts and existing binaries as
recipes, configured in KDL, with no Rust.

## Acceptance criteria

- [x] Arguments with `$in.*`, `$out`, `$chunk.start`, `$chunk.end` and
      `$cores` substituted.
- [x] Tools with version probes; scripts hashed into the key.
- [x] Outputs as a file or a frame pattern, with chunking; chunks commit
      atomically (ADR 0008).
- [x] Declared cost, cores, memory and remote eligibility feed the
      scheduler.
- [x] Full stdout and stderr in the task log; a non-zero exit fails the
      chunk, with the last lines shown in the progress view.
- [x] Fenland's Blender shot runs as a command recipe in a test config.

## Close-out (2026-11-18)

The `command` recipe is `videbimus::command` (`CommandRecipe`, its task
`CommandTask`), registered in `RecipeRegistry::with_builtins()`, with a
code identity from a new `crates/videbimus/build.rs`. Its KDL syntax and
decisions are in ADR 0009 §3 ("How it landed").

- `run` arguments, `env` values and `cwd` take `$in.<label>`, `$out`,
  `$chunk.start`, `$chunk.last` (inclusive), `$chunk.end` (exclusive, like
  `to`), `$chunk.count`, `$cores`, `$script.<name>` and `$source.<name>`;
  `$$` is a literal `$`, other `$names` pass through for a shell. A frame
  sequence input is linked into one temporary directory per run.
- `tool` probes (`version-from`, optional `version-match` regex, or
  `version`) run at load, cached per process; only the version is keyed.
  `script` and `source` files or directories are hashed at load.
- `output file=`/`dir=`, or `output frames="####.png" from= to= chunk=`;
  outputs are checked after a successful exit. `cost` (`per-frame` or
  `per-run`, `cores`, `mem`, `gpu`, `remote`) feeds the scheduler and is
  not keyed, nor is `chunk`.
- Tests: `crates/videbimus/tests/command.rs` (9, plus the ignored
  `fenland_blender_shot`), 4 unit tests in the module, 1 config test.
  All pass under `nice`.
- Fenland: `examples/fenland-blender-shot/` (paths in an uncommitted
  `local.kdl`; see its README). The ignored test rendered frames 1–2 of
  s14 at 480×270, 4 samples, through `videbimus build`: 2 assets built,
  the Blender chunk in about 6 s, frames correct.

Changes outside the module:

- `videbimus-config`: strings starting with `$in`, `$out`, `$chunk`,
  `$cores`, `$script` or `$source` (or `$$` and one of them) are kept as
  written (`RUN_TIME`), and those names can't be `let`-bound. Without
  this, `run "blender" "$out"` was a config error. ADR 0005 §2 updated.
- `recipe.rs`: `RecipeContext::key_skipping` and
  `canonical_config_skipping`, so tools, scripts, cost and chunk size
  aren't keyed as config.
- `videbimus` gained `regex`, `shlex` and `blake3` dependencies (all
  already in the registry cache; `shlex` and `blake3` were already in the
  lock file).

Open points:

- Probing at load means every command (`status` too) runs each tool's
  probe once; a missing tool fails the load even for commands that
  wouldn't build it.
- Programs that aren't declared tools run unkeyed (`sh`, `cp`).
- `remote` is stored on the task only; nothing reads it yet.
- `hwscene.py` reads control models from a path fixed in the script,
  which isn't hashed.
