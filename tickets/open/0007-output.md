---
id: "0007"
title: "Output: segmented render and encode, join, audio mux; videbimus review and preview"
priority: medium
created: 2026-11-18
epic: E001
depends: ["0006"]
---

## Acceptance criteria

- [ ] The composite renders and encodes in keyed segments, with
      checkpoints persisted per segment (ADR 0008 §4).
- [ ] The join is a stream copy; the audio is muxed from an asset.
- [ ] `videbimus review` plays or exports what exists, with placeholders for
      missing frames.
- [ ] `videbimus preview <frames>` renders single frames or ranges on demand.
