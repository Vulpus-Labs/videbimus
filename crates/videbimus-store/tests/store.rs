//! The asset store against real temporary directories.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use videbimus_core::{AssetKey, Frame, Interval};
use videbimus_store::{
    ChunkId, CommitOptions, Cutoff, Error, KeyBuilder, KeyInputs, LockHolder, Shape, Status, Store,
    Warning, Why, frames, key_hex, machine_name,
};

fn shot(config: &str) -> KeyInputs {
    KeyBuilder::new("blender-shot")
        .code(b"kind code v1")
        .config(config)
        .input("panel", AssetKey([1; 32]))
        .script("hwscene.py", b"import bpy\n")
        .tool("blender", "5.2.0")
        .build()
        .unwrap()
}

fn seq(start: i64, end: i64) -> Shape {
    Shape::Frames(Interval::new(Frame(start), Frame(end)))
}

/// Builds every missing chunk of `inputs` under `name`, `step` frames at a
/// time, each frame's file holding `content`.
fn build(store: &Store, name: &str, inputs: &KeyInputs, shape: Shape, step: i64, content: &str) {
    let asset = store.begin(name, inputs, shape).unwrap();
    for gap in asset.missing().unwrap() {
        let iv = gap.frames().unwrap();
        let mut at = iv.start.0;
        while at < iv.end.0 {
            let end = (at + step).min(iv.end.0);
            let chunk = asset.chunk(frames(at, end)).unwrap();
            for f in at..end {
                fs::write(chunk.out_dir().join(format!("{f:04}.png")), content).unwrap();
            }
            chunk
                .commit(CommitOptions {
                    hash: true,
                    peak_memory_bytes: Some(1000 + u64::try_from(at).unwrap()),
                })
                .unwrap();
            at = end;
        }
    }
    asset.finish(None).unwrap();
}

fn tmp_entries(root: &Path) -> usize {
    fs::read_dir(root.join("tmp")).unwrap().count()
}

// ---------------------------------------------------------------- keys

#[test]
fn keys_are_stable_and_ignore_insertion_order() {
    let a = KeyBuilder::new("k")
        .config(b"c")
        .input("a", AssetKey([1; 32]))
        .input("b", AssetKey([2; 32]))
        .tool("ffmpeg", "7.1")
        .tool("blender", "5.2.0")
        .build()
        .unwrap();
    let b = KeyBuilder::new("k")
        .tool("blender", "5.2.0")
        .input("b", AssetKey([2; 32]))
        .tool("ffmpeg", "7.1")
        .input("a", AssetKey([1; 32]))
        .config(b"c")
        .build()
        .unwrap();
    assert_eq!(a.key(), b.key());
    assert_eq!(a.records(), b.records());
    assert_eq!(shot("x").key(), shot("x").key());
}

#[test]
fn the_key_scheme_is_pinned() {
    // Changing how keys hash invalidates every store: do it on purpose.
    let k = KeyBuilder::new("k").config(b"c").build().unwrap().key();
    let again = KeyBuilder::new("k").config(b"c").build().unwrap().key();
    assert_eq!(k, again);
    assert_eq!(key_hex(&k).len(), 64);
    assert_eq!(key_hex(&k), PINNED_KEY);
}

const PINNED_KEY: &str = "cd923b04e1bbd5165c1ed5473a17717cc38d87630d9a82b594732ff19489fd24";

