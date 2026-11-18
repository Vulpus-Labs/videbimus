# ADR 0002: Scene components

- **Status:** Accepted
- **Date:** 2026-11-18

## Context

The video is a composition of parts: full-frame shots, layered captions,
crossfades, a title card over everything, and post-processing filters.
Some parts are pure in time. A Blender frame player's frame `n` depends
only on `n`. Others are stateful: Fenland's procedural backdrop kept a
trail buffer built up over the frames before. Rendering it at an arbitrary
frame needed warm-up, and it had to keep advancing while hidden, so it
didn't jump when it came back.

## Decision

1. **A component renders its frame `n` into an image.** It receives a
   context holding global config, its resolved assets, and render context
   (absolute start frame, rate, output size). Its contract:

   ```text
   interval            [start, end) in frames, in its parent's frame
   horizon             0 | H | ∞ frames of history its state depends on
   needs(ctx)       →  asset keys it requires (ADR 0003)
   reset(ctx)          state := state₀
   step(n, ctx)        state(n − 1) → state(n)
   render(n, ctx, out) draws state(n) into out
   checkpoint() / restore(cp)   optional state snapshots
   ```

2. **Horizon declares statefulness.** `0` means pure: any frame in any
   order, so frames can render in parallel. `H` means frame `n` depends
   only on the last H frames: catching up starts from fresh state at
   `n − H`. `∞` means exact: catching up starts from the nearest
   checkpoint, or from frame 0.
3. **The framework handles random access.** It keeps each stateful
   component's current position and checkpoints. To render frame `n` it
   continues, restores a checkpoint, or resets, whichever is closest, and
   steps forward in a loop, never by recursion. Components never see
   requests out of order. A hidden component is not ticked: when it comes
   back, catch-up covers the gap, bounded by its horizon.
4. **Frame caches belong to the framework.** Rendered images and state
   checkpoints are cached by frame number under the component's key
   (ADR 0003). Images are cached in a small in-memory window and on disk
   when asked. State checkpoints are kept every K frames.
5. **Composition is by component kinds.**
   - **sequence:** children in order. It dispatches each frame to the child
     whose interval holds it.
   - **stack:** children layered by z-order, each with an interval,
     opacity curve and blend mode. A crossfade is a stack of two children
     whose intervals overlap.
   - **filter:** wraps one child and post-processes its image (grain,
     vignette, fade).
   - **leaf kinds:** frame player (plays an image-sequence asset), caption,
     image, and procedural kinds written in Rust.
6. **Parents own their children.** A parent creates its children from
   config, initialises them, and disposes of them. Initialising does no
   heavy work: it reports needs, and the scheduler builds them.
7. **Randomness is keyed, not running.** Random values are hashes of
   (seed, frame, element), so catch-up reproduces exactly what sequential
   rendering would have.

8. **The runtime's rules** (ticket 0006, `videbimus-runtime`):
   - **Placement.** Times in config count from the start of the video, as
     everywhere (ADR 0005 §6), so each boundary snaps from its own
     position. A component's interval is stored in its parent's frames:
     the builder subtracts the parent's absolute start. `from` defaults
     to the start of the span the parent offers (a sequence offers the
     previous child's end), the end to `to` or `from + length`, else the
     parent's end. A child may not start before its parent.
   - **Driving.** Each child is held as a `Child`, which owns the
     component, its position and its checkpoints (every K local frames,
     60 by default). Pure components are reset once and never stepped.
     A stack does not render a layer whose opacity is 0, so it is not
     ticked.
   - **Containers are pure to their parents.** A sequence, stack or
     filter renders any of its frames on request, because its children
     catch themselves up, so it reports horizon 0. Rendering one tree's
     frames in parallel needs one tree per thread.
   - **Ramps.** A ramp in over L frames is 0 at the first frame and 1 at
     frame L; a ramp out mirrors it, 0 at the last frame. Linear or
     smoothstep. A crossfade is a stack whose second child fades in over
     the overlap (normal blend), an even mix at the overlap's middle for
     opaque children.
   - **Filters** are one `filter` kind holding effect nodes (`fade`,
     `vignette`, `grain`), applied in order, and one component.
   - **Pixels.** Images are premultiplied linear `f32`; PNG input is
     sRGB, decoded and premultiplied on load. A frame player shows a
     labelled placeholder for a frame it can't find or read.

## Consequences

- Fenland's backdrop becomes a component with a finite horizon (its trail
  decays). A Blender shot becomes a frame player with horizon 0.
- Pure components parallelise across frames; stateful ones parallelise
  only across components.
