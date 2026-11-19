//! The `command` recipe (ADR 0009 §3), with small shell commands.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use videbimus::core::AssetKey;
use videbimus::runtime::color::{read_png, write_png};
use videbimus::scheduler::{
    Collect, EventKind, Outcome, Profile, Scheduler, SchedulerOptions, TaskState,
};
use videbimus::store::Store;
use videbimus::{App, Console, Image};

/// A temporary project directory with `video.kdl`.
struct Dir {
    dir: tempfile::TempDir,
}

impl Dir {
    fn new(config: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("video.kdl"), config).unwrap();
        Self { dir }
    }
    fn config(&self) -> PathBuf {
        self.dir.path().join("video.kdl")
    }
    fn path(&self) -> &Path {
        self.dir.path()
    }
    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.path().join(name), text).unwrap();
    }
    fn run(&self, args: &[&str]) -> (ExitCode, Console) {
        let console = Console::memory();
        let mut all = vec!["--config".to_owned(), self.config().display().to_string()];
        all.extend(args.iter().map(|&s| s.to_owned()));
        let code = App::new("videbimus").run_from(all, &console);
        (code, console)
    }
    fn build(&self) -> Console {
        let (code, console) = self.run(&["build"]);
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "{}{}",
            console.out(),
            console.err()
        );
        console
    }
    fn error(&self) -> String {
        let e = App::new("videbimus")
            .load(&self.config(), None)
            .unwrap_err()
            .to_string();
        e.replace(&self.config().display().to_string(), "video.kdl")
    }
    /// The key of asset `name` as the config now stands.
    fn key(&self, name: &str) -> AssetKey {
        let app = App::new("videbimus");
        let p = app.load(&self.config(), None).unwrap();
        let id = p.asset(name).unwrap().node;
        p.graph().task(id).key().build().unwrap().key()
    }
    fn store(&self) -> Store {
        Store::open_read_only(self.path().join(".videbimus/store")).unwrap()
    }
    /// The committed chunk directories of `name`'s latest build.
    fn chunks(&self, name: &str) -> Vec<PathBuf> {
        let store = self.store();
        let key = store.latest(name).unwrap().unwrap();
        store
            .committed(key)
            .unwrap()
            .iter()
            .map(|c| store.chunk_dir(key, c.id()))
            .collect()
    }
}

#[cfg(unix)]
fn executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// ---- substitution

#[test]
fn substitutes_arguments_env_and_cwd() {
    let d = Dir::new(
        r#####"
video rate=30
asset "src" recipe="command" {
    run "sh" "-c" "printf hello > \"$1/greeting.txt\"" "sh" "$out"
    output file="greeting.txt"
}
asset "args" recipe="command" {
    input "src" "src"
    env WHO="chunk $chunk.start" PLAIN=7
    cwd "$out"
    run "sh" "-c" "for f in $(seq $1 $2); do { printf '%s\\n' \"$@\"; echo \"WHO=$WHO PLAIN=$PLAIN\"; pwd -P; cat \"$3\"; echo; } > $(printf %04d $f).txt; done" "sh" "$chunk.start" "$chunk.last" "$in.src/greeting.txt" "$chunk.end" "$chunk.count" "$cores" "$$out" "a$$b" "$$HOME" 12
    output frames="####.txt" from=(f)3 to=(f)6 chunk=2
}
"#####,
    );
    d.build();
    let chunks = d.chunks("args");
    assert_eq!(chunks.len(), 2, "frames 3–4 and 5");
    let first = std::fs::read_to_string(chunks[0].join("0003.txt")).unwrap();
    let lines: Vec<&str> = first.lines().collect();
    let src = d.chunks("src")[0].join("greeting.txt");
    assert_eq!(
        lines[..10],
        [
            "3",
            "4",
            src.to_str().unwrap(),
            "5",
            "2",
            "1",
            "$out",
            "a$b",
            "$HOME",
            "12"
        ]
    );
    assert_eq!(lines[10], "WHO=chunk 3 PLAIN=7");
    // cwd "$out" is the chunk's working directory, since committed
    assert!(lines[11].contains(".videbimus"), "{}", lines[11]);
    assert_eq!(lines[12], "hello");
    let second = std::fs::read_to_string(chunks[1].join("0005.txt")).unwrap();
    assert!(second.starts_with("5\n5\n"), "{second}");
    assert!(second.contains("WHO=chunk 5"), "{second}");
}

