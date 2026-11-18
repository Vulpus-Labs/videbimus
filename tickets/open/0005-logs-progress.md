---
id: "0005"
title: "Logs and progress: event log, task logs, status file, live tree, videbimus status"
priority: high
created: 2026-11-18
epic: E001
depends: ["0004"]
---

## Acceptance criteria

- [ ] A task log per task in the store: why, resolved config, commands
      with full output, chunk timings, peak memory, exit status.
- [ ] A JSON-lines event log per run.
- [ ] A live tree mirroring the scene hierarchy, with state, progress, ETA
      and machine per node, and the budget in the header.
- [ ] A status file rewritten atomically; `videbimus status` prints it.
- [ ] Progress counts only committed chunks under current keys.
