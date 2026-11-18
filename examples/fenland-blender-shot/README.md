# Fenland's Blender shot as a command recipe

Fenland's s14 hardware shot (`hwscene.py`, rendered by Blender) as two
`command` assets ([ADR 0009](../../adrs/0009-using-videbimus.md) §3), with
no Rust:

- `s14/shot` copies the shot's inputs (`panel.mp4`, `controls.json`,
  `camera.json`) into the store. They are hashed as sources, so editing
  one rebuilds the shot.
- `s14/blender` runs
  `PHYS=1 blender -b -t <cores> -P hwscene.py -- <shot> <out> --start S --end E`
  per chunk of 4 frames. Blender's version and `hwscene.py`'s contents are
  in its key; a stopped build resumes from the committed chunks.

## Setup

The script and the shot's inputs aren't in this repository. Copy
`local.kdl.example` to `local.kdl` (ignored by git) and fill it in:

| value | what |
|---|---|
| `blender` | Blender's executable (`/Applications/Blender.app/Contents/MacOS/Blender` on macOS, or `blender` on `PATH`) |
| `scene-script` | `hwscene.py` |
| `panel`, `controls`, `camera` | the shot's `panel.mp4`, `controls.json` and `camera.json` (`out/hw/s14/`) |
| `from`, `to` | the frames to render: `from` up to but not including `to`, 1-based (`(f)1` to `(f)279` is the whole shot) |
| `samples`, `width`, `height` | render settings, passed to `hwscene.py` |

Then:

```sh
videbimus --config examples/fenland-blender-shot/video.kdl build
videbimus --config examples/fenland-blender-shot/video.kdl status
```

The store is `.videbimus/store` here. `hwscene.py` also reads the control
models from a path fixed in the script; that isn't hashed.

## The test

`fenland_blender_shot` in `crates/videbimus/tests/command.rs` is ignored by
default. It writes its own `local.kdl` for 2 frames at 480×270 and 4
samples:

```sh
VIDEBIMUS_FENLAND_SONG=/path/to/song \
VIDEBIMUS_BLENDER=/Applications/Blender.app/Contents/MacOS/Blender \
    cargo test -p videbimus --test command fenland -- --ignored --nocapture
```

`VIDEBIMUS_FENLAND_SONG` is the directory holding `hwscene.py` and
`out/hw/s14/`. Set `VIDEBIMUS_FENLAND_KEEP` to a directory to keep the
frames.