#[test]
fn substitution_errors_are_located() {
    let d = Dir::new(
        r#"
video rate=30
asset "a" recipe="command" {
    run "sh" "-c" "true" "$in.shto"
    output file="x"
}
"#,
    );
    let e = d.error();
    assert!(e.contains("there is no input `shto`"), "{e}");
    assert!(e.contains("video.kdl:4:"), "{e}");
    let d = Dir::new(
        r#"
asset "a" recipe="command" {
    run "sh" "-c" "true" "$chunk.start"
    output file="x"
}
"#,
    );
    let e = d.error();
    assert!(e.contains("needs a frame output"), "{e}");
    let d = Dir::new(
        r#"
asset "a" recipe="command" {
    run "true"
}
"#,
    );
    assert!(
        d.error().contains("a command needs an `output`"),
        "{}",
        d.error()
    );
    let d = Dir::new(
        r#"
asset "a" recipe="command" {
    run "true"
    outptu file="x"
}
"#,
    );
    assert!(
        d.error().contains("did you mean `output`?"),
        "{}",
        d.error()
    );
}

// ---- tools and scripts in the key

#[cfg(unix)]
#[test]
fn tool_versions_are_probed_into_the_key() {
    let d = Dir::new(
        r#"
asset "a" recipe="command" {
    tool "fake" path="bin/fake" version-from="fake --version" version-match="fake ([0-9.]+)"
    run "fake" "$out"
    output file="out.txt"
}
"#,
    );
    std::fs::create_dir(d.path().join("bin")).unwrap();
    let probe = d.path().join("bin/fake");
    let script = |v: &str| {
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo \"fake {v} (built today)\"; else echo {v} > \"$1/out.txt\"; fi\n"
        )
    };
    executable(&probe, &script("1.2.3"));
    let k1 = d.key("a");
    executable(&probe, &script("1.2.4"));
    assert_eq!(d.key("a"), k1, "probed once per process");
    videbimus::command::forget_tool_versions();
    let k2 = d.key("a");
    assert_ne!(k2, k1, "a new version is a new key");

    // the tool's path isn't keyed: the same version elsewhere is the same key
    std::fs::create_dir(d.path().join("other")).unwrap();
    executable(&d.path().join("other/fake"), &script("1.2.4"));
    let moved = std::fs::read_to_string(d.config())
        .unwrap()
        .replace("bin/fake", "other/fake");
    d.write("video.kdl", &moved);
    assert_eq!(d.key("a"), k2);

    // it runs the tool's path, and the manifest records the version
    d.build();
    let out = std::fs::read_to_string(d.chunks("a")[0].join("out.txt")).unwrap();
    assert_eq!(out.trim(), "1.2.4");
    let m = d.store().manifest(k2).unwrap().unwrap();
    let json = serde_json::to_string(&m).unwrap();
    assert!(json.contains("1.2.4"), "{json}");
}

#[test]
fn a_missing_tool_names_the_tool_and_the_probe() {
    let d = Dir::new(
        r#"
asset "a" recipe="command" {
    tool "videbimus-no-such-tool" version-from="videbimus-no-such-tool --version"
    run "videbimus-no-such-tool"
    output file="x"
}
"#,
    );
    let e = d.error();
    assert!(
        e.contains("tool `videbimus-no-such-tool` wasn't found: its version probe `videbimus-no-such-tool --version` couldn't run"),
        "{e}"
    );
    assert!(e.contains("video.kdl:3:"), "{e}");
    assert!(
        e.contains("path=\"/path/to/videbimus-no-such-tool\""),
        "{e}"
    );

    let d = Dir::new(
        r#"
asset "a" recipe="command" {
    tool "sh" version-from="sh -c 'echo nope; exit 2'"
    run "sh"
    output file="x"
}
"#,
    );
    let e = d.error();
    assert!(e.contains("tool `sh`: its version probe"), "{e}");
    assert!(e.contains("failed"), "{e}");
    assert!(e.contains("| nope"), "{e}");

    let d = Dir::new(
        r#"
asset "a" recipe="command" {
    tool "sh" version-from="sh -c 'echo 1'" version-match="v([0-9]+)"
    run "sh"
    output file="x"
}
"#,
    );
    assert!(
        d.error().contains("printed nothing matching `v([0-9]+)`"),
        "{}",
        d.error()
    );
}

