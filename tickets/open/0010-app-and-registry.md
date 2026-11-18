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

- [ ] `App::new(name).component::<K>().recipe::<R>().run()`; the CLI
      (`build`, `status`, `review`, `preview`, `stop`) is identical in every
      binary.
- [ ] `ComponentKind` and `RecipeKind` traits: `KIND`, `from_config` with
      located errors, and an optional `VERSION`.
- [ ] `videbimus-build`: a build-script helper that hashes each kind's declared
      source files into its code identity; editing one kind's source
      changes only that kind's keys.
- [ ] `requires` and `binary` in config; a missing kind fails at load and
      names the binary to run.
- [ ] The stock `videbimus` binary, with the built-in kinds only.
