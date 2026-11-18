---
id: "0010"
title: "videbimus::App: kind registry, CLI entry point, per-kind code identity, requires"
priority: high
created: 2026-11-18
epic: E001
depends: ["0002", "0003"]
---

## Summary

ADR 0009 §1, §2 and §4–§6: the library entry point that both the stock
binary and project binaries run.

## Acceptance criteria

- [x] `App::new(name).component::<K>().recipe::<R>().run()`; the CLI
      (`build`, `status`, `review`, `preview`, `stop`) is identical in every
      binary.
- [x] `ComponentKind` and `RecipeKind` traits: `KIND`, `from_config` with
      located errors, and an optional `VERSION`.
- [x] `videbimus-build`: a build-script helper that hashes each kind's declared
      source files into its code identity; editing one kind's source
      changes only that kind's keys.
- [x] `requires` and `binary` in config; a missing kind fails at load and
      names the binary to run.
- [x] The stock `videbimus` binary, with the built-in kinds only.

## Close-out (2026-11-18)

New crates `crates/videbimus` (the library projects depend on, and the
stock binary) and `crates/videbimus-build` (the build-script helper).

- `App::new(name).component::<K>().recipe::<R>().run()`; `run_from(args,
  &Console)` for tests. Duplicate kinds panic, naming the kind.
- `RecipeKind` (`KIND`, `CODE`, `VERSION`, `from_config(&Node, &Resolver,
  &RecipeContext) -> Box<dyn Task>`), `RecipeRegistry` (built-ins empty:
  the `command` recipe lands there in 0011), `RecipeContext::key` and
  `KeyStart` (kind, code identity, canonical config).
- `Project` (`project.rs`): `requires`/`binary` check, `store`,
  `asset`/`group` declarations, name assignment, the graph (inputs first,
  cycles and unknown inputs as located errors), root selection, and the
  scene's asset reads turned into targets (whole, or the asset frames a
  preview's frames show).
- The CLI (`cli.rs`): `build`, `preview`, `status` (latest builds; the
  live view is 0005), `stop` (SIGINT to the store's lock holder on this
  host), `review` ("not yet available", 0007). A hand-written parser:
  clap isn't in the offline registry, and the surface is small.
- `videbimus_build::Kinds` hashes each kind's declared files or
  directories into `$OUT_DIR/videbimus_code.rs`; `videbimus::include_code!`
  includes it. The runtime's built-in kinds use it (`build.rs`).
- Tests: `crates/videbimus/tests/app.rs` (21: registration, duplicates,
  the `requires` message pinned, root selection, names and graph
  errors, wiring and preview targets, per-kind code identity through
  `videbimus-build`, canonical config, CLI `preview` writing the expected
  PNG, `build` then cached, usage errors, the stock binary), 2 CLI unit
  tests, 3 doc tests; `videbimus-build` 5 unit tests.

Changes to other crates (`videbimus-runtime` only):

- `ComponentKind` gained `CODE` and `VERSION` (default empty);
  `ComponentRegistry` keeps a `CodeIdentity` per kind
  (`register_with_code`, `code`). `register_fn` is unchanged.
- `Builder` can record asset reads (`ComponentRegistry::builder_reading`,
  `Builder::read_asset`, `Builder::store`, `Builder::absolute`;
  `AssetRead`, `AssetReads`, `ReadBy`), and `scene_from_config_with`
  builds a scene with such a builder.
- `frame-player` gained `asset="…"` (the project's store) and records
  reads for `asset` and `store`+`name`.
- A `build.rs` gives the built-in kinds code identities.

Decisions, recorded in ADR 0009 §7 (and ADR 0002 §8, ADR 0003 §6):
config syntax for assets, groups, inputs, `store` and `root`; asset
names (group path + own name, defaulting to the recipe kind); reads by
name drive the graph, `dir` and `key` are unmanaged.

Open points:

- `Component::needs` returns keys, which can't drive the graph; it stays
  for fixed keys. Whether to replace it with name-based needs is open.
- Component code identities are recorded but not yet in any key: the
  encoded output (0007) is where they go.
- `status` reports each name's latest build, not whether it matches the
  current config's key (lazy keys make that a scheduler question; 0005).
- `preview` renders after building; it doesn't stream frames while
  assets build (0007's preview).
- `stop` reaches only a build on this machine.
