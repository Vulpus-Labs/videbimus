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

## Consequences

- Stopping overnight, reviewing over breakfast and resuming loses at most
  one chunk per running task.
- Segment boundaries need keyframes in the output encoding. Joining is a
  stream copy, not a re-encode.
