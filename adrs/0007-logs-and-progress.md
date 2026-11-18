# ADR 0007: Logs and live progress

- **Status:** Accepted
- **Date:** 2026-11-18

## Context

During Fenland, the user's most frequent question was "status?". The
answer came from tailing logs, counting PNGs per shot directory and
estimating by hand. That is error-prone: stale frames from a previous run
looked like progress.

## Decision

1. **Every task logs, in detail.** Each task has a log in the store
   (ADR 0003 §6) with:
   - why it is running (which input key changed);
   - its resolved config;
   - the commands it ran, with their full stdout and stderr;
   - per-chunk timings;
   - peak memory;
   - its exit status.

   The scheduler writes a structured event log (JSON lines) for the whole
   run: task queued, started, progress, finished, failed, cached, why.
2. **Progress mirrors the hierarchy.** The live view is a tree: the scene
   tree, with each component's asset tasks under it. Each node shows:
   - its state: waiting, queued, running, done, cached or failed;
   - its progress: frames or chunks done out of the total;
   - its elapsed time and ETA, from cost history (ADR 0004 §2);
   - its machine.

   Parents aggregate their children. Cached subtrees collapse to one line.
   Header: budget in use against the limit, and the overall ETA.
3. **Three ways to see it:**
   - a live terminal view (`videbimus build` in the foreground);
   - a status file rewritten atomically every second or two (JSON, the
     same tree), for other tools and agents;
   - `videbimus status`, which prints a snapshot from the status file, for
     builds running in the background.
4. **Progress comes only from the store.** Frame counts come from chunks
   committed under the current key, never from files lying in a folder, so
   leftovers from an earlier run never read as progress.
5. **Failures carry context.** A failed task shows its last lines of
   output in the view and its log path, and its dependants show as blocked
   by it.

## Consequences

- "Status?" is answered by `videbimus status`, with accurate ETAs.
- Logs are part of the store and garbage-collected with their assets.
