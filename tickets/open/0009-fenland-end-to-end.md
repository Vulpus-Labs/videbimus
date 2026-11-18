---
id: "0009"
title: "Fenland on videbimus end to end; the rocker-edit and stop/resume checks"
priority: medium
created: 2026-11-18
epic: E001
depends: ["0008"]
---

## Acceptance criteria

- [ ] `videbimus build fenland.kdl` produces a video that matches v4 shot for
      shot.
- [ ] Editing the rocker code rebuilds exactly the vxn-1c shots and the
      composite, and the logs say why.
- [ ] Stopping mid-build, reviewing, then resuming loses at most one chunk
      per running task.
- [ ] The overnight profile keeps memory within budget on the 8 GB
      machine.
