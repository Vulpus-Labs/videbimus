---
id: "0003"
title: "Asset store: keys, manifests, atomic chunks, why-rebuilt, lock, GC"
priority: high
created: 2026-11-18
epic: E001
depends: ["0001"]
---

## Acceptance criteria

- [x] Keys hash the recipe kind, config, input keys and tool versions;
      scripts are hashed per entry point (ADR 0003 §2).
- [x] Chunks commit by atomic rename with a manifest entry; temporaries
      are cleared on start (ADR 0008 §1).
- [x] Early cutoff on output hashes where a recipe asks for it.
- [x] Each rebuild records which input key changed.
- [x] One lock per store; a stale lock is cleared with a warning.
- [x] GC keeps what the config reaches, plus N generations.

## Close-out (2026-11-18)

Landed as the `videbimus-store` crate (`crates/videbimus-store`).

- **Keys** (`KeyBuilder`, `KeyInputs`): labelled inputs of seven classes
  (kind, code identity, canonical config bytes, input asset keys, source
  data, scripts per entry point, tools with versions), sorted by class and
  label before hashing, so insertion order doesn't matter and labels do.
  blake3, domain-separated per class. Takes canonical bytes; no
  dependency on the config crate. The scheme is pinned by a test.
- **Layout**: `<store>/<key>/{manifest.json, log.txt, chunks/<chunk>/{chunk.json, out/}}`,
  plus `lock`, `tmp/` and `names/`; documented in the crate docs.
- **Chunks** (`AssetWriter`, `ChunkWriter`): written under `tmp/` with
  their `chunk.json` beside the output, synced, committed by one directory
  rename. `Store::open` deletes `tmp/`'s contents and says so. Progress and
  resume read only committed chunks (`committed`, `progress`, `missing`).
- **Why rebuilt**: each asset has a name (its slot, e.g. `s07/blender`);
  `names/<name>.json` lists its keys, newest first. A new key's inputs are
  diffed against the previous key's manifest: `script hwscene.py changed`,
  `input s07/panel changed`, `tool blender 5.2.0 → 5.2.1`. The reason is
  stored in the manifest and kept across resumes.
- **Early cutoff**: chunks optionally hash their files; a complete asset's
  output hash is independent of chunking. `early_cutoff` compares with the
  name's previous complete build; `identity_for_dependants` gives the
  output hash (or the key, if unhashed) for dependants' keys.
- **Lock**: `lock` holds pid, host and start; hard-linked into place. A
  dead pid on this host is cleared with a `Warning`; a live one, or one on
  another host, is an error naming the holder. `Store::open_read_only`
  reads without the lock.
- **GC**: keeps reachable keys, keys being written, and each name's N
  newest unreachable generations; deletes the rest via `tmp/` and reports
  bytes freed.

Deviations and open points:

- ADR 0003 §4 as written couldn't work: dependants key on their inputs'
  keys, which change on any rebuild. The ADR now says dependants key on an
  input's output hash where it has one. That means a dependant's key is
  known only after the input is built; the scheduler (0004) has to live
  with that.
- ADR 0003 §6/§7 gained the asset name, needed to find "the previous
  build" in a content-addressed store. Who assigns names (the scene path
  plus the recipe's role, presumably) is for 0004/0010.
- ADR 0003 §5 "cached per frame": the store caches per chunk. Carrying
  committed frames over from a previous key when a recipe declares that a
  change touches only a frame range is not done; it belongs with the
  recipe declarations (0004/0010).
- A frame sequence's range must be in the key's config: a key begun with
  another shape is an error.
- Windows: lock liveness isn't checked and directory renames fail on open
  files; noted in the crate docs, not solved.