#[test]
fn scripts_are_hashed_into_the_key_and_scheduling_is_not() {
    let config = |cost: &str, chunk: u32| {
        format!(
            r#####"
video rate=30
asset "a" recipe="command" {{
    script "gen.sh" name="gen"
    source "data"
    run "sh" "$script.gen" "$out" "$chunk.start" "$source.data"
    output frames="####.txt" from=(f)0 to=(f)4 chunk={chunk}
    cost {cost}
}}
"#####
        )
    };
    let d = Dir::new(&config("per-frame=1 cores=1", 2));
    d.write("gen.sh", "echo one\n");
    std::fs::create_dir(d.path().join("data")).unwrap();
    d.write("data/x.txt", "x");
    let k1 = d.key("a");
    d.write("gen.sh", "echo two\n");
    let k2 = d.key("a");
    assert_ne!(k1, k2, "an edited script is a new key");
    d.write("data/x.txt", "y");
    let k3 = d.key("a");
    assert_ne!(k2, k3, "an edited source file is a new key");
    d.write(
        "video.kdl",
        &config(
            "per-run=(s)300 cores=4 mem=\"2G\" gpu=#true remote=#true",
            3,
        ),
    );
    assert_eq!(d.key("a"), k3, "cost, resources and chunking aren't keyed");

    d.write(
        "video.kdl",
        &config("per-frame=1", 2).replace("\"gen.sh\"", "\"missing.sh\""),
    );
    let e = d.error();
    assert!(e.contains("can't read script"), "{e}");
    assert!(e.contains("missing.sh"), "{e}");
}

// ---- running

#[test]
fn a_non_zero_exit_fails_the_chunk_with_its_last_lines() {
    let d = Dir::new(
        r#"
asset "a" recipe="command" {
    run "sh" "-c" "echo working; echo 'it broke' >&2; exit 3"
    output file="x"
}
"#,
    );
    let (code, console) = d.run(&["build"]);
    assert_eq!(code, ExitCode::FAILURE);
    let out = console.out() + &console.err();
    assert!(out.contains("a whole: failed:"), "{out}");
    assert!(out.contains("exit status: 3"), "{out}");
    assert!(out.contains("| it broke"), "{out}");
    // the full output is in the task log
    let store = d.store();
    let key = store.latest("a").unwrap().unwrap();
    let log = std::fs::read_to_string(store.log_path(key)).unwrap();
    assert!(log.contains("| working"), "{log}");
    assert!(log.contains("! it broke"), "{log}");
}

#[test]
fn missing_outputs_fail_precisely() {
    let d = Dir::new(
        r#####"
video rate=30
asset "frames" recipe="command" {
    run "sh" "-c" "echo rendering; for f in $(seq $1 $2); do [ $f = 4 ] || [ $f = 6 ] || touch $(printf %04d $f).png; done" "sh" "$chunk.start" "$chunk.last"
    cwd "$out"
    output frames="####.png" from=(f)1 to=(f)8 chunk=10
}
asset "file" recipe="command" {
    run "sh" "-c" "touch \"$1/other.mp4\"" "sh" "$out"
    output file="shot.mp4"
}
"#####,
    );
    let (code, console) = d.run(&["build"]);
    assert_eq!(code, ExitCode::FAILURE);
    let out = console.out() + &console.err();
    assert!(
        out.contains("didn't write 2 of the chunk's 7 frames (1–7): frames 4, 6 (\"####.png\": expected 0004.png)"),
        "{out}"
    );
    assert!(out.contains("| rendering"), "the last lines too: {out}");
    assert!(
        out.contains("didn't write file `shot.mp4` into $out; it wrote other.mp4"),
        "{out}"
    );
}

