//! The output's plan: segments and join added to the asset graph; export
//! and review.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use videbimus_config::{Config, Error};
use videbimus_core::{AssetKey, Frame, Image, Interval};
use videbimus_runtime::raster::placeholder;
use videbimus_runtime::registry::did_you_mean;
use videbimus_runtime::{AssetRead, ComponentRegistry};
use videbimus_scheduler::{Graph, Node, NodeId, Target, TaskError};
use videbimus_store::{ChunkId, Store};

use crate::encode::encode_segment;
use crate::join::{AUDIO_LABEL, JoinTask, audio_file, concat_list, join_command, segment_label};
use crate::segment::{SEGMENT_FILE, SEGMENT_META, SceneSource, SegmentTask, meta};
use crate::spec::{AudioSource, OutputSpec};
use crate::state::state_key;

/// The store names of the output's tasks start with this: segments are
/// `output/segment-00000`, …, and the join is `output/join`. It is also
/// their group in the progress tree.
pub const NAME: &str = "output";

/// One segment of the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    /// Its task's name.
    pub name: String,
    /// Its frames.
    pub frames: Interval,
    /// Its node in the graph.
    pub node: NodeId,
    /// The assets it reads, by name.
    pub reads: Vec<String>,
}

/// What [`Output::plan`] needs from the project.
pub struct PlanContext<'a> {
    /// The resolved config.
    pub config: &'a Config,
    /// The root component: an index into `config.nodes()`, if there is one.
    pub root: Option<usize>,
    /// The kinds to build the scene with.
    pub registry: &'a ComponentRegistry,
    /// The project's store.
    pub store: &'a Path,
    /// The scene's canonical config: the `video` node and the root, as
    /// keyed bytes.
    pub scene_identity: Vec<u8>,
    /// The scene's reads of declared assets, with their nodes.
    pub wired: &'a [(NodeId, AssetRead)],
    /// The node of a declared asset, by name.
    pub asset: &'a dyn Fn(&str) -> Option<NodeId>,
    /// Every declared asset's name, for suggestions.
    pub asset_names: Vec<&'a str>,
}

/// The encoded output, planned: its segments and join, in the graph.
#[derive(Debug)]
pub struct Output {
    spec: OutputSpec,
    frames: Interval,
    segments: Vec<Segment>,
    join: NodeId,
    join_name: String,
}

/// What a review assembled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    /// The file written.
    pub path: PathBuf,
    /// Segments in all.
    pub segments: usize,
    /// The frames of segments that weren't built, shown as placeholders.
    pub missing: Vec<Interval>,
    /// Whether audio was muxed.
    pub audio: bool,
}