#[test]
fn every_input_class_changes_the_key() {
    let base = || {
        KeyBuilder::new("k")
            .code(b"code")
            .config(b"config")
            .input("in", AssetKey([1; 32]))
            .source("song/track3", b"automation")
            .script("hwscene.py", b"script")
            .tool("blender", "5.2.0")
    };
    let k0 = base().build().unwrap().key();
    let variants = [
        KeyBuilder::new("k2")
            .code(b"code")
            .config(b"config")
            .input("in", AssetKey([1; 32]))
            .source("song/track3", b"automation")
            .script("hwscene.py", b"script")
            .tool("blender", "5.2.0"),
        base().code(b"code2"),
        base().config(b"config2"),
        base().input("in2", AssetKey([1; 32])),
        base().source("song/track4", b"automation"),
        base().script("other.py", b"script"),
        base().tool("ffmpeg", "7.1"),
    ];
    let mut seen = HashSet::from([k0]);
    for v in variants {
        assert!(seen.insert(v.build().unwrap().key()));
    }
    // Changed content under the same labels.
    let changed = [
        KeyBuilder::new("k")
            .code(b"code")
            .config(b"config")
            .input("in", AssetKey([2; 32]))
            .source("song/track3", b"automation")
            .script("hwscene.py", b"script")
            .tool("blender", "5.2.0"),
        KeyBuilder::new("k")
            .code(b"code")
            .config(b"config")
            .input("in", AssetKey([1; 32]))
            .source("song/track3", b"automation2")
            .script("hwscene.py", b"script")
            .tool("blender", "5.2.0"),
        KeyBuilder::new("k")
            .code(b"code")
            .config(b"config")
            .input("in", AssetKey([1; 32]))
            .source("song/track3", b"automation")
            .script("hwscene.py", b"script2")
            .tool("blender", "5.2.0"),
        KeyBuilder::new("k")
            .code(b"code")
            .config(b"config")
            .input("in", AssetKey([1; 32]))
            .source("song/track3", b"automation")
            .script("hwscene.py", b"script")
            .tool("blender", "5.2.1"),
    ];
    for v in changed {
        assert!(seen.insert(v.build().unwrap().key()));
    }
}

#[test]
fn labels_and_classes_are_part_of_the_key() {
    // Swapping keys between roles changes the key.
    let ab = KeyBuilder::new("join")
        .input("segment/000", AssetKey([1; 32]))
        .input("segment/001", AssetKey([2; 32]))
        .build()
        .unwrap();
    let ba = KeyBuilder::new("join")
        .input("segment/000", AssetKey([2; 32]))
        .input("segment/001", AssetKey([1; 32]))
        .build()
        .unwrap();
    assert_ne!(ab.key(), ba.key());
    // The same bytes as a script or as a source differ.
    let script = KeyBuilder::new("k").script("a", b"x").build().unwrap();
    let source = KeyBuilder::new("k").source("a", b"x").build().unwrap();
    assert_ne!(script.key(), source.key());
    // Label boundaries can't be shifted into content.
    let t1 = KeyBuilder::new("k").tool("ab", "c").build().unwrap();
    let t2 = KeyBuilder::new("k").tool("a", "bc").build().unwrap();
    assert_ne!(t1.key(), t2.key());
}

#[test]
fn singletons_replace_and_labels_must_be_unique() {
    let once = KeyBuilder::new("k").config(b"b").build().unwrap();
    let twice = KeyBuilder::new("k")
        .config(b"a")
        .config(b"b")
        .build()
        .unwrap();
    assert_eq!(once.key(), twice.key());
    assert!(matches!(
        KeyBuilder::new("k").script("a.py", b"1").script("a.py", b"2").build(),
        Err(Error::DuplicateInput { label, .. }) if label == "a.py"
    ));
    assert!(matches!(
        KeyBuilder::new("k").tool("", "1").build(),
        Err(Error::EmptyLabel { .. })
    ));
}

#[test]
fn script_files_hash_their_contents_under_their_label() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hwscene.py");
    fs::write(&path, b"import bpy\n").unwrap();
    let from_file = KeyBuilder::new("k")
        .script_file("hwscene.py", &path)
        .build()
        .unwrap();
    let from_bytes = KeyBuilder::new("k")
        .script("hwscene.py", b"import bpy\n")
        .build()
        .unwrap();
    assert_eq!(from_file.key(), from_bytes.key());
    let missing = KeyBuilder::new("k")
        .script_file("gone.py", dir.path().join("gone.py"))
        .build();
    let err = missing.unwrap_err();
    assert!(matches!(err, Error::InputFile { .. }));
    assert!(err.to_string().contains("gone.py"), "{err}");
}

// ---------------------------------------------------------------- why

