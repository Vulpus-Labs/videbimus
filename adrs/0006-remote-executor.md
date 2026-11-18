# ADR 0006: A second machine

- **Status:** Proposed (after E001)
- **Date:** 2026-11-18

## Context

The user has two computers, and might want to send work to the second. A
large grid is out of scope. Measured on Fenland, one Blender shot needs
about 1 MB of inputs (`panel.mp4`, `controls.json`, `camera.json`), plus
scripts and art that rarely change. It produces 577 MB of PNG frames from
12–30 minutes of rendering.

## Decision

1. **Executors are a short list in config.** `local`, plus any number of
   ssh hosts, each with a work directory, cores, memory, budget profile
   and tool versions.
2. **Whole tasks travel, not frames.** Each recipe declares whether it may
   run remotely. Blender renders, headless exports and encodes can travel.
   Final compositing stays where the frames are.
3. **Inputs move by key.** Before dispatch, the scheduler sends only the
   input keys the remote's store lacks (rsync over ssh). Scripts and art
   are sent once and stay.
4. **Outputs are packed for the trip.** Frame sequences come back
   losslessly compressed (FFV1 or compressed EXR) and are unpacked into the
   local store, or played from the archive.
5. **Tool versions are in keys** (ADR 0003 §2). A mismatch is a cache miss
   and a scheduling constraint, not a visual difference.
6. **Placement is by estimated finish time,** counting transfer time.

## Consequences

- No shared filesystem and no queue service: ssh, rsync and the same
  binary on both machines.
