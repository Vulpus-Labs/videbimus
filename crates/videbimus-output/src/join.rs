//! Joining the segments by stream copy, and muxing the audio.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use videbimus_core::{Frame, Rate};
use videbimus_scheduler::{Cost, Resources, Task, TaskContext, TaskError};
use videbimus_store::{KeyBuilder, Shape};

use crate::encode::{ffmpeg, ffmpeg_version};
use crate::segment::{JOIN_CODE, SEGMENT_FILE};
use crate::spec::{AudioSource, AudioSpec, OutputSpec};

/// The label of segment `i` among the join's inputs: ordered, so
/// swapping two segments changes the key.
#[must_use]
pub fn segment_label(i: usize) -> String {
    format!("segment/{i:05}")
}

/// The label of the audio asset among the join's inputs.
pub const AUDIO_LABEL: &str = "audio";

/// The join's task: concatenates the segments' videos with a stream copy
/// and muxes the audio, into the output's file name.
pub(crate) struct JoinTask {
    pub name: String,
    pub segments: usize,
    pub spec: OutputSpec,
    pub frames: i64,
}

impl Task for JoinTask {
    fn name(&self) -> &str {
        &self.name
    }

    fn shape(&self) -> Shape {
        Shape::Single
    }

    fn resources(&self) -> Resources {
        Resources::new(1, 256 << 20)
    }

    #[allow(clippy::cast_precision_loss)]
    fn cost(&self) -> Cost {
        Cost::Seconds(1.0 + self.frames as f64 / 3000.0)
    }

    fn key(&self) -> KeyBuilder {
        let k = KeyBuilder::new("output-join")
            .code(JOIN_CODE)
            .config(format!("{} frames {}", self.spec.canonical(), self.frames))
            .tool("ffmpeg", ffmpeg_version());
        match &self.spec.audio {
            Some(AudioSpec {
                source: AudioSource::File(path),
                ..
            }) => k.source_file("audio", path),
            _ => k,
        }
    }

    fn run(&self, ctx: &TaskContext<'_>) -> Result<(), TaskError> {
        let mut videos = Vec::with_capacity(self.segments);
        for i in 0..self.segments {
            let label = segment_label(i);
            let dir = ctx
                .input_dirs(&label)?
                .into_iter()
                .next()
                .ok_or_else(|| TaskError::new(format!("input {label} has no output")))?;
            videos.push(dir.join(SEGMENT_FILE));
        }
        let audio = match &self.spec.audio {
            None => None,
            Some(a) => {
                let path =
                    match &a.source {
                        AudioSource::File(p) => p.clone(),
                        AudioSource::Asset { name, file } => {
                            let dir = ctx.input_dirs(AUDIO_LABEL)?.into_iter().next().ok_or_else(
                                || TaskError::new(format!("audio {name} has no output")),
                            )?;
                            audio_file(&dir, name, file.as_deref())?
                        }
                    };
                Some((path, a))
            }
        };
        let list = ctx.out_dir().join("concat.txt");
        std::fs::write(&list, concat_list(&videos))?;
        let out = ctx.out_dir().join(&self.spec.file_name);
        let mut cmd = join_command(
            &list,
            audio.as_ref().map(|(p, a)| (p.as_path(), *a)),
            self.spec.encode.rate,
            self.frames,
            &out,
        );
        ctx.log(&format!(
            "joining {} segments ({} frames){}",
            self.segments,
            self.frames,
            audio.as_ref().map_or(String::new(), |(p, _)| format!(
                " with audio {}",
                p.display()
            ))
        ));
        let ran = ctx.command(&mut cmd);
        let _ = std::fs::remove_file(&list);
        ran.map(|_| ())
    }
}

/// The concat demuxer's list of `videos`, as absolute paths (relative
/// ones would be read from the list's directory).
#[must_use]
pub fn concat_list(videos: &[PathBuf]) -> String {
    let mut s = String::new();
    for v in videos {
        let v = std::path::absolute(v).unwrap_or_else(|_| v.clone());
        // the concat demuxer's quoting: ' ends the string, \' is a quote
        let p = v.to_string_lossy().replace('\'', r"'\''");
        let _ = writeln!(s, "file '{p}'");
    }
    s
}

/// The audio file in an asset's output `dir`: `file` if given, else its
/// only file.
///
/// # Errors
///
/// If `file` isn't there, or there isn't exactly one file.
pub fn audio_file(dir: &Path, asset: &str, file: Option<&str>) -> Result<PathBuf, TaskError> {
    if let Some(f) = file {
        let p = dir.join(f);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(TaskError::new(format!(
                "audio asset {asset} has no file `{f}`"
            )))
        };
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    files.sort();
    match files.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(TaskError::new(format!("audio asset {asset} has no files"))),
        many => Err(TaskError::new(format!(
            "audio asset {asset} has {} files; pick one with file=\"…\" on `audio`: {}",
            many.len(),
            many.iter()
                .filter_map(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// The ffmpeg command that joins the videos listed in `list` by stream
/// copy into `out`, muxing `audio` if given. The audio is resampled to its
/// sample rate and cut or padded with silence to exactly the samples of
/// `frames` frames (ADR 0001 §3), sample 0 with frame 0.
#[must_use]
pub fn join_command(
    list: &Path,
    audio: Option<(&Path, &AudioSpec)>,
    rate: Rate,
    frames: i64,
    out: &Path,
) -> Command {
    let mut cmd = Command::new(ffmpeg());
    cmd.args(["-hide_banner", "-nostdin", "-y"])
        .args(["-f", "concat", "-safe", "0", "-i"])
        .arg(list);
    match audio {
        None => {
            cmd.args(["-map", "0:v:0", "-c", "copy"]);
        }
        Some((path, a)) => {
            let samples = rate.sample(Frame(frames), a.sample_rate);
            cmd.arg("-i")
                .arg(path)
                .args(["-map", "0:v:0", "-map", "1:a:0", "-c:v", "copy"])
                .arg("-af")
                .arg(format!(
                    "aresample={sr},apad=whole_len={samples},atrim=end_sample={samples}",
                    sr = a.sample_rate
                ))
                .args(["-c:a", "aac", "-b:a", &a.bitrate])
                .arg("-ar")
                .arg(a.sample_rate.to_string());
        }
    }
    let ext = out
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    if matches!(ext.as_deref(), Some("mp4" | "mov" | "m4v")) {
        cmd.args(["-movflags", "+faststart"]);
    }
    cmd.arg(out);
    cmd
}