#[test]
fn changes_read_as_reasons() {
    let old = KeyBuilder::new("blender-shot")
        .input("s07/panel", AssetKey([1; 32]))
        .script("hwscene.py", b"v1")
        .script("rig.py", b"v1")
        .tool("blender", "5.2.0")
        .build()
        .unwrap();
    let new = KeyBuilder::new("blender-shot")
        .input("s07/panel", AssetKey([2; 32]))
        .script("hwscene.py", b"v2")
        .script("lights.py", b"v1")
        .tool("blender", "5.2.1")
        .config(b"{}")
        .build()
        .unwrap();
    let reasons: Vec<String> = new
        .changes_since(old.records())
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        reasons,
        [
            "config added",
            "input s07/panel changed",
            "script hwscene.py changed",
            "script lights.py added",
            "script rig.py removed",
            "tool blender 5.2.0 → 5.2.1",
        ]
    );
    let kind = KeyBuilder::new("fen").build().unwrap();
    let reasons = kind.changes_since(old.records());
    assert_eq!(reasons[0].to_string(), "kind blender-shot → fen");
    assert!(new.changes_since(new.records()).is_empty());
}

#[test]
fn the_store_says_why_against_the_names_last_key() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = Store::open(dir.path()).unwrap();
    let v1 = shot("{\"samples\":64}");
    assert_eq!(store.why("s07/blender", &v1).unwrap(), Why::New);
    build(&store, "s07/blender", &v1, seq(0, 4), 4, "a");

    let v2 = KeyBuilder::new("blender-shot")
        .code(b"kind code v1")
        .config("{\"samples\":64}")
        .input("panel", AssetKey([1; 32]))
        .script("hwscene.py", b"import bpy  # rocker\n")
        .tool("blender", "5.2.0")
        .build()
        .unwrap();
    let why = store.why("s07/blender", &v2).unwrap();
    assert_eq!(why.to_string(), "script hwscene.py changed");
    assert!(matches!(why, Why::Changed { previous, .. } if previous == v1.key()));

    // Beginning records the reason in the manifest, and moves the name.
    let asset = store.begin("s07/blender", &v2, seq(0, 4)).unwrap();
    assert_eq!(asset.why(), &why);
    drop(asset);
    assert_eq!(store.latest("s07/blender").unwrap(), Some(v2.key()));
    assert_eq!(
        store.generations("s07/blender").unwrap(),
        [v2.key(), v1.key()]
    );
    let m = store.manifest(v2.key()).unwrap().unwrap();
    assert_eq!(m.why, why);
    assert_eq!(m.name, "s07/blender");
    assert_eq!(m.kind, "blender-shot");
    assert_eq!(m.tools[0].name, "blender");
    assert_eq!(m.tools[0].version, "5.2.0");
    assert_eq!(m.inputs, v2.records());
    assert_eq!(m.status, Status::Building);

    // Resuming keeps the original reason.
    assert_eq!(store.why("s07/blender", &v2).unwrap(), Why::Resume);
    let asset = store.begin("s07/blender", &v2, seq(0, 4)).unwrap();
    assert_eq!(asset.why(), &why);
    drop(asset);

    // Once the old key is collected, the reason can't be given.
    store.gc(&HashSet::from([v2.key()]), 0).unwrap();
    let v3 = shot("{\"samples\":128}");
    assert!(matches!(
        store.why("s07/blender", &v3).unwrap(),
        Why::Changed { .. }
    ));
    fs::remove_dir_all(store.key_dir(v2.key())).unwrap();
    assert!(matches!(
        store.why("s07/blender", &v3).unwrap(),
        Why::PreviousGone { .. }
    ));
}

#[test]
fn manifests_round_trip_as_json() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = Store::open(dir.path()).unwrap();
    let v1 = shot("1");
    build(&store, "s", &v1, seq(0, 2), 1, "a");
    let v2 = shot("2");
    let asset = store.begin("s", &v2, Shape::Single).unwrap();
    asset.fail("blender exited with status 1").unwrap();
    for key in [v1.key(), v2.key()] {
        let path = store.key_dir(key).join("manifest.json");
        let text = fs::read_to_string(path).unwrap();
        let m: videbimus_store::Manifest = serde_json::from_str(&text).unwrap();
        assert_eq!(Some(m), store.manifest(key).unwrap());
    }
    let m = store.manifest(v2.key()).unwrap().unwrap();
    assert_eq!(
        m.status,
        Status::Failed {
            message: "blender exited with status 1".into()
        }
    );
    assert_eq!(m.shape, Shape::Single);
}

// ---------------------------------------------------------------- chunks

