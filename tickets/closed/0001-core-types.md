---
id: "0001"
title: "Core types: frames, rates, intervals, tempo map, catch-up planning"
priority: high
created: 2026-11-18
epic: E001
depends: []
---

## Summary

The integer time base (ADR 0001) and the random-access planner for
stateful components (ADR 0002 §3), in `videbimus-core`, with no dependencies.

## Acceptance criteria

- [x] `Rate` (a fraction): seconds of a frame, the audio sample of a frame
      (exact in integers), and the frame of a time.
- [x] `Frame`, `Interval` (half-open): contains, shift, overlap.
- [x] `Tempo`: bars and beats to the nearest frame, from absolute
      positions; no drift over hundreds of bars.
- [x] `Horizon` and `plan_catch_up`: given position, target, horizon and
      checkpoints, the cheapest start (continue, checkpoint or reset) and
      the frames to step.
- [x] The component contract as a trait (ADR 0002 §1), object-safe.

## Close-out (2026-11-18)

`videbimus-core`, with no dependencies:

- `time`: `Frame`, `Rate` (exact sample positions in `i128`), `Interval`
  and `Tempo`.
- `catch_up`: `Horizon`, `Origin`, `Plan` and `plan_catch_up`.
- `component`: the `Component` trait, `Context` (with nesting through
  `child`), `Image` (premultiplied linear `f32` RGBA), `AssetKey` and
  `Checkpoint`.

Tests cover:

- exact samples at 24 fps / 44.1 kHz and at NTSC rates;
- no musical-time drift over 500 bars at Fenland's 104 BPM;
- every catch-up origin, and its tie-breaking;
- random access through `Box<dyn Component>` matching sequential
  stepping.