impl Output {
    /// Reads the config's `output` and adds its tasks to `graph`: a task
    /// per segment, reading the assets the scene shows during its frames,
    /// and the join, reading every segment and the audio asset. `None` if
    /// the config declares no output.
    ///
    /// # Errors
    ///
    /// A located error: a bad `output`, no scene, an empty scene, an
    /// unknown audio asset, or a task name an asset already has.
    #[allow(clippy::too_many_lines)]
    pub fn plan(cx: &PlanContext<'_>, graph: &mut Graph) -> Result<Option<Self>, Error> {
        let Some(spec) = OutputSpec::from_config(cx.config)? else {
            return Ok(None);
        };
        let Some(root) = cx.root else {
            return Err(Error::new("an output needs a scene, and the config has none")
                .at(spec.origin.clone())
                .element("output")
                .help("add a top-level component (and root=\"…\" on `video` if there are several)"));
        };
        let source = Arc::new(SceneSource::new(
            cx.config.clone(),
            root,
            cx.registry.clone(),
            cx.store.to_owned(),
        ));
        let (scene, reads) = source.build_recording()?;
        let end = scene.interval().end.0;
        if end <= 0 {
            return Err(Error::new("the scene has no frames to output")
                .at(spec.origin.clone())
                .element("output")
                .help("give the root component a length"));
        }
        let kinds: Arc<Vec<(String, Vec<u8>)>> = Arc::new(
            reads
                .kinds()
                .into_iter()
                .map(|k| {
                    let code = cx.registry.code(k).cloned().unwrap_or_default();
                    (k.to_owned(), code.bytes())
                })
                .collect(),
        );
        let key = state_key(&cx.scene_identity, &kinds);
        let frames = Interval::new(Frame(0), Frame(end));
        let at = |e: &dyn std::fmt::Display| {
            Error::new(e.to_string())
                .at(spec.origin.clone())
                .element("output")
                .help(format!(
                    "names under `{NAME}/` are the output's: rename the asset"
                ))
        };
        let mut segments: Vec<Segment> = Vec::new();
        let step = i64::from(spec.segment_frames);
        let mut start = 0;
        while start < end {
            let iv = Interval::new(Frame(start), Frame((start + step).min(end)));
            let name = format!("{NAME}/segment-{:05}", segments.len());
            let mut config = cx.scene_identity.clone();
            config.extend_from_slice(
                format!(
                    "\n{}\nframes {} {}",
                    spec.encode.canonical(),
                    iv.start.0,
                    iv.end.0
                )
                .as_bytes(),
            );
            let task = SegmentTask {
                name: name.clone(),
                frames: iv,
                prev: segments.last().map(|s| s.name.clone()),
                scene: source.clone(),
                encode: spec.encode.clone(),
                config,
                kinds: kinds.clone(),
                state_key: key,
            };
            let mut node = Node::new(task).group(NAME);
            let mut read_names: Vec<String> = Vec::new();
            for (id, read) in cx.wired {
                if read.video.overlap(iv).is_some() && !read_names.contains(&read.name) {
                    node = node.input(&format!("asset/{}", read.name), *id);
                    read_names.push(read.name.clone());
                }
            }
            let id = graph.add(node).map_err(|e| at(&e))?;
            segments.push(Segment {
                name,
                frames: iv,
                node: id,
                reads: read_names,
            });
            start = iv.end.0;
        }
        let join_name = format!("{NAME}/join");
        let mut node = Node::new(JoinTask {
            name: join_name.clone(),
            segments: segments.len(),
            spec: spec.clone(),
            frames: end,
        })
        .group(NAME);
        for (i, s) in segments.iter().enumerate() {
            node = node.input(&segment_label(i), s.node);
        }
        if let Some(a) = &spec.audio
            && let AudioSource::Asset { name, .. } = &a.source
        {
            let Some(id) = (cx.asset)(name) else {
                let help = match did_you_mean(name, &cx.asset_names) {
                    Some(k) => format!("did you mean `{k}`?"),
                    None => format!("declare it: asset \"{name}\" recipe=\"…\""),
                };
                return Err(
                    Error::new(format!("there is no asset `{name}` in this config"))
                        .at(a.origin.clone())
                        .element("audio")
                        .help(help),
                );
            };
            node = node.input(AUDIO_LABEL, id);
        }
        let join = graph.add(node).map_err(|e| at(&e))?;
        Ok(Some(Self {
            spec,
            frames,
            segments,
            join,
            join_name,
        }))
    }

    /// What the config declares.
    #[must_use]
    pub fn spec(&self) -> &OutputSpec {
        &self.spec
    }

    /// The video's frames: from 0 to the root's end.
    #[must_use]
    pub fn frames(&self) -> Interval {
        self.frames
    }

    /// The segments, in order.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// The join's task name.
    #[must_use]
    pub fn join_name(&self) -> &str {
        &self.join_name
    }

    /// The join's node.
    #[must_use]
    pub fn join(&self) -> NodeId {
        self.join
    }

    /// The target that builds the output: the join, and so every segment.
    #[must_use]
    pub fn target(&self) -> Target {
        Target::all(self.join)
    }