/// A stop discards the running chunk; building again runs only the
/// chunks that weren't committed.
#[cfg(unix)]
#[test]
fn frame_chunks_resume_after_a_stop() {
    let d = Dir::new(
        r#####"
video rate=30
asset "shot" recipe="command" {
    run "sh" "-c" "echo $1 >> runs.txt; if [ -e stop ] && [ $1 -ge 4 ]; then touch started; sleep 30; fi; for f in $(seq $1 $2); do echo $f > \"$3/$(printf %04d $f).txt\"; done" "sh" "$chunk.start" "$chunk.last" "$out"
    output frames="####.txt" from=(f)0 to=(f)8 chunk=2
    cost per-frame=1
}
"#####,
    );
    d.write("stop", "");
    let app = App::new("videbimus");
    let project = app.load(&d.config(), None).unwrap();
    let (store, _) = Store::open(project.store()).unwrap();
    let options = SchedulerOptions {
        kill_grace: Duration::from_millis(200),
        ..SchedulerOptions::default()
    };
    let events = Arc::new(Collect::new());
    let s = Scheduler::new(store.clone(), Profile::new("test", 1, 1 << 30, 0))
        .with_options(options.clone())
        .with_sink(events.clone());
    let control = s.control();
    let started = d.path().join("started");
    let stopper = std::thread::spawn(move || {
        let until = Instant::now() + Duration::from_secs(20);
        while !started.exists() {
            assert!(Instant::now() < until, "the slow chunk never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        control.stop();
    });
    let t0 = Instant::now();
    let report = s.run(project.graph(), &[]).unwrap();
    stopper.join().unwrap();
    assert!(t0.elapsed() < Duration::from_secs(20), "the stop waited");
    assert_eq!(report.outcome, Outcome::Stopped);
    let shot = report.task("shot").unwrap();
    assert_eq!(shot.state, TaskState::Stopped);
    assert_eq!(shot.chunks_cancelled, 1);
    let runs = |p: &Path| -> Vec<i64> {
        std::fs::read_to_string(p.join("runs.txt"))
            .unwrap()
            .lines()
            .map(|l| l.parse().unwrap())
            .collect()
    };
    let first = runs(d.path());
    assert!(
        first.len() - shot.chunks_committed <= 1,
        "lost more than one chunk: ran {first:?}, committed {}",
        shot.chunks_committed
    );
    let key = shot.key.unwrap();
    let committed: Vec<i64> = store
        .committed(key)
        .unwrap()
        .iter()
        .map(|c| c.id().frames().unwrap().start.0)
        .collect();

    // build again: the same key, only the missing chunks run
    std::fs::remove_file(d.path().join("stop")).unwrap();
    std::fs::remove_file(d.path().join("runs.txt")).unwrap();
    events.clear();
    let s = Scheduler::new(store.clone(), Profile::new("test", 1, 1 << 30, 0))
        .with_options(options)
        .with_sink(events.clone());
    let project = app.load(&d.config(), None).unwrap();
    let report = s.run(project.graph(), &[]).unwrap();
    assert_eq!(report.outcome, Outcome::Complete);
    assert_eq!(report.task("shot").unwrap().key, Some(key));
    let mut second = runs(d.path());
    second.sort_unstable();
    let mut expected: Vec<i64> = [0, 2, 4, 6]
        .into_iter()
        .filter(|s| !committed.contains(s))
        .collect();
    expected.sort_unstable();
    assert_eq!(second, expected);
    let progress = events
        .events()
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Progress { .. }))
        .count();
    assert!(progress > 0, "chunks report their frames");
    for c in store.committed(key).unwrap() {
        let dir = store.chunk_dir(key, c.id());
        for f in c.id().frames().unwrap().start.0..c.id().frames().unwrap().end.0 {
            let text = std::fs::read_to_string(dir.join(format!("{f:04}.txt"))).unwrap();
            assert_eq!(text.trim(), f.to_string());
        }
    }
}

// ---- in a scene

#[test]
fn a_frame_player_reads_a_command_asset_by_name() {
    let d = Dir::new(
        r#####"
video rate=30 width=4 height=2
asset "s01/render" recipe="command" {
    source "red.png" name="red"
    run "sh" "-c" "for f in $(seq $1 $2); do cp \"$3\" \"$4/$(printf %04d $f).png\"; done" "sh" "$chunk.start" "$chunk.last" "$source.red" "$out"
    output frames="####.png" from=(f)1 to=(f)7 chunk=3
}
sequence "main" to=(f)6 {
    frame-player asset="s01/render" start-index=1
}
"#####,
    );
    let mut red = Image::new(4, 2);
    red.fill([1.0, 0.0, 0.0, 1.0]);
    write_png(&red, &d.path().join("red.png")).unwrap();

    let console = d.build();
    assert!(console.out().contains("s01/render"), "{}", console.out());
    assert_eq!(d.chunks("s01/render").len(), 2);

    let out = d.path().join("preview");
    let (code, console) = d.run(&["preview", "0..6", "--out", &out.display().to_string()]);
    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "{}{}",
        console.out(),
        console.err()
    );
    for f in 0..6 {
        let img = read_png(&out.join(format!("{f:06}.png"))).unwrap();
        for p in img.pixels() {
            assert!(
                p[0] > 0.99 && p[1] < 0.01 && p[2] < 0.01,
                "frame {f}: {p:?}"
            );
        }
    }
}

