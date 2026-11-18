# ADR 0005: Configuration: expressive, not a language

- **Status:** Accepted
- **Date:** 2026-11-18

## Context

The scene tree, shot list, asset recipes and render profiles must be easy
to change (README goal 3). The user prefers expressive config to a
real-language DSEL, unless something important breaks.

Fenland's shot list (`shots.py`) needed more than literal values:

- **Repetition with variation.** Fifteen close-ups shared defaults and
  differed in synth, tab, bars, camera and captions.
- **Computed values.** Camera key times were rescaled from the shot's
  length plus a dissolve tail, and frame counts came from bars at the song's
  tempo.
- **Data-driven content.** Captions read the automation in `song.json`,
  and the backdrop followed chords and note onsets.

## Decision

1. **The format is KDL v2.** It has nested nodes for trees, comments,
   properties and children. Its type annotations carry units:
   `(bar)12`, `(beat)2`, `(s)0.5`, `(f)15`.
2. **Expressive features, bounded:**
   - **Units and musical time:** values in bars, beats, seconds or frames,
     resolved against the song's tempo map (ADR 0001 §4).
   - **References:** `$name` refers to a defined value. A component may
     refer to its own resolved fields (`$self.start`, `$self.length`) and
     its parent's.
   - **Arithmetic:** `(expr)"$self.length + 0.5s"`: `+ − × ÷`, min, max and
     clamp over typed quantities, with unit checking. (First written
     `(=)"…"`, which KDL v2 rejects: a type annotation is an identifier,
     and `=` can't appear in one. See §6.)
   - **Templates:** `template "close-up" { … }`, then `shot "s02"
     use="close-up" { … }` overrides fields. Templates nest.
   - **Includes:** `include "shots.kdl"`.
   - **Literal lists:** `each` expands a block over a literal list, for
     example one band per synth.
3. **Not in config:** user-defined functions, general loops, conditionals
   on runtime data, and access to files except through assets. Anything
   that needs these is a **component or recipe written in Rust**, with
   config choosing it and setting its parameters.
4. **Data-driven structure belongs to generator components.** "A cut at
   every chord change", or "a caption for each automated parameter", is a
   component that reads its data asset at initialisation and creates its
   own children. The config names the generator, not the expansion.
5. **Errors point at the source.** Every resolved value remembers its file,
   line and template chain, so an error says where the bad value came from
   and what was expected.

6. **The resolver's rules** (ticket 0002, `videbimus-config`):
   - **Timebase.** One top-level `video rate=30 { tempo bpm=104
     beats-per-bar=4 }`; `rate="30000/1001"` for fractional rates. It is
     resolved first; the tempo is optional if no time is in bars or beats.
     `video` stays in the tree for its other properties.
   - **Times.** A time is an amount of one unit, counted from the start
     (bar 0, 0 s, frame 0). Positions and durations share the type: with
     one constant tempo they convert to frames identically, and nothing
     is rounded before the final conversion, so each boundary still snaps
     once from its own position (ADR 0001 §4). A tempo map would need the
     two told apart.
   - **Unit rules.** time ± time converts the right operand into the
     left's unit, exactly; time × number and time ÷ number keep the unit;
     time ÷ time is a plain number; time ± number, time × time and
     number ÷ time are errors. min, max and clamp take all times or all
     numbers. `frames(t)` is the nearest whole frame to `t`, as `(f)`:
     the one rounding an expression can ask for, for formulas that round
     a length (Fenland's camera timing does).
   - **References** resolve lazily in document order. `$name` is a `let`
     or `each` binding, from the innermost block out. Paths start at
     `$self`, `$parent` or `$prev`, may step through `parent` and `prev`,
     and may enter the children of a resolved node (an earlier sibling,
     by kind or name), never of one still resolving. `$$` escapes a
     literal `$`.
   - **`let name=value`** binds in its block and the blocks inside it.
   - **Templates** are defined at the top level of a file (includes
     count). The using node's properties win; a child with the same kind
     and name as a template child merges into it; other children follow.
     Template values resolve where the template is used.
   - **`include "f.kdl"`** is relative to the including file and splices
     in place. **`each "x" a b c index="i" { … }`** repeats its body per
     literal item.

## What would break it

Fenland needs none of the excluded features. The pressure points, and
their answers:

- **Shots generated from song data:** generator components (§4).
- **Camera curves from a formula:** arithmetic over `$self` (§2). A curve
  family that keeps recurring becomes a camera component in Rust.
- **Cross-shot constraints** (a camera that ends where the next one
  starts): references to siblings' resolved fields (`$prev.camera.end`),
  resolved in order. Cycles are config errors.

If any of these stops being enough, a narrow escape is to let a generator
component call an external script that emits KDL, run as an asset with the
same caching. That keeps the core declarative.

## Consequences

- One config file, plus includes, describes a whole video.
- The resolver (units, references, templates, includes) is a core
  library, independent of rendering.
