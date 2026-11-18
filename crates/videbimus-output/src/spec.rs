//! The `output` node: what file the video is encoded to, in what
//! segments, with what audio.

use std::path::{Path, PathBuf};

use videbimus_config::{Config, Error, Node, Origin, Resolver};
use videbimus_core::{Frame, Rate};
use videbimus_runtime::registry::origin_of;

/// The top-level node that declares the encoded output.
pub const OUTPUT: &str = "output";

/// The `output` node's child that declares its audio.
pub const AUDIO: &str = "audio";

/// x264's presets.
const PRESETS: [&str; 10] = [
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
    "slow",
    "slower",
    "veryslow",
    "placebo",
];

/// Segment length when `segment` isn't given, in seconds.
pub const DEFAULT_SEGMENT_SECONDS: f64 = 4.0;

/// How the video is encoded: the same for every segment, so segments join
/// by stream copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodeSettings {
    /// Width in pixels (even).
    pub width: u32,
    /// Height in pixels (even).
    pub height: u32,
    /// Frame rate.
    pub rate: Rate,
    /// x264's constant rate factor: 0 lossless to 51; 18 by default.
    pub crf: u32,
    /// x264's preset: `ultrafast` … `veryslow`; `medium` by default.
    pub preset: String,
}

impl EncodeSettings {
    /// As canonical bytes, for keys.
    #[must_use]
    pub fn canonical(&self) -> String {
        format!(
            "encode libx264 yuv420p bt709 {}x{} rate {}/{} crf {} preset {}",
            self.width,
            self.height,
            self.rate.num(),
            self.rate.den(),
            self.crf,
            self.preset
        )
    }
}

/// Where the audio comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioSource {
    /// A single-output asset the project declares, by name: its one file,
    /// or the file named `file` in it.
    Asset {
        /// The asset's name.
        name: String,
        /// The file in its output, if it has several.
        file: Option<String>,
    },
    /// A file, relative to the config file. Hashed into the join's key.
    File(PathBuf),
}

/// The output's audio: muxed sample-exact against the frames (ADR 0001
/// §3): sample 0 plays with frame 0, and the track is cut or padded with
/// silence to `floor(frames · sample_rate · den / num)` samples.
#[derive(Clone, Debug)]
pub struct AudioSpec {
    /// Where it comes from.
    pub source: AudioSource,
    /// The output sample rate; 48000 by default.
    pub sample_rate: u32,
    /// The AAC bitrate; `"320k"` by default.
    pub bitrate: String,
    /// Where it is declared.
    pub origin: Origin,
}

/// The `output` node, read.
///
/// ```kdl
/// video rate=30 width=1920 height=1080
/// output "film.mp4" segment=(s)4 crf=18 preset="medium" {
///     audio asset="mix"        // a declared asset's output file
///     // or: audio "mix.wav"   // a file, relative to the config
/// }
/// ```
#[derive(Clone, Debug)]
pub struct OutputSpec {
    /// The file the joined video is exported to.
    pub path: PathBuf,
    /// Its file name, as stored in the join's output.
    pub file_name: String,
    /// Frames per segment.
    pub segment_frames: u32,
    /// How the video is encoded.
    pub encode: EncodeSettings,
    /// Its audio, if any.
    pub audio: Option<AudioSpec>,
    /// Where it is declared.
    pub origin: Origin,
}

impl OutputSpec {
    /// The config's `output` node, read; `None` if it has none.
    ///
    /// # Errors
    ///
    /// A located error for anything wrong with it: a second `output`, no
    /// `video` size, an odd size, an unknown property or child.
    #[allow(clippy::too_many_lines)]
    pub fn from_config(config: &Config) -> Result<Option<Self>, Error> {
        let r = config.resolver();
        let node = match config.nodes_of(OUTPUT).collect::<Vec<_>>().as_slice() {
            [] => return Ok(None),
            [one] => *one,
            [_, second, ..] => {
                return Err(r
                    .error(
                        second,
                        second.origin(),
                        "a config has one `output`, and this is a second",
                    )
                    .help("remove one"));
            }
        };
        r.check_props(node, &["segment", "crf", "preset"])?;
        let (Some(video), Some(tb)) = (config.nodes_of("video").next(), config.timebase()) else {
            return Err(r
                .error(
                    node,
                    node.origin(),
                    "an output needs the `video` node's size and rate",
                )
                .help("add `video rate=30 width=1920 height=1080` at the top"));
        };
        let width: u32 = r.get(video, "width")?;
        let height: u32 = r.get(video, "height")?;
        if width == 0 || height == 0 || width % 2 == 1 || height % 2 == 1 {
            return Err(r
                .error(
                    video,
                    video.origin(),
                    format!("the output is encoded as 4:2:0, which needs an even size, and this is {width}×{height}"),
                )
                .element("video")
                .help("make `width` and `height` even"));
        }
        if node.args().len() != 1 {
            return Err(r
                .error(node, node.origin(), "an output names one file")
                .help("output \"film.mp4\""));
        }
        let file: String = r.arg(node, 0)?;
        let path = relative_to(&node.args()[0].origin, Path::new(&file));
        let file_name = path
            .file_name()
            .and_then(|f| f.to_str())
            .filter(|f| Path::new(f).extension().is_some())
            .ok_or_else(|| {
                r.error(
                    node,
                    &node.args()[0].origin,
                    format!("`{file}` isn't a file name with an extension"),
                )
                .help(
                    "name the file, with its container: \"film.mp4\", \"film.mov\" or \"film.mkv\"",
                )
            })?
            .to_owned();
        let segment_frames = segment_frames(node, r, tb.rate)?;
        let crf: u32 = r.opt(node, "crf")?.unwrap_or(18);
        if crf > 51 {
            return Err(r
                .error(
                    node,
                    &origin_of(node, "crf"),
                    format!("a crf of {crf} is out of range"),
                )
                .help("x264's crf is 0 (lossless) to 51; 18 is visually transparent"));
        }
        let preset: String = r
            .opt(node, "preset")?
            .unwrap_or_else(|| "medium".to_owned());
        if !PRESETS.contains(&preset.as_str()) {
            let help = match videbimus_runtime::registry::did_you_mean(&preset, &PRESETS) {
                Some(p) => format!("did you mean `{p}`?"),
                None => format!("the presets are {}", PRESETS.join(", ")),
            };
            return Err(r
                .error(
                    node,
                    &origin_of(node, "preset"),
                    format!("there is no x264 preset `{preset}`"),
                )
                .help(help));
        }
        let mut audio = None;
        for c in node.children() {
            if c.kind() != AUDIO {
                return Err(r
                    .error(
                        c,
                        c.origin(),
                        format!("an output holds `audio`, not `{}`", c.kind()),
                    )
                    .help("audio asset=\"<asset name>\", or audio \"<file>\""));
            }
            if audio.is_some() {
                return Err(r
                    .error(
                        c,
                        c.origin(),
                        "an output has one `audio`, and this is a second",
                    )
                    .help("mix the audio into one asset, and name that"));
            }
            audio = Some(read_audio(c, r)?);
        }
        Ok(Some(Self {
            path,
            file_name,
            segment_frames,
            encode: EncodeSettings {
                width,
                height,
                rate: tb.rate,
                crf,
                preset,
            },
            audio,
            origin: node.origin().clone(),
        }))
    }

