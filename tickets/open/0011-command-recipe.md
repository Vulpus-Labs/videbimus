---
id: "0011"
title: "The command recipe: external programs as assets"
priority: high
created: 2026-11-18
epic: E001
depends: ["0004", "0010"]
---

## Summary

ADR 0009 §3. Blender, ffmpeg, Python scripts and existing binaries as
recipes, configured in KDL, with no Rust.

## Acceptance criteria

- [ ] Arguments with `$in.*`, `$out`, `$chunk.start`, `$chunk.end` and
      `$cores` substituted.
- [ ] Tools with version probes; scripts hashed into the key.
- [ ] Outputs as a file or a frame pattern, with chunking; chunks commit
      atomically (ADR 0008).
- [ ] Declared cost, cores, memory and remote eligibility feed the
      scheduler.
- [ ] Full stdout and stderr in the task log; a non-zero exit fails the
      chunk, with the last lines shown in the progress view.
- [ ] Fenland's Blender shot runs as a command recipe in a test config.