// ---- Fenland

/// Fenland's Blender shot as a command recipe
/// (`examples/fenland-blender-shot`). Needs Blender and the shot's
/// inputs: set `VIDEBIMUS_FENLAND_SONG` to the directory holding
/// `hwscene.py` and `out/hw/s14/`, and `VIDEBIMUS_BLENDER` to Blender's
/// executable if it isn't on `PATH`. Renders 2 frames at low samples.
#[test]
#[ignore = "needs Blender and Fenland's shot inputs: set VIDEBIMUS_FENLAND_SONG"]
fn fenland_blender_shot() {
    let Ok(song) = std::env::var("VIDEBIMUS_FENLAND_SONG") else {
        panic!("set VIDEBIMUS_FENLAND_SONG to the directory holding hwscene.py and out/hw/s14");
    };
    let song = PathBuf::from(song);
    let blender = std::env::var("VIDEBIMUS_BLENDER").unwrap_or_else(|_| "blender".to_owned());
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/fenland-blender-shot");
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(example.join("video.kdl"), dir.path().join("video.kdl")).unwrap();
    let shot = song.join("out/hw/s14");
    std::fs::write(
        dir.path().join("local.kdl"),
        format!(
            "let blender={blender:?} scene-script={:?} panel={:?} controls={:?} camera={:?} from=(f)1 to=(f)3 samples=4 width=480 height=270\n",
            song.join("hwscene.py").display().to_string(),
            shot.join("panel.mp4").display().to_string(),
            shot.join("controls.json").display().to_string(),
            shot.join("camera.json").display().to_string(),
        ),
    )
    .unwrap();
    let console = Console::memory();
    let config = dir.path().join("video.kdl").display().to_string();
    let code = App::new("videbimus").run_from(["--config", &config, "build"], &console);
    println!("{}{}", console.out(), console.err());
    assert_eq!(code, ExitCode::SUCCESS);
    let store = Store::open_read_only(dir.path().join(".videbimus/store")).unwrap();
    let key = store.latest("s14/blender").unwrap().unwrap();
    let mut frames = Vec::new();
    for c in store.committed(key).unwrap() {
        for e in std::fs::read_dir(store.chunk_dir(key, c.id())).unwrap() {
            frames.push(e.unwrap().file_name().to_string_lossy().into_owned());
        }
    }
    frames.sort();
    assert_eq!(frames, ["0001.png", "0002.png"]);
    let img = read_png(
        &store
            .chunk_dir(key, store.committed(key).unwrap()[0].id())
            .join("0001.png"),
    )
    .unwrap();
    assert_eq!((img.width(), img.height()), (480, 270));
    if let Ok(keep) = std::env::var("VIDEBIMUS_FENLAND_KEEP") {
        let keep = PathBuf::from(keep);
        std::fs::create_dir_all(&keep).unwrap();
        for c in store.committed(key).unwrap() {
            for e in std::fs::read_dir(store.chunk_dir(key, c.id())).unwrap() {
                let e = e.unwrap();
                std::fs::copy(e.path(), keep.join(e.file_name())).unwrap();
            }
        }
    }
}

// ---- a bare --config file name (found by the fenland project, 0008)

#[test]
fn a_bare_config_name_runs_commands_in_its_directory() {
    let d = Dir::new(
        r#"
video rate=30
asset "here" recipe="command" {
    run "sh" "-c" "pwd -P > \"$1/cwd.txt\"" "sh" "$out"
    script "tool.sh"
    output file="cwd.txt"
}
"#,
    );
    d.write("tool.sh", "true\n");
    // run from the project directory with `--config video.kdl`, whose
    // parent is the empty path
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_videbimus"))
        .args(["--config", "video.kdl", "build"])
        .current_dir(d.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let cwd = std::fs::read_to_string(d.chunks("here")[0].join("cwd.txt")).unwrap();
    assert_eq!(
        Path::new(cwd.trim()),
        d.path().canonicalize().unwrap(),
        "the default cwd is the config's directory"
    );
}
