# ADR 0003: The asset graph and its cache

- **Status:** Accepted
- **Date:** 2026-11-18

## Context

During Fenland, deciding which shots to re-render was done by hand, twice
for a single fix. File timestamps say nothing about whether a change
mattered. Coarse dependencies ("everything depends on song.json") would
rebuild everything for any mix tweak.

## Decision

1. **An asset is the output of a recipe.** A recipe declares its inputs:
   other assets, source files, config values. It also declares the tools it
   runs (with versions) and its cost (ADR 0004). Recipes are the asset
   counterpart of components: a component's `needs` names assets, and the
   graph is derived from the scene config, never kept separately.
2. **Keys are content hashes.** An asset's key hashes its recipe kind, its
   own config, its input keys and its tool versions. Scripts and binaries
   are hashed per entry point, so a change to one script invalidates only
   the assets that run it.
3. **Dependencies are as narrow as what is read.** A recipe that reads
   one track's automation from a song depends on that track's data, not the
   whole song. Recipes split their inputs to make this possible.
4. **Early cutoff.** Where an output is worth hashing (frame sequences,
   exported movies), a rebuilt asset whose output hash equals its old one
   leaves its dependants cached.
   For that to hold, a dependant's key takes such an input's output hash in
   place of its key, so the dependant's key can only be computed once the
   input is built. Output hashes cover file paths and contents, not how the
   frames were chunked.
5. **Frame sequences are cached per frame.** A partial build resumes from
   the frames present. A recipe may declare that an input change touches
   only a frame range, which then rebuilds alone.
6. **The store is content-addressed on disk.** `store/<key>/` holds the
   output, a manifest (inputs, tool versions, timings, peak memory,
   machine), and the build log (ADR 0007). Each asset also has a name, its
   stable slot in the scene (`s07/blender`); the store keeps each name's
   keys, newest first. The project assigns names: the group path the
   asset is declared in, then its own name, by default its recipe kind
   (ADR 0009 §7). Garbage collection keeps what the current config
   reaches, plus each name's last N generations that it no longer reaches.
7. **Every rebuild says why.** Keys hash labelled inputs, and the
   manifest keeps them; a new key is diffed against its name's previous
   key's inputs. The store records which input key changed,
   and the logs and progress view show it ("s07: hwscene.py changed").

## Consequences

- Editing a caption rebuilds the composite only. Editing `hwscene.py`
  rebuilds every shot that runs it, and nothing else.
- Tool versions in keys make outputs from either machine interchangeable
  (ADR 0006).
