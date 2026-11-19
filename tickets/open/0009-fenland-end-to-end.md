---
id: "0009"
title: "Fenland on videbimus end to end; the rocker-edit and stop/resume checks"
priority: medium
created: 2026-11-18
epic: E001
depends: ["0008"]
---

## Acceptance criteria

- [ ] A full `fenland build --profile overnight` produces a video that
      matches v4 shot for shot (24 samples, full resolution).
- [ ] Editing the rocker code rebuilds exactly the shots whose code
      depends on the edited file, plus the output segments that show them,
      and the logs say why. vxn-2b's Blender controls import vxn-1c's and
      share its panel relief, so its shots count. Decided with the user,
      2026-11-19.
- [ ] Stopping mid-build, reviewing, then resuming loses at most one chunk
      per running task.
- [ ] The overnight profile keeps memory within budget on the 8 GB
      machine.
