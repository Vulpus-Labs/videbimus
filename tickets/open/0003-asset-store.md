---
id: "0003"
title: "Asset store: keys, manifests, atomic chunks, why-rebuilt, lock, GC"
priority: high
created: 2026-11-18
epic: E001
depends: ["0001"]
---

## Acceptance criteria

- [ ] Keys hash the recipe kind, config, input keys and tool versions;
      scripts are hashed per entry point (ADR 0003 §2).
- [ ] Chunks commit by atomic rename with a manifest entry; temporaries
      are cleared on start (ADR 0008 §1).
- [ ] Early cutoff on output hashes where a recipe asks for it.
- [ ] Each rebuild records which input key changed.
- [ ] One lock per store; a stale lock is cleared with a warning.
- [ ] GC keeps what the config reaches, plus N generations.
