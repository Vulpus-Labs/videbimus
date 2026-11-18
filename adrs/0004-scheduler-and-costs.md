# ADR 0004: The scheduler, costs and budgets

- **Status:** Accepted
- **Date:** 2026-11-18

## Context

Fenland's overnight renders were rate-limited by hand: `nice`, Blender
`-t 4`, 60-frame batches with sleeps. On an 8 GB machine, memory was the
real limit: two EEVEE renders at once would have swapped. Builds running
alongside skewed timings (one shot took 33 minutes instead of 9 while Rust
compiled).

## Decision

1. **One scheduler owns all heavy work.** Asset builds and frame rendering
   are tasks in one queue. No component or recipe starts work outside it.
2. **Recipes declare costs.** For each task: estimated seconds (per frame
   for frame sequences), cores, peak memory, and whether it wants the GPU.
   The store records measured cost with the load at the time. Once there
   is history, it replaces the estimate, ignoring runs made under
   contention.
3. **Budgets are profiles.** `interactive`, `background` and `overnight`
   each set a core budget, a memory budget and a priority. The scheduler
   packs tasks so that the sum of declared cores and memory stays within
   budget. Subprocesses inherit priority and thread caps from the task
   (Blender `-t`, `nice`). The user can switch profile while a build runs.
4. **The critical path goes first.** Tasks are ordered by the longest
   estimated chain to the final output. A preview request (a frame or a
   short range) jumps the queue with only the tasks it needs.
5. **Work is chunked to fit.** Frame sequences split into chunks (by
   default about 2 minutes' work each), so budgets apply at a fine grain,
   cancelling loses little, and partial builds resume (ADR 0003 §5).

## Consequences

- "Don't melt the laptop" is a profile choice, not script edits.
- Cost history gives the progress view real ETAs (ADR 0007).