    /// As canonical bytes, for the join's key: the file name, segment
    /// length, encoding and audio settings (not the audio's content,
    /// which is an input).
    #[must_use]
    pub fn canonical(&self) -> String {
        let audio = match &self.audio {
            None => "none".to_owned(),
            Some(a) => {
                let src = match &a.source {
                    AudioSource::Asset { file, .. } => {
                        format!("asset file {}", file.as_deref().unwrap_or("-"))
                    }
                    AudioSource::File(_) => "file".to_owned(),
                };
                format!("{src} rate {} aac {}", a.sample_rate, a.bitrate)
            }
        };
        format!(
            "output {:?} segment {} {} audio {audio}",
            self.file_name,
            self.segment_frames,
            self.encode.canonical()
        )
    }
}

fn relative_to(origin: &Origin, p: &Path) -> PathBuf {
    if p.is_absolute() {
        return p.to_owned();
    }
    origin.at.path().parent().unwrap_or(Path::new("")).join(p)
}

fn segment_frames(node: &Node, r: &Resolver, rate: Rate) -> Result<u32, Error> {
    let frames = match r.opt::<Frame>(node, "segment")? {
        Some(f) => f.0,
        None => rate.frame_at(DEFAULT_SEGMENT_SECONDS).0,
    };
    u32::try_from(frames)
        .ok()
        .filter(|&f| f > 0)
        .ok_or_else(|| {
            r.error(
                node,
                &origin_of(node, "segment"),
                format!("a segment of {frames} frames is too short"),
            )
            .help("give a segment length of at least one frame: segment=(s)4")
        })
}

fn read_audio(node: &Node, r: &Resolver) -> Result<AudioSpec, Error> {
    r.check_props(node, &["asset", "file", "sample-rate", "bitrate"])?;
    let asset: Option<String> = r.opt(node, "asset")?;
    let source = match (asset, node.args()) {
        (Some(name), []) => AudioSource::Asset {
            name,
            file: r.opt(node, "file")?,
        },
        (None, [a]) => {
            if node.prop("file").is_some() {
                return Err(r
                    .error(node, &origin_of(node, "file"), "`file` picks a file in an asset's output, and this audio is a file already")
                    .help("drop `file`"));
            }
            let f: String = r.arg(node, 0)?;
            AudioSource::File(relative_to(&a.origin, Path::new(&f)))
        }
        (Some(_), [a, ..]) => {
            return Err(r
                .error(node, &a.origin, "both an asset and a file are given")
                .help("take the audio from one: audio asset=\"mix\", or audio \"mix.wav\""));
        }
        (None, _) => {
            return Err(r
                .error(node, node.origin(), "the audio names no asset or file")
                .help("audio asset=\"<asset name>\", or audio \"<file>\""));
        }
    };
    let sample_rate: u32 = r.opt(node, "sample-rate")?.unwrap_or(48_000);
    if !(8_000..=192_000).contains(&sample_rate) {
        return Err(r
            .error(
                node,
                &origin_of(node, "sample-rate"),
                format!("a sample rate of {sample_rate} Hz is out of range"),
            )
            .help("use 44100 or 48000"));
    }
    Ok(AudioSpec {
        source,
        sample_rate,
        bitrate: r.opt(node, "bitrate")?.unwrap_or_else(|| "320k".to_owned()),
        origin: node.origin().clone(),
    })
}
