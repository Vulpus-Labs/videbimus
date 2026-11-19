---
id: "0015"
title: "Keys for files components and tools read outside the asset system"
priority: medium
created: 2026-11-19
epic: E001
depends: []
---

## Summary

These files are read but not in any key, so editing them doesn't rebuild
anything:

- an `image` component's file, a `caption`'s font, and a `frame-player`'s
  `dir` (0007 close-out);
- fonts that `rsvg-convert` resolves through fontconfig (fenland's print
  rasters ask for `Jost*`; 0008).

## Acceptance criteria

- [ ] `image`, `caption` and `frame-player dir=` hash what they read into the
      output's segment keys (as `source` does for command recipes).
- [ ] A way for command recipes to declare extra files they read
      implicitly (fonts), with a note on fontconfig.