#[test]
fn chunks_are_invisible_until_committed() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = Store::open(dir.path()).unwrap();
    let inputs = shot("x");
    let asset = store.begin("s", &inputs, seq(0, 10)).unwrap();
    let chunk = asset.chunk(frames(0, 5)).unwrap();
    fs::write(chunk.out_dir().join("0000.png"), "x").unwrap();
    assert!(chunk.out_dir().starts_with(dir.path().join("tmp")));
    assert!(asset.committed().unwrap().is_empty());
    let rec = chunk.commit(CommitOptions::default()).unwrap();
    assert_eq!(rec.id(), frames(0, 5));
    assert_eq!(rec.machine, machine_name());
    assert_eq!(asset.committed().unwrap(), [rec]);
    let out = store.chunk_dir(inputs.key(), frames(0, 5));
    assert_eq!(fs::read_to_string(out.join("0000.png")).unwrap(), "x");

    // Dropped uncommitted: gone, temporaries and all.
    let chunk = asset.chunk(frames(5, 10)).unwrap();
    fs::write(chunk.out_dir().join("0005.png"), "x").unwrap();
    drop(chunk);
    assert_eq!(tmp_entries(dir.path()), 0);
    assert_eq!(asset.committed().unwrap().len(), 1);
}

#[test]
fn a_crash_leaves_only_whole_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let inputs = shot("x");
    {
        let (store, warnings) = Store::open(dir.path()).unwrap();
        assert!(warnings.is_empty());
        let asset = store.begin("s", &inputs, seq(0, 10)).unwrap();
        let done = asset.chunk(frames(0, 5)).unwrap();
        fs::write(done.out_dir().join("0000.png"), "x").unwrap();
        done.commit(CommitOptions::default()).unwrap();
        // Killed mid-chunk: its temporary isn't cleaned up.
        let half = asset.chunk(frames(5, 10)).unwrap();
        fs::write(half.out_dir().join("0005.png"), "half").unwrap();
        std::mem::forget(half);
        // And a half-written manifest replacement.
        fs::write(dir.path().join("tmp/manifest.1.2"), "{").unwrap();
        assert_eq!(tmp_entries(dir.path()), 2);
        // Killed: nothing is dropped, so the lock stays behind too.
        std::mem::forget(asset);
        std::mem::forget(store);
    }
    // The process is gone: its lock is stale.
    write_lock(
        dir.path(),
        &LockHolder {
            pid: dead_pid(),
            host: machine_name(),
            started_ms: 0,
        },
    );

    let (store, warnings) = Store::open(dir.path()).unwrap();
    assert!(matches!(warnings[0], Warning::StaleLockCleared { .. }));
    assert_eq!(warnings[1], Warning::TemporariesRemoved { count: 2 });
    assert_eq!(tmp_entries(dir.path()), 0);
    let p = store.progress(inputs.key()).unwrap().unwrap();
    assert_eq!(p.status, Status::Building);
    assert_eq!((p.done_frames, p.total_frames), (5, 10));
    assert_eq!(p.missing, [frames(5, 10)]);

    // Resuming builds only the missing frames.
    let asset = store.begin("s", &inputs, seq(0, 10)).unwrap();
    assert_eq!(asset.why(), &Why::New);
    assert_eq!(asset.missing().unwrap(), [frames(5, 10)]);
    let c = asset.chunk(frames(5, 10)).unwrap();
    c.commit(CommitOptions::default()).unwrap();
    let m = asset.finish(Some(42)).unwrap();
    assert_eq!(m.status, Status::Complete);
    assert_eq!(m.peak_memory_bytes, Some(42));
    assert!(m.output_hash.is_none());
}

