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

## How it is built (ticket 0005)

- **Where things are.** Each task's log is `<store>/<key>/log.txt`
  (ADR 0003 §6). It records the task's resolved config as KDL text
  (`Task::config_text`; the project supplies the asset's declaration
  with references and expressions resolved), since the key holds only
  its digest. Each run's events go to
  `<store>/runs/<UTC time>-<pid>.jsonl`, one JSON object per event;
  `<store>/runs/latest` names the newest; the newest 50 are kept. The
  status file is `<store>/status.json`, rewritten (staged, synced,
  renamed) every second and once when the run ends.
- **States.** Besides §2's, a task can be *not needed* (everything that
  reads it is cached), *blocked* (an input failed) or *cancelled* (the
  run stopped first). A group's state is the first of running, queued,
  failed, waiting, blocked, cancelled that any child has; else cached if
  all are cached, else done.
- **The tree's shape.** A task sits under its scene group if it has one,
  else under its name's directories (`s07/blender` under `s07`).
- **ETAs.** A task's remaining work is its frames not yet committed times
  seconds per frame: from this run's committed chunks once there are
  some, else from the plan's estimate (cost history, ADR 0004 §2). A
  running chunk is credited with its reported progress, else with its
  elapsed time (up to 90%). A task's ETA adds the longest ETA of its
  unfinished inputs. The run's ETA is the longer of the longest such
  chain and the remaining core-seconds spread over the core budget.
- **Budget in the header**: cores granted to running chunks and their
  declared memory, against the profile's limits.
- **`videbimus status`** with a build running (its lock held by a live
  process) prints the status file's tree. With none, it prints the last
  run's outcome and failures, then each asset against the current
  config: *current* (with frames committed under its key), *stale* (its
  name's latest build has another key), *missing*, or *pending input*:
  an input hashes its output (ADR 0003 §4) and isn't built under its
  current key, so the asset's key can't be known yet. It never guesses.
- **The terminal.** On a TTY the tree is redrawn in place with ANSI
  escapes at most four times a second (and once a second for the
  clocks), cut to the terminal's width and height. Otherwise a line per
  event is printed, as before.

## Consequences

- "Status?" is answered by `videbimus status`, with accurate ETAs.
- Logs are part of the store and garbage-collected with their assets.
