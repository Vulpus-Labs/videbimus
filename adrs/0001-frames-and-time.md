# ADR 0001: Frames and time

- **Status:** Accepted
- **Date:** 2026-11-18

## Context

The Fenland compositor called components with a time `t` in seconds and
derived frame indices from it in several places, each with its own
rounding. The exporter computed `round(bar_s(b0) · fps)`, and the
compositor computed `round((t − bar_s(b0)) · fps) + 1`. Interval
boundaries, cache lookups and dissolves all compared floats.

## Decision

1. **Calls take a frame number.** Every call into a component passes an
   integer `Frame` relative to the component's start. A parent passes
   `n − child.start`. Intervals are half-open frame ranges `[start, end)`.
2. **The frame rate is a fraction, set in config.** `Rate { num, den }`:
   30/1, 24/1, 30000/1001. Time is derived exactly as
   `t = n · den / num`, and only inside a component, to compute
   time-varying values (automation, colour curves, motion). Whether a
   component is active, and which frame it shows, are decided in integers.
3. **Audio positions are rational.** The sample for frame `n` is
   `floor(n · sample_rate · den / num)`, computed in integers. At 48 kHz
   and 30 fps that is exactly 1600 · n.
4. **Musical time snaps once, from absolute positions.** Config may give
   times in bars and beats against the song's tempo map. Each boundary is
   converted to a frame from its absolute musical position, rounding to
   the nearest frame. Durations are never summed after rounding, so error
   does not accumulate. Events inside a component keep exact time.
5. **Subframes, if ever needed, are an integer subdivision.** Motion blur
   or supersampling would address `(n, k of K)`, never a fractional `n`.

## Consequences

- Cache keys, interval tests and crossfade overlaps compare exactly.
- A stateful component's `step` advances exactly one frame, so its
  per-frame constants (decay, drift) are defined per frame (ADR 0002).
- Changing the frame rate changes every frame-sequence asset's key.
