---
id: "0014"
title: "Runtime: a stack skips compositing layers that draw nothing"
priority: low
created: 2026-11-19
epic: E001
depends: []
---

## Summary

The root stack clears and composites every layer whose interval holds a
frame, even if the layer's child draws nothing in that frame (for example a
wrapper stack whose only child is outside its interval). Each costs a
full-frame clear and composite.

## Acceptance criteria

- [ ] Containers report whether a frame drew anything (or a layer's child
      interval is checked), and empty layers cost no full-frame pass.
- [ ] Measured on fenland before and after.
