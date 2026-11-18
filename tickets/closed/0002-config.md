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

- [x] Units `(bar) (beat) (s) (f)`, with unit-checked arithmetic.
- [x] `$name`, `$self.*`, `$parent.*` and `$prev.*` references; cycles
      are errors.
- [x] Templates with overrides, nested; includes; `each` over literal
      lists.
- [x] Errors name the file, the line, the element and the fix.
- [x] Fenland's shot list written in KDL resolves to the same frame
      intervals and camera keys as `shots.py`.

## Close-out (2026-11-18)

`videbimus-config`, on `kdl` 6.7 (KDL v2, with spans) and `videbimus-core`:

- `Config::load(path)` / `load_with(path, &dyn Loader)` (`FsLoader`,
  `MemLoader`) expand includes, templates, `let` and `each`, then resolve
  every value in order into a tree of `Node`s (kind, name, arguments,
  properties, children), each value a `Located<Value>` with its `Origin`:
  file, line, column, and the template/include/`each` chain.
- `Value` is null, bool, number, string or `Time` (`Unit::{Bar, Beat,
  Second, Frame}`). `Timebase` (rate and optional tempo, from the
  top-level `video` node) converts times to `Frame` from absolute
  positions.
- `Resolver` is the read side that kinds will use in
  `from_config(&Node, &Resolver)` (ADR 0009): `get`, `opt`, `arg`,
  `args` through `FromValue` (numbers, integers, strings, booleans,
  `Time`, `Frame`, `Option<T>`), `interval(node, "from", "to")`,
  `check_props`, and `error` for kinds' own checks.
- `Error` displays like a compiler error: file:line:column, the source
  line underlined (inside the expression string where it can be), the
  element, the chain, notes and the fix, with near-miss suggestions for
  properties, templates, names and units.

Tests: 40 (units and conversions, references and cycles, templates,
includes and `each`, pinned error text). `tests/fixtures/fenland-shots.kdl`
holds shots.py's FACE list; `tests/fenland.rs` checks its intervals and
camera keys against values computed from shots.py's formulas, with
`frames = main + round(0.5 s × 30)`.

Deviations, recorded in ADR 0005 §6:

- Expressions are `(expr)"…"`: the ADR's `(=)"…"` isn't valid KDL v2.
- Positions and durations are one type, a time from the start. Correct
  for one constant tempo; a tempo map would need them told apart.
- Added `frames(t)`, so a config can round a length to whole frames the
  way shots.py's camera timing does. A shot's interval (positions
  rounded) and its length rounded as a duration differ by a frame on
  some shots (s07: 276 against 277); the fixture reproduces both.
- shots.py's `fstop = 11 if elev >= 85 else 2.8` is a conditional, which
  config doesn't have; the fixture says it with two key templates,
  `top` and `oblique`.
