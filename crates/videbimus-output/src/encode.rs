//! Encoding frames with ffmpeg.
//!
//! Every segment is its own H.264 stream in its own MP4, encoded with the
//! same settings ([`EncodeSettings`]). Each therefore starts with an IDR
//! frame and no frame refers across a segment boundary (a closed GOP at
//! every boundary by construction), so the concat demuxer joins them with
//! `-c copy`: no re-encode, and a rebuilt segment drops into place.
//!
//! Frames are fed to ffmpeg as `rgb24` rawvideo through a named pipe
//! (Unix only, for now), so they are never written to disk; the scene renders on the
//! calling thread while ffmpeg runs on another, through whatever runner
//! the caller passes (a task's [`TaskContext::command`], so stop, niceness
//! and logging apply).
//!
//! [`TaskContext::command`]: videbimus_scheduler::TaskContext::command

use std::ffi::OsString;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use videbimus_core::Image;
use videbimus_runtime::color::linear_to_srgb;
use videbimus_scheduler::TaskError;

use crate::spec::EncodeSettings;

/// The ffmpeg binary: `VIDEBIMUS_FFMPEG`, else `ffmpeg` on the path.
#[must_use]
pub fn ffmpeg() -> OsString {
    std::env::var_os("VIDEBIMUS_FFMPEG").unwrap_or_else(|| "ffmpeg".into())
}

/// ffmpeg's version (`8.1.2`), read once per process: a tool input of the
/// output's keys. `"not found"` if it won't run.
#[must_use]
pub fn ffmpeg_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        Command::new(ffmpeg())
            .arg("-version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .and_then(|l| l.strip_prefix("ffmpeg version "))
                    .and_then(|l| l.split_whitespace().next())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "not found".to_owned())
    })
}

/// A frame as `rgb24`, over black: a premultiplied linear image over
/// black is its colour channels as they are, then sRGB-encoded.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn rgb24(image: &Image) -> Vec<u8> {
    let mut out = Vec::with_capacity(image.pixels().len() * 3);
    for p in image.pixels() {
        for &c in &p[..3] {
            out.push((linear_to_srgb(c.clamp(0.0, 1.0)) * 255.0).round() as u8);
        }
    }
    out
}

/// The ffmpeg command that encodes `rgb24` frames read from `input` to an
/// MP4 segment at `out`, with `threads` encoder threads.
#[must_use]
pub fn segment_command(s: &EncodeSettings, input: &Path, out: &Path, threads: u32) -> Command {
    let mut cmd = Command::new(ffmpeg());
    cmd.args(["-hide_banner", "-nostdin", "-y", "-f", "rawvideo"])
        .args(["-pix_fmt", "rgb24"])
        .arg("-s")
        .arg(format!("{}x{}", s.width, s.height))
        .arg("-framerate")
        .arg(format!("{}/{}", s.rate.num(), s.rate.den()))
        .arg("-i")
        .arg(input)
        .args(["-c:v", "libx264", "-preset", &s.preset])
        .arg("-crf")
        .arg(s.crf.to_string())
        .arg("-threads")
        .arg(threads.max(1).to_string())
        .args(["-vf", "scale=out_color_matrix=bt709:out_range=tv"])
        .args(["-pix_fmt", "yuv420p"])
        .args(["-colorspace", "bt709", "-color_primaries", "bt709"])
        .args(["-color_trc", "iec61966-2-1", "-color_range", "tv"])
        .args(["-movflags", "+faststart", "-f", "mp4"])
        .arg(out);
    cmd
}

