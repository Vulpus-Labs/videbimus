//! A segment of the output: its frames rendered through the scene and
//! encoded, as one asset with its own key.

use std::sync::Arc;

use videbimus_core::{Frame, Interval};
use videbimus_runtime::{AssetReads, ComponentRegistry, DriveOptions, Scene};
use videbimus_scheduler::{Cost, Resources, Task, TaskContext, TaskError};
use videbimus_store::{KeyBuilder, Shape};

use crate::encode::{encode_segment, ffmpeg_version};
use crate::spec::EncodeSettings;
use crate::state::{STATE_FILE, SavedState, load_previous};

/// The segment's video, in its output.
pub const SEGMENT_FILE: &str = "segment.mp4";

/// What the segment holds, in its output: its frames and encoding, so
/// review can tell a segment of another plan from one of this.
pub const SEGMENT_META: &str = "segment.json";

mod code {
    include!(concat!(env!("OUT_DIR"), "/videbimus_code.rs"));
}

/// The output recipes' code identities, from this crate's build script.
pub(crate) const SEGMENT_CODE: &str = code::OUTPUT_SEGMENT;
pub(crate) const JOIN_CODE: &str = code::OUTPUT_JOIN;

/// How to build the scene, from any thread: a config, its root and the
/// kinds to build it with. Each segment builds its own tree (a tree
/// renders on one thread).
#[derive(Debug)]
pub struct SceneSource {
    config: videbimus_config::Config,
    root: usize,
    registry: ComponentRegistry,
    store: std::path::PathBuf,
}

impl SceneSource {
    /// The scene of the top-level node `root` (an index into
    /// `config.nodes()`), built with `registry`, reading assets from
    /// `store`.
    #[must_use]
    pub fn new(
        config: videbimus_config::Config,
        root: usize,
        registry: ComponentRegistry,
        store: std::path::PathBuf,
    ) -> Self {
        Self {
            config,
            root,
            registry,
            store,
        }
    }

    /// A fresh scene.
    ///
    /// # Errors
    ///
    /// If it won't build.
    pub fn build(&self) -> Result<Scene, videbimus_config::Error> {
        self.build_recording().map(|(s, _)| s)
    }

    /// A fresh scene, with what building it read and which kinds it
    /// built.
    ///
    /// # Errors
    ///
    /// If it won't build.
    pub fn build_recording(&self) -> Result<(Scene, AssetReads), videbimus_config::Error> {
        let reads = AssetReads::new(Some(self.store.clone()));
        let scene = {
            let builder = self
                .registry
                .builder_reading(DriveOptions::default(), &reads);
            videbimus_runtime::scene_from_config_with(
                &self.config,
                &self.config.nodes()[self.root],
                builder,
                0,
            )?
        };
        Ok((scene, reads))
    }

    /// The registry.
    #[must_use]
    pub fn registry(&self) -> &ComponentRegistry {
        &self.registry
    }
}

/// One segment's task.
pub(crate) struct SegmentTask {
    pub name: String,
    pub frames: Interval,
    /// The previous segment's name, whose saved state this one starts
    /// from if it can.
    pub prev: Option<String>,
    pub scene: Arc<SceneSource>,
    pub encode: EncodeSettings,
    /// Canonical config: the scene's, the encoding, the frames.
    pub config: Vec<u8>,
    /// Each kind in the scene and its code identity.
    pub kinds: Arc<Vec<(String, Vec<u8>)>>,
    pub state_key: [u8; 32],
}

impl Task for SegmentTask {
    fn name(&self) -> &str {
        &self.name
    }

    fn shape(&self) -> Shape {
        Shape::Single
    }

    fn resources(&self) -> Resources {
        let pixels = u64::from(self.encode.width) * u64::from(self.encode.height);
        Resources::new(2, (256 << 20) + pixels * 64)
    }

    #[allow(clippy::cast_precision_loss)]
    fn cost(&self) -> Cost {
        let pixels = f64::from(self.encode.width) * f64::from(self.encode.height);
        Cost::Seconds(0.5 + self.frames.len() as f64 * pixels / 2.0e7)
    }

    fn key(&self) -> KeyBuilder {
        let mut k = KeyBuilder::new("output-segment")
            .code(SEGMENT_CODE)
            .config(&self.config)
            .tool("ffmpeg", ffmpeg_version());
        for (kind, code) in self.kinds.iter() {
            k = k.source(&format!("kind/{kind}"), code);
        }
        k
    }

    fn run(&self, ctx: &TaskContext<'_>) -> Result<(), TaskError> {
        let mut scene = self
            .scene
            .build()
            .map_err(|e| TaskError::new(e.to_string()))?;
        let start = self.frames.start.0;
        if let Some(prev) = &self.prev {
            match load_previous(ctx.store(), prev, start, &self.state_key) {
                Ok(saved) if scene.load_state(&saved.bytes) => ctx.log(&format!(
                    "resumed from the state {prev} saved at frame {start}"
                )),
                Ok(_) => ctx.log(&format!(
                    "the state {prev} saved didn't load: catching up from the start"
                )),
                Err(why) => ctx.log(&format!("no saved state ({why}): catching up")),
            }
        }
        let total = usize::try_from(self.frames.len()).unwrap_or(0);
        let every = (total / 10).max(1);
        let out = ctx.out_dir().join(SEGMENT_FILE);
        ctx.log(&format!(
            "rendering frames {start}..{} and encoding to {SEGMENT_FILE}",
            self.frames.end.0
        ));
        encode_segment(
            &self.encode,
            total,
            &out,
            ctx.out_dir(),
            ctx.cores(),
            |i| {
                if i % every == 0 {
                    ctx.progress(i as u64, total as u64);
                }
                Ok(scene.render(Frame(start + i64::try_from(i).expect("a frame index fits"))))
            },
            |cmd| ctx.command(cmd).map(|_| ()),
            &|| ctx.is_cancelled(),
        )?;
        ctx.progress(total as u64, total as u64);
        let saved = SavedState {
            key: self.state_key,
            frame: self.frames.end.0,
            bytes: scene.save_state(),
        };
        std::fs::write(ctx.out_dir().join(STATE_FILE), saved.encode())?;
        std::fs::write(
            ctx.out_dir().join(SEGMENT_META),
            meta(self.frames, &self.encode),
        )?;
        Ok(())
    }
}

/// A segment's [`SEGMENT_META`].
pub(crate) fn meta(frames: Interval, encode: &EncodeSettings) -> String {
    serde_json::json!({
        "frames": [frames.start.0, frames.end.0],
        "encode": encode.canonical(),
    })
    .to_string()
}
