---
id: "0007"
title: "Output: segmented render and encode, join, audio mux; videbimus review and preview"
priority: medium
created: 2026-11-18
epic: E001
depends: ["0006"]
---

## Acceptance criteria

- [x] The composite renders and encodes in keyed segments, with
      checkpoints persisted per segment (ADR 0008 §4).
- [x] The join is a stream copy; the audio is muxed from an asset.
- [x] `videbimus review` plays or exports what exists, with placeholders for
      missing frames.
- [x] `videbimus preview <frames>` renders single frames or ranges on demand.

## Close-out (2026-11-18)

New crate `crates/videbimus-output`:

- `spec.rs`: the top-level `output` node (`output "film.mp4"
  segment=(s)4 crf=18 preset="medium" { audio asset="mix" }`, or
  `audio "mix.wav"`; `sample-rate`, `bitrate`, `file`), with located
  errors (odd size, unknown preset, a second output or audio).
- `output.rs`: `Output::plan` adds a task per segment
  (`output/segment-00000`, …) and the join (`output/join`) to the asset
  graph; `export` copies the joined file to the output's path; `review`
  assembles committed segments and `NOT RENDERED <frame>` placeholders,
  building nothing.
- `segment.rs`: a segment renders through a fresh `Scene` and pipes
  `rgb24` to ffmpeg (x264, 4:2:0, BT.709) through a named pipe, via
  `TaskContext::command`. Its key: the scene's canonical config, the
  encoding and its frames; `kind/<kind>` sources with each scene kind's
  code identity; `asset/<name>` inputs for the assets it shows; ffmpeg's
  version.
- `join.rs`: concat demuxer with `-c copy`; the audio muxed as AAC,
  resampled and padded or cut to the frames' exact sample count.
- `state.rs`: `state.bin` per segment, keyed by scene config and kinds'
  code; the next segment loads it, or catches up.
- `encode.rs`: ffmpeg commands, its version, the pipe feeding.
- Tests: 9 integration (keys follow code identity and only the reads a
  segment shows; stop then resume rebuilds only missing segments;
  joined frame count and duration, audio asset padded and audio file
  cut within a frame, at 30 and 30000/1001; review placeholders and no
  building; persisted state equals sequential rendering, at the scene
  level and through the encoder; located config errors), 2 unit. ffmpeg
  tests skip with a message when ffmpeg or ffprobe is missing.

Changes outside the new crate:

- `videbimus-core`: `Component::save_state`/`load_state`, declined by
  default.
- `videbimus-runtime`: `Child::save_state`/`load_state`,
  `Scene::save_state`/`load_state`, `save_children`/`load_children`,
  implemented by `sequence`, `stack` and `filter`; `AssetReads::kinds`
  records the kinds a reading builder builds.
- `videbimus`: `output` is reserved at the top level; `Project::output`
  plans it into the graph; `build` with no names targets the output too
  and exports it after a complete run; `review [--out FILE] [--open]`;
  one CLI test.

Decisions recorded in ADR 0008 §7, ADR 0002 §8 and ADR 0009 §7.

Open points:

- A segment's config is the whole scene's: editing any component
  re-renders every segment. Narrowing to the subtree a segment shows
  (and the stateful components before it) is open.
- Files components read that aren't assets (an `image`'s PNG, a
  caption's font, a frame player's `dir`) aren't in any key.
- A saved state isn't checked against asset content; components whose
  state depends on what they read must not save it.
- Parallel segments don't wait for their predecessor's state, so they
  catch up from frame 0 (an unbounded-horizon component costs O(n²)
  steps over a long video built in parallel).
- Encoding uses named pipes: Unix only.
- `preview --video` (encoding a preview range) isn't done.