    /// Copies the join built under `key` to the output's file, replacing
    /// it whole (written beside it, then renamed).
    ///
    /// # Errors
    ///
    /// If the join isn't built under `key`, or the copy fails.
    pub fn export(&self, store: &Store, key: AssetKey) -> Result<PathBuf, String> {
        let src = store
            .chunk_dir(key, ChunkId::Whole)
            .join(&self.spec.file_name);
        if !src.is_file() {
            return Err(format!("the joined output isn't built ({})", src.display()));
        }
        let dest = &self.spec.path;
        if let Some(dir) = dest.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("can't create {}: {e}", dir.display()))?;
        }
        let tmp = dest.with_file_name(format!(".{}.part", self.spec.file_name));
        std::fs::copy(&src, &tmp).map_err(|e| format!("can't write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, dest).map_err(|e| format!("can't write {}: {e}", dest.display()))?;
        Ok(dest.clone())
    }

    /// Where [`review`](Self::review) writes by default: beside the
    /// output, `film.review.mp4` for `film.mp4`.
    #[must_use]
    pub fn default_review_path(&self) -> PathBuf {
        let p = Path::new(&self.spec.file_name);
        let stem = p.file_stem().unwrap_or_default().to_string_lossy();
        let ext = p.extension().unwrap_or_default().to_string_lossy();
        self.spec
            .path
            .with_file_name(format!("{stem}.review.{ext}"))
    }

    /// Assembles what exists into `dest`, building nothing (ADR 0008 §5):
    /// each segment's latest committed build, if it is of this plan's
    /// frames and encoding; a labelled placeholder (`NOT RENDERED <frame>`)
    /// for each frame of the others; and the audio, if it exists. Reads
    /// the store at `store` without locking it, so it runs beside a build.
    ///
    /// A segment's latest build may be of an older config: review shows
    /// what was rendered, not whether it is current.
    ///
    /// # Errors
    ///
    /// If ffmpeg fails, or a file can't be written.
    pub fn review(&self, store: &Path, dest: &Path) -> Result<Review, String> {
        let store = if store.is_dir() {
            Some(Store::open_read_only(store).map_err(|e| e.to_string())?)
        } else {
            None
        };
        let dir = dest
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
        let work = dir.join(format!(".videbimus-review-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)
            .map_err(|e| format!("can't create {}: {e}", work.display()))?;
        let result = self.assemble(store.as_ref(), &work, dest);
        let _ = std::fs::remove_dir_all(&work);
        result
    }

    fn assemble(&self, store: Option<&Store>, work: &Path, dest: &Path) -> Result<Review, String> {
        let mut videos = Vec::new();
        let mut missing = Vec::new();
        let expected = |iv: Interval| meta(iv, &self.spec.encode);
        for (i, s) in self.segments.iter().enumerate() {
            if let Some(found) = store.and_then(|st| built(st, &s.name, &expected(s.frames))) {
                videos.push(found);
                continue;
            }
            missing.push(s.frames);
            let out = work.join(format!("placeholder-{i:05}.mp4"));
            let (w, h) = (self.spec.encode.width, self.spec.encode.height);
            let start = s.frames.start.0;
            encode_segment(
                &self.spec.encode,
                usize::try_from(s.frames.len()).unwrap_or(0),
                &out,
                work,
                threads(),
                |k| {
                    let mut img = Image::new(w, h);
                    placeholder(
                        &mut img,
                        &format!(
                            "NOT RENDERED {}",
                            start + i64::try_from(k).expect("a frame index fits")
                        ),
                    );
                    Ok(img)
                },
                run_plain,
                &|| false,
            )
            .map_err(|e| e.to_string())?;
            videos.push(out);
        }
        let audio_path = self.spec.audio.as_ref().and_then(|a| match &a.source {
            AudioSource::File(p) => p.is_file().then(|| p.clone()),
            AudioSource::Asset { name, file } => {
                let st = store?;
                let key = st.latest(name).ok()??;
                st.committed(key)
                    .ok()?
                    .iter()
                    .any(|c| c.id() == ChunkId::Whole)
                    .then_some(())?;
                audio_file(&st.chunk_dir(key, ChunkId::Whole), name, file.as_deref()).ok()
            }
        });
        let list = work.join("concat.txt");
        std::fs::write(&list, concat_list(&videos))
            .map_err(|e| format!("can't write {}: {e}", list.display()))?;
        let audio = self
            .spec
            .audio
            .as_ref()
            .and_then(|a| audio_path.as_deref().map(|p| (p, a)));
        let mut cmd = join_command(&list, audio, self.spec.encode.rate, self.frames.end.0, dest);
        run_plain(&mut cmd).map_err(|e| e.to_string())?;
        Ok(Review {
            path: dest.to_owned(),
            segments: self.segments.len(),
            missing,
            audio: audio.is_some(),
        })
    }
}

/// The video of the latest committed build of segment `name`, if it is
/// of the expected frames and encoding.
fn built(store: &Store, name: &str, expected_meta: &str) -> Option<PathBuf> {
    let key = store.latest(name).ok()??;
    if !store
        .committed(key)
        .ok()?
        .iter()
        .any(|c| c.id() == ChunkId::Whole)
    {
        return None;
    }
    let dir = store.chunk_dir(key, ChunkId::Whole);
    let meta = std::fs::read_to_string(dir.join(SEGMENT_META)).ok()?;
    let video = dir.join(SEGMENT_FILE);
    (meta == expected_meta && video.is_file()).then_some(video)
}

fn threads() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| u32::try_from(n.get()).unwrap_or(1))
}

/// Runs a command outside any build, keeping its output for errors.
fn run_plain(cmd: &mut Command) -> Result<(), TaskError> {
    let out = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| {
            TaskError::new(format!(
                "couldn't start {}: {e}",
                cmd.get_program().display()
            ))
        })?;
    if out.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<String> = text.lines().rev().take(20).map(str::to_owned).collect();
    Err(TaskError::new(format!(
        "{} failed: {}: {}",
        cmd.get_program().display(),
        out.status,
        lines.into_iter().rev().collect::<Vec<_>>().join("\n")
    )))
}
