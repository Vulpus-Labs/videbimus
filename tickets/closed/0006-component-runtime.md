---
id: "0006"
title: "Component runtime: sequence, stack, filter, frame player, caption, image; stateful stepping and checkpoints"
priority: high
created: 2026-11-18
epic: E001
depends: ["0001", "0002", "0003"]
---

## Acceptance criteria

- [x] Components built from resolved config; parents own their children.
- [x] Random access through `plan_catch_up`, with checkpoints every K
      frames; hidden components are not ticked.
- [x] Sequence, stack (z-order, opacity curve, blend), filter, frame
      player, caption and image kinds.
- [x] A crossfade is a stack of two overlapping children.

## Close-out (2026-11-18)

New crate `crates/videbimus-runtime`:

- `registry.rs`: `ComponentKind` (`KIND`, `from_config(&Node, &Resolver,
  &Builder)`), `ComponentRegistry` (`with_builtins`, `register`/`with`,
  `register_fn`, `build`; unknown kinds are located errors with
  did-you-mean), and `Builder`, which places a node in its parent's
  frames (`from`, `to` | `length`, absolute times) and builds children.
- `drive.rs`: `Child` drives one component through `plan_catch_up`, with
  checkpoints every K frames (`DriveOptions`); `Scene` is the top of a
  tree, with a small window of recent frames. `scene_from_config` builds
  one from a config's `video` node (`rate`, `width`, `height`) and a root.
- `kinds/`: `sequence`, `stack`, `crossfade` (expands to a stack),
  `filter` (`fade`, `vignette`, `grain` effects), `frame-player` (a
  directory, or a store asset by `key` or `name`; placeholders for
  missing frames), `image`, `caption` (`fontdue`).
- `color.rs`: sRGB ↔ linear, PNG read (to premultiplied linear) and
  write (8-bit sRGB, for tests and debugging).
- Tests: 20 unit, 16 integration (KDL scenes through the registry), 2
  doc. Test font: Jost 500 (OFL, licence included) in `tests/fonts/jost`.

Deviations and decisions, recorded in ADR 0002 §8 and ADR 0009 §4:

- `from_config` takes a third argument, `&Builder`.
- Config times are absolute; intervals are stored parent-relative.
- Containers report horizon 0 and catch their children up themselves.
- Stack layer properties (`opacity`, `fade-in`, `fade-out`, `ease`,
  `blend`, `z`) sit on the child's own node; kinds check properties
  through `Builder::check_props` so these are accepted.
- Effects are children of one `filter` kind, not kinds of their own.
- Frame-player store layout is assumed: files named by asset frame
  number, inside the chunk whose frames hold it (to confirm in 0008).
- Which top-level node is the root is left to 0010; `scene_from_config`
  takes it as an argument.
