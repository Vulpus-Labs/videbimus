# videbimus

*Latin: "we shall see."*

A build system for rendered video. A video is a tree of **scene
components**, described in a config file and rendered frame by frame. The
expensive things those components need (Blender renders, exported panel
movies, encoded audio) are **assets** in a content-addressed cache. A
scheduler builds them within a CPU and memory budget, on this machine or
on one other machine.

Goals:

1. **Regenerate only what changed.** Assets are keyed by what they are
   made from, so an edit rebuilds exactly the assets downstream of it.
2. **Run efficiently without overloading the machine.** One scheduler owns
   all heavy work, packs it into a budget and puts the critical path first.
3. **Change freely.** The asset graph, the order of scenes and how they
   are composited are all config, and rebuilding after an edit is cheap.
4. **See what is happening.** Every task logs in detail, and a live
   progress view shows the running tasks inside the scene hierarchy.

Background: videbimus generalises the pipeline that made silkscreen's "Fenland"
video: headless editor export, then Blender close-ups, then a Rust
compositor. Rebuilding that video on videbimus is the first milestone
([E001](epics/open/E001-fenland-on-videbimus.md)).

Using it: a project made only of config runs on the stock `videbimus` binary,
with external programs (Blender, ffmpeg, scripts) as `command` recipes. A
project with its own Rust components or recipes is a small crate whose
`main` registers its kinds with `videbimus::App` and hands over to the same CLI
([ADR 0009](adrs/0009-using-videbimus.md)).

Design: [adrs/](adrs/). Work: [epics/](epics/) and [tickets/](tickets/).
