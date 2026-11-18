---
id: "0004"
title: "Scheduler: profiles and budgets, cost history, critical path, chunking, subprocess control, stop"
priority: high
created: 2026-11-18
epic: E001
depends: ["0003"]
---

## Acceptance criteria

- [ ] Profiles (interactive, background, overnight) set core and memory
      budgets and priority; switching profile mid-run takes effect.
- [ ] Declared costs, replaced by measured history (contended runs
      ignored).
- [ ] Critical-path ordering; previews jump the queue with only their
      needs.
- [ ] Subprocesses get niceness and thread caps from the task; stop kills
      them and discards in-flight chunks (ADR 0008 §2).
