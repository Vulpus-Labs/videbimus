---
id: "0008"
title: "The fenland project crate: song render, print raster, editor export, Blender shot, fen backdrop"
priority: medium
created: 2026-11-18
epic: E001
depends: ["0007", "0011"]
---

## Summary

Port the scratch pipeline (silkscreen session, 2026-11-18) into a
`fenland` project crate outside videbimus's repository (ADR 0009 §7). It
registers its own kinds through `videbimus::App`, and uses command recipes
where an existing program already does the job.

## Acceptance criteria

- [x] Rust recipe kinds: song render (stems, mix, `song.json` split per
      track) and headless editor export.
- [x] Command recipes: print raster (`rsvg-convert`) and Blender shot
      (chunked frames).
- [x] Rust component kinds: the fen backdrop (stateful, finite horizon)
      and the automation caption.
- [x] `fenland.kdl` declares `requires` and `binary "fenland"`.

## Close-out (2026-11-19)

Done in a separate local repository, `~/src/fenland` (commit `778dfe9`),
as ADR 0009 §7 intends. It isn't published, because it depends on the
user's silkscreen synths by path.

- **Recipe kinds:**
  - `song-render`: in-process, thread per core. Engines are created in
    track order, so renders repeat exactly.
  - `song-track`: per-track stems and data, output-hashed for early cutoff.
  - `panel-export`: the headless editor export. The s14 export is
    bit-identical to the scratch pipeline's.
  - `shot-camera`, and `files`: content-keyed file sets, for per-synth art
    and SVGs.
- **Command recipes:**
  - print rasters (`python3`, `rsvg-convert`);
  - Blender shots (chunked frames, with the art directory and
    `hwscene.py` keyed).
- **Component kinds:**
  - `fen`: the backdrop, with horizon 245 frames, matching its trail
    decay;
  - `caption-automation`;
  - `lettering`.
- **Built-ins:** everything else uses them: dissolves, triptych bands, the
  grid, title, grain, vignette and fades.
- **`fenland.kdl`** declares the whole video: 24 Blender shots, the scene,
  and `output` with the mix as its audio.
- **Checks:**
  - `build song`, one panel export, 2 frames of the s14 Blender shot at low
    settings, and preview frames of a fen section and the title.
  - Compared by eye and by numbers with v4 at the same timestamps: the
    fen's mean difference is 1.8 levels, and the title is
    indistinguishable.
  - 20 tests in fenland.

Videbimus problems found, filed as follow-ups:

- **0012:** a bare `--config` name broke command recipes; fixed.
- **0013:** a template's lets aren't visible to its own properties.
- **0014:** stacks composite empty layers.
- **0015:** fonts and images read outside the asset system aren't keyed.

Open for 0009:

- **The rocker-edit criterion.** vxn-2b's Blender controls import vxn-1c's,
  and both use vxn-1c's panel relief. Editing the rocker code therefore
  also rebuilds the vxn-2b shots. Either the criterion changes, or the
  shared code is split in silkscreen.
- **Kritter's noise seed** comes from a process-wide instance counter, so a
  re-render repeats exactly only if `song-render` creates the first Kritter
  engines in its process. A seed API in silkscreen would make this robust.
- **Fen loading.** Each fen instance loads two 76 MB stems per segment
  process. Per-frame envelopes from `song-track` would be lighter.