/// Encodes `count` frames, from `frame(i)`, to an MP4 segment at `out`.
/// `work` is a directory for the pipe. `run` runs the ffmpeg command
/// (on another thread) and returns when it exits; `cancelled` is polled
/// between frames.
///
/// # Errors
///
/// A frame that can't be made, ffmpeg's failure, or a cancellation.
#[allow(clippy::too_many_arguments)]
pub fn encode_segment<F, R>(
    settings: &EncodeSettings,
    count: usize,
    out: &Path,
    work: &Path,
    threads: u32,
    mut frame: F,
    run: R,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<(), TaskError>
where
    F: FnMut(usize) -> Result<Image, TaskError>,
    R: FnOnce(&mut Command) -> Result<(), TaskError> + Send,
{
    let input = work.join("frames.rgb");
    let _ = std::fs::remove_file(&input);
    platform::make_pipe(&input)?;
    let mut cmd = segment_command(settings, &input, out, threads);
    let done = AtomicBool::new(false);
    let result = std::thread::scope(|s| {
        let encoder = s.spawn(|| {
            let r = run(&mut cmd);
            done.store(true, Ordering::SeqCst);
            r
        });
        let fed = feed(&input, count, &mut frame, &done, cancelled);
        let ran = encoder
            .join()
            .unwrap_or_else(|_| Err(TaskError::new("the encoder thread panicked")));
        match (fed, ran) {
            (_, Err(e)) if e.is_cancelled() => Err(e),
            (Err(e), _) | (Ok(()), Err(e)) => Err(e),
            (Ok(()), Ok(())) => Ok(()),
        }
    });
    let _ = std::fs::remove_file(&input);
    result
}

fn feed<F>(
    input: &Path,
    count: usize,
    frame: &mut F,
    done: &AtomicBool,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<(), TaskError>
where
    F: FnMut(usize) -> Result<Image, TaskError>,
{
    let Some(mut w) = platform::open_writer(input, done, cancelled)? else {
        // the encoder ended (or we were cancelled) before it read: its
        // result says why
        return if cancelled() {
            Err(TaskError::cancelled())
        } else {
            Ok(())
        };
    };
    for i in 0..count {
        if cancelled() {
            return Err(TaskError::cancelled());
        }
        let image = frame(i)?;
        if let Err(e) = w.write_all(&rgb24(&image)) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                // the encoder died: its result says why
                return Ok(());
            }
            return Err(TaskError::new(format!("feeding the encoder: {e}")));
        }
    }
    w.flush()
        .map_err(|e| TaskError::new(format!("feeding the encoder: {e}")))
}

#[cfg(unix)]
mod platform {
    use std::ffi::CString;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use videbimus_scheduler::TaskError;

    /// A named pipe at `path`.
    pub(super) fn make_pipe(path: &Path) -> Result<(), TaskError> {
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| TaskError::new(format!("{} has a NUL byte", path.display())))?;
        // SAFETY: `c` is a valid NUL-terminated path.
        if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
            let e = io::Error::last_os_error();
            return Err(TaskError::new(format!(
                "can't make a pipe at {}: {e}",
                path.display()
            )));
        }
        Ok(())
    }

    /// Opens the pipe for writing once the encoder opens it for reading.
    /// `None` if the encoder ends first, or the work is cancelled.
    pub(super) fn open_writer(
        path: &Path,
        done: &AtomicBool,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<File>, TaskError> {
        loop {
            match OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)
            {
                Ok(f) => {
                    let fd = f.as_raw_fd();
                    // SAFETY: fd is open; F_GETFL/F_SETFL only change its
                    // flags. Back to blocking writes.
                    unsafe {
                        let flags = libc::fcntl(fd, libc::F_GETFL);
                        libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
                    }
                    return Ok(Some(f));
                }
                Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {
                    if done.load(Ordering::SeqCst) || cancelled() {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => {
                    return Err(TaskError::new(format!(
                        "can't open the pipe {}: {e}",
                        path.display()
                    )));
                }
            }
        }
    }
}

#[cfg(not(unix))]
mod platform {
    //! No named pipes: encoding isn't supported (Unix first).

    use std::fs::File;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    use videbimus_scheduler::TaskError;

    pub(super) fn make_pipe(_path: &Path) -> Result<(), TaskError> {
        Ok(())
    }

    pub(super) fn open_writer(
        _path: &Path,
        _done: &AtomicBool,
        _cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<File>, TaskError> {
        Err(TaskError::new(
            "encoding the output needs named pipes, which this platform lacks",
        ))
    }
}
