---
id: "0002"
title: "Config: KDL, units, references, arithmetic, templates, includes, located errors"
priority: high
created: 2026-11-18
epic: E001
depends: ["0001"]
---

## Summary

The config resolver of ADR 0005, as a library: parse KDL v2, expand
includes and templates, resolve references and arithmetic over typed
quantities, convert musical time to frames, and report errors with file,
line and template chain.

## Acceptance criteria

- [ ] Units `(bar) (beat) (s) (f)`, with unit-checked arithmetic.
- [ ] `$name`, `$self.*`, `$parent.*` and `$prev.*` references; cycles
      are errors.
- [ ] Templates with overrides, nested; includes; `each` over literal
      lists.
- [ ] Errors name the file, the line, the element and the fix.
- [ ] Fenland's shot list written in KDL resolves to the same frame
      intervals and camera keys as `shots.py`.
