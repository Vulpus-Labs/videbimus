---
id: "0008"
title: "The fenland project crate: song render, print raster, editor export, Blender shot, fen backdrop"
priority: medium
created: 2026-11-18
epic: E001
depends: ["0007", "0011"]
---

## Summary

Port the scratch pipeline (silkscreen session, 2026-11-18) into a
`fenland` project crate outside videbimus's repository (ADR 0009 §7). It
registers its own kinds through `videbimus::App`, and uses command recipes
where an existing program already does the job.

## Acceptance criteria

- [ ] Rust recipe kinds: song render (stems, mix, `song.json` split per
      track) and headless editor export.
- [ ] Command recipes: print raster (`rsvg-convert`) and Blender shot
      (chunked frames).
- [ ] Rust component kinds: the fen backdrop (stateful, finite horizon)
      and the automation caption.
- [ ] `fenland.kdl` declares `requires` and `binary "fenland"`.
