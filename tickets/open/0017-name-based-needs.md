---
id: "0017"
title: "Replace Component::needs (keys) with name-based asset reads"
priority: low
created: 2026-11-19
epic: E001
depends: []
---

## Summary

`Component::needs` returns asset keys, but keys can't be known before
lazily keyed inputs are built (ADR 0003 §4). 0010 linked components to
assets by name through `Builder::read_asset` instead, and kept `needs` only
for a frame player that reads a fixed key.

## Acceptance criteria

- [ ] `needs` removed from `Component`, or redefined as names; a frame
      player reading by key declares it through the builder.
- [ ] ADR 0002 §1 updated.
