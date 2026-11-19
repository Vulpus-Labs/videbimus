---
id: "0013"
title: "Config: a template's own properties can use the lets it binds"
priority: medium
created: 2026-11-19
epic: E001
depends: []
---

## Summary

Found by fenland (0008). A template's properties resolve in the scope
outside the node, so a template can't bind `let`s and then use them in its
own `from`/`to`. fenland works around this by wrapping every shot layer in a
`stack` that spans the whole video, which costs about 21 extra full-frame
composites per frame (see 0014).

## Acceptance criteria

- [ ] Properties of a node created with `use=` can refer to `let`s bound
      by its template, or earlier in the same node.
- [ ] Order and cycle rules documented in ADR 0005 §6; cycles are still
      located errors.
- [ ] fenland's shot layers drop their wrapper stacks.
