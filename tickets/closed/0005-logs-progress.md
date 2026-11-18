---
id: "0005"
title: "Logs and progress: event log, task logs, status file, live tree, videbimus status"
priority: high
created: 2026-11-18
epic: E001
depends: ["0004"]
---

## Acceptance criteria

- [x] A task log per task in the store: why, resolved config, commands
      with full output, chunk timings, peak memory, exit status.
- [x] A JSON-lines event log per run.
- [x] A live tree mirroring the scene hierarchy, with state, progress, ETA
      and machine per node, and the budget in the header.
- [x] A status file rewritten atomically; `videbimus status` prints it.
- [x] Progress counts only committed chunks under current keys.

## Close-out (2026-11-18)

New crate `crates/videbimus-progress`:

- `Recorder` (the scheduler's sink for a run): appends each event as a
  JSON line to `<store>/runs/<UTC time>-<pid>.jsonl`, names it in
  `<store>/runs/latest` (newest 50 kept), folds events into a `Tree`,
  rewrites `<store>/status.json` atomically every second and at the end,
  and shows the run through a `View`: `Live` (redrawn in place with
  ANSI, at most 4 times a second, fitted to the terminal), `Plain` (a
  line per event, for non-terminals) or `Quiet`.
- `Tree` / `Snapshot` / `NodeView`: the scene hierarchy (scene group,
  else the name's directories) with state, frames committed of total,
  elapsed, ETA, machine, why, log; groups aggregate; all-cached groups
  collapse; header budget (cores and memory in use against the profile)
  and overall ETA; failures with last lines and log; blocked dependants
  name their failed input.
- `render(&Snapshot, width, color)`: the live view's frame and
  `status`'s tree.
- `asset_statuses(&Graph, Option<&Store>)` and `status_report`: current
  (with committed frames), stale, missing, or pending input (an input
  that hashes its output isn't built under its current key), computed
  as the scheduler computes keys.
- Progress counts only chunks committed under the current key: the
  scheduler's `begun` count from the store, plus `chunk-committed`
  events; in-flight frames feed ETAs only.
- Tests: `crates/videbimus-progress/tests/progress.rs` (11: states and
  hierarchy, collapse, budget, failure and blocked, ETA falling, stop,
  render pinned at 80 columns with and without colour, failure render,
  status file atomic under a concurrent reader and the run log, pruning,
  plain and live views) and `crates/videbimus/tests/progress.rs` (4:
  `status` current/stale/missing/pending on a temp project with fake
  recipes, a build's run log and status file, resolved config in task
  logs, `status` while a build holds the lock). All pass under `nice`.

Changes outside the new crate:

- `videbimus-scheduler`: `Task::config_text()` (default `None`), logged
  under `config:` when an asset is begun; `Graph::name`, `inputs`,
  `group` and `ids` accessors, for `status` to compute keys.
- `videbimus`: `KindTask` supplies the asset node as resolved KDL text
  for `config_text`; the CLI's `build`/`preview` run through a
  `Recorder` (live tree on a terminal, the old event lines otherwise),
  and `status` prints `status_report` after the scene's reads (the old
  per-name latest-build listing is gone). `tests/app.rs`'s status
  assertion follows the new wording.

Decisions recorded in ADR 0007 ("How it is built"): file locations,
the extra states (not needed, blocked, cancelled), tree placement, ETA
method, `status`'s key states, terminal behaviour.

Open points:

- Run logs are pruned by count, not collected with GC (they aren't
  under a key).
- `status` while building trusts the status file; with no write for 10 s
  it says so, but can't tell a stuck build from a slow one.
- No crossterm (not in the offline registry): plain ANSI and a
  `TIOCGWINSZ` ioctl; Windows gets a fixed width.
