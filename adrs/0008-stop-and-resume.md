# ADR 0008: Stopping, reviewing and resuming

- **Status:** Accepted
- **Date:** 2026-11-18

## Context

The user may stop a build early, look at partial output, then start again
roughly where they left off. Any element that was part-complete is
discarded and restarted. In Fenland this was done by hand, with batch
loops that skipped existing frames.

## Decision

1. **Work commits in chunks, atomically.** Each chunk (ADR 0004 §5) is
   written to a temporary location and renamed into the store only when
   complete, along with its manifest entry. A stopped or killed build
   leaves only whole chunks. In-flight temporaries are deleted on the next
   start.
2. **Stopping is always safe.** A graceful stop (Ctrl-C, or `videbimus stop`)
   cancels running tasks and kills their subprocesses. A hard kill or power
   loss is equivalent, because nothing is committed half-written. Pausing
   is a stop.
3. **Resuming is just building again.** Keys are unchanged, so committed
   chunks are cache hits and only missing chunks are scheduled. Part-
   complete chunks are discarded and restarted. If config changed in
   between, chunks whose keys changed rebuild and the rest still hit.
4. **The final render resumes too.** The composite is rendered and encoded
   in segments, each a chunk with its own key, then joined by a cheap
   concatenation step. Stateful components' checkpoints (ADR 0002 §4)
   are persisted with their segments, so a resumed segment starts from a
   checkpoint instead of from frame 0.
5. **Partial output is reviewable.**
   - `videbimus review` plays, or exports, the video from whatever segments and
     frames exist, with missing frames shown as a labelled placeholder.
   - `videbimus preview <frames>` renders single frames or short ranges on
     demand.
   - Any asset's committed chunks can be opened from the store.
6. **One scheduler per store.** A lock file stops two builds writing the
   same store. A stale lock (its process gone) is cleared with a warning.

7. **How the output landed** (ticket 0007, crate `videbimus-output`):
   - **Config.** A top-level `output` names the file and how it is cut
     and encoded; its `audio` child names a declared asset or a file:

     ```kdl
     output "film.mp4" segment=(s)4 crf=18 preset="medium" {
         audio asset="mix"        // or: audio "mix.wav"
     }
     ```

     `segment` (default 4 s) is the segment length; boundaries are on
     frame numbers from 0 to the root's end. Encoding is x264, 4:2:0,
     BT.709, and the size must be even.
   - **Segments are assets.** Each segment is a task of its own
     (`output/segment-00000`, …), a single output holding `segment.mp4`,
     `segment.json` and `state.bin`, so each has its own key: the
     scene's canonical config (`video` and the root), the encoding and
     its frames; a `kind/<kind>` source per component kind the scene
     builds, with that kind's code identity; an `asset/<name>` input per
     declared asset the scene shows during its frames; and ffmpeg's
     version. Editing a kind's source re-renders every segment; rebuilding
     an asset re-renders only the segments that show it. Editing the
     scene's config re-renders every segment (narrowing to the subtree a
     segment shows is open).
   - **Encoding.** A segment renders its frames through a fresh scene
     and pipes them, as `rgb24` (premultiplied linear over black,
     sRGB-encoded), through a named pipe to ffmpeg run by the task's
     subprocess helper, so stop, niceness and logging apply. Each
     segment is a separate encode, so it starts with an IDR frame and
     no frame refers across a boundary: the join is the concat demuxer
     with `-c copy`. Named pipes make this Unix-only for now.
   - **State across segments** (§4). A segment saves the scene's state
     after its last frame (ADR 0002 §8). The next segment loads it if
     the previous segment's latest build has one for the same scene
     config and kinds' code and the right frame, and otherwise catches
     its stateful components up as usual (from frame 0, or within their
     horizon). Frames are the same either way. Segments that run in
     parallel don't wait for each other's state, so the saving shows on
     resume and in serial builds. The state key doesn't cover asset
     content: components whose state depends on what they read must not
     save it.
   - **The join** (`output/join`) reads the segments, labelled in order,
     and the audio asset (an audio file is a source of its key). It
     muxes the audio as AAC at `sample-rate` (48 kHz by default),
     resampled, then padded with silence or cut to exactly
     `floor(frames · rate · den / num)` samples (ADR 0001 §3), sample 0
     with frame 0. `build` copies the result to the output's path.
   - **Review** (§5): `videbimus review [--out FILE] [--open]` takes each
     segment's latest committed build, if it is of the plan's frames
     and encoding, encodes a `NOT RENDERED <frame>` placeholder for each
     frame of the others, joins them with whatever audio exists, and
     writes `<output>.review.<ext>` beside the output. It opens the
     store read-only and builds nothing, so it runs beside a build. It
     shows what was rendered, which after an edit may be the previous
     render of a segment not yet rebuilt.
   - `videbimus preview <frames>` is unchanged: PNG frames. Encoding a
     preview range to video is not done.

## Consequences

- Stopping overnight, reviewing over breakfast and resuming loses at most
  one chunk per running task.
- Segment boundaries need keyframes in the output encoding. Joining is a
  stream copy, not a re-encode.