#[test]
fn committed_chunks_answer_progress_and_resume() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = Store::open(dir.path()).unwrap();
    let inputs = shot("x");
    assert!(store.progress(inputs.key()).unwrap().is_none());
    let asset = store.begin("s", &inputs, seq(100, 200)).unwrap();
    for (a, b) in [(150, 160), (100, 120), (190, 200)] {
        asset
            .chunk(frames(a, b))
            .unwrap()
            .commit(CommitOptions::default())
            .unwrap();
    }
    // Stray files in the key directory are never progress.
    fs::write(store.key_dir(inputs.key()).join("chunks/0170.png"), "x").unwrap();
    fs::create_dir(store.key_dir(inputs.key()).join("chunks/f160_190")).unwrap();
    let p = store.progress(inputs.key()).unwrap().unwrap();
    assert_eq!(
        p.committed
            .iter()
            .map(videbimus_store::ChunkRecord::id)
            .collect::<Vec<_>>(),
        [frames(100, 120), frames(150, 160), frames(190, 200)]
    );
    assert_eq!((p.done_frames, p.total_frames), (40, 100));
    assert_eq!(p.missing, [frames(120, 150), frames(160, 190)]);
    fs::remove_dir(store.key_dir(inputs.key()).join("chunks/f160_190")).unwrap();

    assert!(matches!(
        asset.chunk(frames(110, 130)),
        Err(Error::ChunkOverlaps { committed, .. }) if committed == frames(100, 120)
    ));
    assert!(matches!(
        asset.chunk(frames(195, 205)),
        Err(Error::ChunkOutsideShape { .. })
    ));
    assert!(matches!(
        asset.chunk(ChunkId::Whole),
        Err(Error::ChunkOutsideShape { .. })
    ));
    let asset = match asset.finish(None) {
        Err(Error::Incomplete { missing, .. }) => {
            assert_eq!(missing, [frames(120, 150), frames(160, 190)]);
            store.begin("s", &inputs, seq(100, 200)).unwrap()
        }
        other => panic!("{other:?}"),
    };
    assert!(matches!(
        store.begin("s", &inputs, seq(100, 200)),
        Err(Error::AlreadyWriting { .. })
    ));
    for gap in asset.missing().unwrap() {
        asset
            .chunk(gap)
            .unwrap()
            .commit(CommitOptions::default())
            .unwrap();
    }
    asset.finish(None).unwrap();
    assert!(matches!(
        store.begin("s", &inputs, seq(100, 200)),
        Err(Error::AlreadyComplete { .. })
    ));
    let p = store.progress(inputs.key()).unwrap().unwrap();
    assert_eq!((p.done_frames, p.total_frames), (100, 100));
    assert_eq!(p.status, Status::Complete);
}

#[test]
fn a_single_output_is_one_whole_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = Store::open(dir.path()).unwrap();
    let inputs = shot("movie");
    let asset = store
        .begin("editor/export", &inputs, Shape::Single)
        .unwrap();
    assert_eq!(asset.missing().unwrap(), [ChunkId::Whole]);
    assert!(matches!(
        asset.chunk(frames(0, 1)),
        Err(Error::ChunkOutsideShape { .. })
    ));
    let c = asset.chunk(ChunkId::Whole).unwrap();
    fs::write(c.out_dir().join("export.mov"), "movie").unwrap();
    c.commit(CommitOptions::default()).unwrap();
    assert!(asset.missing().unwrap().is_empty());
    asset.finish(None).unwrap();
    let out = store.chunk_dir(inputs.key(), ChunkId::Whole);
    assert!(out.join("export.mov").is_file());
    assert!(matches!(
        store.begin("editor/export", &inputs, seq(0, 1)),
        Err(Error::AlreadyComplete { .. })
    ));
}

#[test]
fn external_load_keeps_the_highest_noted_across_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = Store::open(dir.path()).unwrap();
    let inputs = shot("load");
    let mut asset = store.begin("s07/blender", &inputs, seq(0, 2)).unwrap();
    asset.note_external_load(30);
    asset.note_external_load(10);
    let c = asset.chunk(frames(0, 1)).unwrap();
    c.commit(CommitOptions::default()).unwrap();
    asset.fail("stopped").unwrap();
    let m = store.manifest(inputs.key()).unwrap().unwrap();
    assert_eq!(m.external_load_pct, Some(30));

    let mut asset = store.begin("s07/blender", &inputs, seq(0, 2)).unwrap();
    asset.note_external_load(20);
    let c = asset.chunk(frames(1, 2)).unwrap();
    c.commit(CommitOptions::default()).unwrap();
    let m = asset.finish(None).unwrap();
    assert_eq!(m.external_load_pct, Some(30));
}

