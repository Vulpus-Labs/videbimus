---
id: "0006"
title: "Component runtime: sequence, stack, filter, frame player, caption, image; stateful stepping and checkpoints"
priority: high
created: 2026-11-18
epic: E001
depends: ["0001", "0002", "0003"]
---

## Acceptance criteria

- [ ] Components built from resolved config; parents own their children.
- [ ] Random access through `plan_catch_up`, with checkpoints every K
      frames; hidden components are not ticked.
- [ ] Sequence, stack (z-order, opacity curve, blend), filter, frame
      player, caption and image kinds.
- [ ] A crossfade is a stack of two overlapping children.
