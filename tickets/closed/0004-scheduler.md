---
id: "0004"
title: "Scheduler: profiles and budgets, cost history, critical path, chunking, subprocess control, stop"
priority: high
created: 2026-11-18
epic: E001
depends: ["0003"]
---

## Acceptance criteria

- [x] Profiles (interactive, background, overnight) set core and memory
      budgets and priority; switching profile mid-run takes effect.
- [x] Declared costs, replaced by measured history (contended runs
      ignored).
- [x] Critical-path ordering; previews jump the queue with only their
      needs.
- [x] Subprocesses get niceness and thread caps from the task; stop kills
      them and discards in-flight chunks (ADR 0008 §2).

## Close-out (2026-11-18)

Landed as the new crate `crates/videbimus-scheduler`:

- `Task` (name, shape, resources, cost, key without graph inputs,
  `hash_output`, `chunk_frames`, `run(&TaskContext)`) is the seam ticket
  0010's `RecipeKind` produces. `Graph`/`Node` hold labelled inputs and a
  scene group; the scheduler adds each input's identity to the key, so
  keys of dependants of hashed inputs resolve lazily, after the input is
  built (ADR 0003 §4).
- `TaskContext`: chunk and out dir, granted cores, niceness, cancel token,
  task log, progress, input lookup, and `command()`: own process group,
  niceness, thread-cap variables, full stdout/stderr into the task log,
  SIGTERM then SIGKILL to the group on cancel, peak memory from `wait4`.
- `Scheduler::run(&Graph, &[Target])` with `Control` (stop, set_profile,
  preview); `Profile` (interactive, background, overnight); cost history
  with a contention rule; critical-path order; chunks of about 120 s;
  stop discards in-flight chunks and a second run resumes.
- `Event`/`EventKind` (serialisable, tagged `event`) through `EventSink`,
  for ticket 0005. `Executor` with one implementation, `LocalExecutor`.
- `stop_on_sigint()`: first Ctrl-C stops gracefully, a second kills the
  subprocess groups and the process.
- Tests: `crates/videbimus-scheduler/tests/scheduler.rs` (12), plus one
  store test.

Deviations and additions:

- Store change: `Manifest::external_load_pct` and
  `AssetWriter::note_external_load`, so manifests record the load they
  were built under (ADR 0004 §2 now says how it is measured).
- A frame-range target (`Target::frames`) builds only the chunks covering
  those frames and leaves the asset incomplete (`partial`); ticket 0007's
  `videbimus preview` builds on it.
- Tasks sharing a key build it once; an input whose dependants are all
  cached isn't built. Recorded in ADR 0004 §4.
- Not checked on Windows (the store's `blake3` C build fails to
  cross-compile here); the non-Unix paths are documented fallbacks.