#[test]
fn a_key_keeps_its_shape() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = Store::open(dir.path()).unwrap();
    let inputs = shot("x");
    drop(store.begin("s", &inputs, seq(0, 10)).unwrap());
    assert!(matches!(
        store.begin("s", &inputs, seq(0, 20)),
        Err(Error::ShapeMismatch { .. })
    ));
}

// ---------------------------------------------------------------- early cutoff

#[test]
fn early_cutoff_compares_with_the_last_complete_build() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = Store::open(dir.path()).unwrap();
    let v1 = shot("1");
    build(&store, "s", &v1, seq(0, 12), 4, "frame");
    assert_eq!(
        store.early_cutoff("s", v1.key()).unwrap(),
        Cutoff::NoPrevious
    );

    // A different config, the same pixels, chunked differently.
    let v2 = shot("2");
    build(&store, "s", &v2, seq(0, 12), 5, "frame");
    assert_eq!(
        store.early_cutoff("s", v2.key()).unwrap(),
        Cutoff::Same { previous: v1.key() }
    );
    let id1 = store.identity_for_dependants(v1.key()).unwrap().unwrap();
    let id2 = store.identity_for_dependants(v2.key()).unwrap().unwrap();
    assert_eq!(id1, id2);
    assert_ne!(id1, v1.key());

    // Different pixels.
    let v3 = shot("3");
    build(&store, "s", &v3, seq(0, 12), 4, "other");
    assert_eq!(
        store.early_cutoff("s", v3.key()).unwrap(),
        Cutoff::Different { previous: v2.key() }
    );
    assert_ne!(store.identity_for_dependants(v3.key()).unwrap(), Some(id1));

    // An unfinished build in between is skipped; an unhashed one isn't
    // comparable.
    let v4 = shot("4");
    drop(store.begin("s", &v4, seq(0, 12)).unwrap());
    let v5 = shot("5");
    let a = store.begin("s", &v5, Shape::Single).unwrap();
    a.chunk(ChunkId::Whole)
        .unwrap()
        .commit(CommitOptions::default())
        .unwrap();
    a.finish(None).unwrap();
    assert_eq!(
        store.early_cutoff("s", v5.key()).unwrap(),
        Cutoff::NotHashed
    );
    assert_eq!(
        store.identity_for_dependants(v5.key()).unwrap(),
        Some(v5.key())
    );
    assert_eq!(store.identity_for_dependants(v4.key()).unwrap(), None);
    let v6 = shot("6");
    build(&store, "s", &v6, seq(0, 12), 6, "other");
    assert_eq!(
        store.early_cutoff("s", v6.key()).unwrap(),
        Cutoff::NotHashed
    );
}

// ---------------------------------------------------------------- lock

fn write_lock(root: &Path, holder: &LockHolder) {
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("lock"), serde_json::to_vec(holder).unwrap()).unwrap();
}

fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

#[test]
fn a_live_lock_is_an_error_naming_its_holder() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = Store::open(dir.path()).unwrap();
    let err = Store::open(dir.path()).unwrap_err();
    match &err {
        Error::Locked {
            holder, other_host, ..
        } => {
            assert_eq!(holder.pid, std::process::id());
            assert_eq!(holder.host, machine_name());
            assert!(!other_host);
        }
        other => panic!("{other:?}"),
    }
    let msg = err.to_string();
    assert!(
        msg.contains(&format!("pid {}", std::process::id())),
        "{msg}"
    );

    // Readers don't need the lock, and can't write.
    let reader = Store::open_read_only(dir.path()).unwrap();
    assert!(matches!(
        reader.begin("s", &shot("x"), Shape::Single),
        Err(Error::ReadOnly)
    ));
    assert!(matches!(
        reader.gc(&HashSet::new(), 0),
        Err(Error::ReadOnly)
    ));

    // Released on drop.
    drop(store);
    assert!(!dir.path().join("lock").exists());
    Store::open(dir.path()).unwrap();
}

#[test]
fn a_stale_lock_is_cleared_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let holder = LockHolder {
        pid: dead_pid(),
        host: machine_name(),
        started_ms: 1_795_000_000_000,
    };
    write_lock(dir.path(), &holder);
    let (_store, warnings) = Store::open(dir.path()).unwrap();
    assert_eq!(warnings, [Warning::StaleLockCleared { holder }]);
    assert!(warnings[0].to_string().contains("stale"));
    let now: LockHolder =
        serde_json::from_slice(&fs::read(dir.path().join("lock")).unwrap()).unwrap();
    assert_eq!(now.pid, std::process::id());
}

