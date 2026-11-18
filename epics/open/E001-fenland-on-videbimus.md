---
id: E001
title: "Rebuild the Fenland video on videbimus"
status: open
created: 2026-11-18
---

## Goal

Produce silkscreen's Fenland video (v4) with videbimus instead of the
hand-run scripts: same shots, same look. The proof is an edit. Changing
the rocker code in the Blender scene script must rebuild exactly the
vxn-1c shots and the composite, report why, stay within the budget, and
survive a stop and resume part-way through.

Decisions: ADRs 0001–0005 and 0007–0009. ADR 0006 (second machine) is E002,
after this epic.

## Tickets

| Ticket | What |
|---|---|
| 0001 | Core types: frames, rates, intervals, tempo map, catch-up planning |
| 0002 | Config: KDL, units, references, arithmetic, templates, includes, located errors |
| 0003 | Asset store: keys, manifests, atomic chunks, why-rebuilt, lock, GC |
| 0004 | Scheduler: profiles and budgets, cost history, critical path, chunking, subprocess control, stop |
| 0005 | Logs and progress: event log, task logs, status file, live tree, `videbimus status` |
| 0006 | Component runtime: sequence, stack, filter, frame player, caption, image; stateful stepping and checkpoints |
| 0007 | Output: segmented render and encode, join, audio mux; `videbimus review`, `videbimus preview` |
| 0010 | `videbimus::App`: kind registry, CLI entry point, per-kind code identity, `requires` |
| 0011 | The `command` recipe: external programs as assets |
| 0008 | The `fenland` project crate: song render, print raster, editor export, Blender shot, fen backdrop |
| 0009 | Fenland on videbimus end to end; the rocker-edit and stop/resume checks |

## Dependency order

0001 first. Then 0002 and 0003 in parallel. Then 0004 (needs 0003), and
0005 (needs 0004). Then 0010 (needs 0002 and 0003), and 0006 (needs 0001,
0002 and 0003). Then 0011 (needs 0004 and 0010), and 0007 (needs 0006).
Then 0008, then 0009.