#[test]
fn a_lock_from_another_host_is_not_cleared() {
    let dir = tempfile::tempdir().unwrap();
    write_lock(
        dir.path(),
        &LockHolder {
            pid: dead_pid(),
            host: "render-box".into(),
            started_ms: 0,
        },
    );
    let err = Store::open(dir.path()).unwrap_err();
    assert!(matches!(
        err,
        Error::Locked {
            other_host: true,
            ..
        }
    ));
    assert!(err.to_string().contains("render-box"), "{err}");
}

// ---------------------------------------------------------------- GC

#[test]
fn gc_keeps_what_is_reachable_plus_n_generations() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = Store::open(dir.path()).unwrap();
    let shots: Vec<KeyInputs> = (0..5).map(|i| shot(&i.to_string())).collect();
    for s in &shots {
        build(&store, "s07/blender", s, seq(0, 4), 2, "x");
    }
    let other = shot("other");
    build(&store, "s08/blender", &other, seq(0, 4), 4, "x");
    // A name the config no longer has keeps its generations too.
    let orphan = KeyBuilder::new("orphan").build().unwrap();
    build(&store, "gone", &orphan, seq(0, 1), 1, "x");
    // A key no name remembers.
    let unnamed = KeyBuilder::new("unnamed").build().unwrap();
    build(&store, "unnamed", &unnamed, seq(0, 1), 1, "x");
    fs::remove_file(dir.path().join("names/unnamed.json")).unwrap();
    let writing = shot("writing");
    let writer = store.begin("s09/blender", &writing, seq(0, 4)).unwrap();
    // Not a key: left alone.
    fs::write(dir.path().join("README"), "hello").unwrap();

    let current = shots[4].key();
    let reachable = HashSet::from([current, other.key()]);
    let report = store.gc(&reachable, 2).unwrap();

    let removed: HashSet<AssetKey> = report.removed.iter().map(|(k, _)| *k).collect();
    assert_eq!(
        removed,
        HashSet::from([shots[0].key(), shots[1].key(), unnamed.key()])
    );
    assert!(report.freed_bytes > 0);
    assert_eq!(
        report.freed_bytes,
        report.removed.iter().map(|(_, b)| b).sum::<u64>()
    );
    assert_eq!(report.kept, 6);
    for k in [
        current,
        shots[3].key(),
        shots[2].key(),
        other.key(),
        writing.key(),
    ] {
        assert!(store.key_dir(k).is_dir(), "{k} was deleted");
    }
    for k in &removed {
        assert!(!store.key_dir(*k).exists());
    }
    assert!(dir.path().join("README").exists());
    assert_eq!(tmp_entries(dir.path()), 0);
    assert_eq!(
        store.generations("s07/blender").unwrap(),
        [current, shots[3].key(), shots[2].key()]
    );
    assert_eq!(store.generations("gone").unwrap(), [orphan.key()]);
    drop(writer);

    // Keep nothing but what's reachable.
    let report = store.gc(&reachable, 0).unwrap();
    let removed: HashSet<AssetKey> = report.removed.iter().map(|(k, _)| *k).collect();
    assert_eq!(
        removed,
        HashSet::from([shots[3].key(), shots[2].key(), writing.key(), orphan.key()])
    );
    assert_eq!(store.generations("s07/blender").unwrap(), [current]);
    assert!(store.generations("gone").unwrap().is_empty());
    assert!(!dir.path().join("names/gone.json").exists());
}

#[test]
fn names_escape_into_file_names() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = Store::open(dir.path()).unwrap();
    store.record("s07/blender", AssetKey([3; 32])).unwrap();
    store.record("../evil", AssetKey([4; 32])).unwrap();
    assert!(dir.path().join("names/s07%2Fblender.json").is_file());
    assert!(dir.path().join("names/%2E.%2Fevil.json").is_file());
    assert_eq!(
        store.latest("s07/blender").unwrap(),
        Some(AssetKey([3; 32]))
    );
    assert!(matches!(
        store.record("", AssetKey([3; 32])),
        Err(Error::EmptyName)
    ));
}
