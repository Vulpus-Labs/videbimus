//! `caption`: lines of text.

use std::fmt;

use fontdue::{Font, FontSettings};
use videbimus_config::{Error, Node, Resolver};
use videbimus_core::{Component, Context, Frame, Image, Interval};

use crate::color::parse_hex;
use crate::curve::{Envelope, read_name};
use crate::raster::{Blend, composite};
use crate::registry::{Builder, ComponentKind, path_prop};

use super::no_children;

/// How lines line up with the caption's `x`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    /// Lines start at `x`.
    Left,
    /// Lines are centred on `x`.
    #[default]
    Center,
    /// Lines end at `x`.
    Right,
}

/// Lines of text at a position, fading in and out (ADR 0002 §5). The text
/// is rasterised once (with `fontdue`, from the font file named in config)
/// and drawn each frame at the opacity its envelope gives. Pure: horizon 0.
///
/// ```kdl
/// caption "title" from=(s)2 to=(s)6 text="Fenland\nvxn-1c" \
///     font="fonts/Jost-500-Medium.ttf" size=48 x=0.5 y=0.8 align="center" \
///     color="#ffffff" line-height=1.2 in=(s)0.5 out=(s)0.5 ease="smooth"
/// ```
///
/// - `text`: the lines, split at `\n`.
/// - `font`: a TrueType or OpenType file, relative to the config file.
/// - `size`: the font size in pixels (default 32).
/// - `x`, `y`: as fractions of the frame, where the lines align across
///   (by `align`: `left`, `center` or `right`) and the block's middle
///   down (defaults 0.5, 0.5).
/// - `color`: sRGB `"#rrggbb"` or `"#rrggbbaa"` (default white).
/// - `line-height`: line spacing as a multiple of the font's (default 1).
/// - `in`, `out`, `ease`: opacity ramps at its start and end.
pub struct Caption {
    interval: Interval,
    lines: Vec<String>,
    font: Font,
    size: f32,
    x: f32,
    y: f32,
    align: Align,
    color: [f32; 4],
    line_height: f32,
    opacity: Envelope,
    text: Option<Image>,
}

impl fmt::Debug for Caption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Caption")
            .field("interval", &self.interval)
            .field("lines", &self.lines)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Caption {
    /// Draws the text, fully opaque, into a transparent image of `w` × `h`.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        clippy::cast_precision_loss,
        clippy::many_single_char_names
    )]
    fn rasterise(&self, w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        let (wi, hi) = (i64::from(w), i64::from(h));
        let lm = self.font.horizontal_line_metrics(self.size);
        let (ascent, step) = lm.map_or((self.size * 0.8, self.size), |m| {
            (m.ascent, m.new_line_size)
        });
        let step = step * self.line_height;
        let block = step * (self.lines.len().max(1) - 1) as f32 + ascent;
        let mut baseline = self.y * h as f32 - block / 2.0 + ascent;
        for line in &self.lines {
            let mut width = 0.0;
            let mut prev = None;
            for c in line.chars() {
                if let Some(p) = prev {
                    width += self.font.horizontal_kern(p, c, self.size).unwrap_or(0.0);
                }
                width += self.font.metrics(c, self.size).advance_width;
                prev = Some(c);
            }
            let mut pen = match self.align {
                Align::Left => self.x * w as f32,
                Align::Center => self.x * w as f32 - width / 2.0,
                Align::Right => self.x * w as f32 - width,
            };
            let mut prev = None;
            for c in line.chars() {
                if let Some(p) = prev {
                    pen += self.font.horizontal_kern(p, c, self.size).unwrap_or(0.0);
                }
                let (m, cov) = self.font.rasterize(c, self.size);
                let left = (pen.round() as i64) + i64::from(m.xmin);
                let top = baseline.round() as i64 - i64::from(m.ymin) - m.height as i64;
                for gy in 0..m.height {
                    for gx in 0..m.width {
                        let (x, y) = (left + gx as i64, top + gy as i64);
                        if x < 0 || y < 0 || x >= wi || y >= hi {
                            continue;
                        }
                        let a = f32::from(cov[gy * m.width + gx]) / 255.0;
                        if a == 0.0 {
                            continue;
                        }
                        let p = &mut img.pixels_mut()[(y * wi + x) as usize];
                        *p = Blend::Normal.pixel(self.color.map(|v| v * a), *p);
                    }
                }
                pen += m.advance_width;
                prev = Some(c);
            }
            baseline += step;
        }
        img
    }
}

impl Component for Caption {
    fn interval(&self) -> Interval {
        self.interval
    }

    fn reset(&mut self, _: &Context) {}

    fn render(&mut self, n: Frame, ctx: &Context, out: &mut Image) {
        let opacity = self.opacity.at(n, self.interval.len());
        if opacity <= 0.0 {
            return;
        }
        let fresh = self
            .text
            .as_ref()
            .is_some_and(|t| (t.width(), t.height()) == (ctx.width, ctx.height));
        if !fresh {
            self.text = Some(self.rasterise(ctx.width, ctx.height));
        }
        let text = self.text.as_ref().expect("made above");
        if (text.width(), text.height()) == (out.width(), out.height()) {
            composite(out, text, opacity, Blend::Normal);
        }
    }
}

impl ComponentKind for Caption {
    const KIND: &'static str = "caption";

    #[allow(clippy::cast_possible_truncation)]
    fn from_config(node: &Node, r: &Resolver, b: &Builder) -> Result<Box<dyn Component>, Error> {
        b.check_props(
            node,
            r,
            &[
                "text",
                "font",
                "size",
                "x",
                "y",
                "align",
                "color",
                "line-height",
                "in",
                "out",
                "ease",
            ],
        )?;
        no_children(node, r)?;
        let interval = b.interval(node, r)?;
        let at = |k: &str| crate::registry::origin_of(node, k);
        let text: String = r.get(node, "text")?;
        let Some(font_path) = path_prop(node, r, "font")? else {
            return Err(r
                .error(node, node.origin(), "the caption has no `font`")
                .help("add font=\"….ttf\", relative to this config file"));
        };
        let bytes = std::fs::read(&font_path).map_err(|e| {
            r.error(
                node,
                &at("font"),
                format!("can't read the font {}: {e}", font_path.display()),
            )
            .help("paths are relative to the config file they are written in")
        })?;
        let font = Font::from_bytes(bytes, FontSettings::default()).map_err(|e| {
            r.error(
                node,
                &at("font"),
                format!("{} isn't a font fontdue can read: {e}", font_path.display()),
            )
            .help("use a TrueType (.ttf) or OpenType (.otf) file")
        })?;
        let num = |k: &str, d: f64| -> Result<f32, Error> {
            Ok(r.opt::<f64>(node, k)?.unwrap_or(d) as f32)
        };
        let size = num("size", 32.0)?;
        if size <= 0.0 {
            return Err(r
                .error(node, &at("size"), "`size` must be more than 0")
                .help("size=32 is 32-pixel text"));
        }
        let align = match read_name(node, r, "align", &["left", "center", "right"])? {
            Some("left") => Align::Left,
            Some("right") => Align::Right,
            _ => Align::Center,
        };
        let color = match r.opt::<String>(node, "color")? {
            None => [1.0; 4],
            Some(s) => parse_hex(&s).ok_or_else(|| {
                r.error(node, &at("color"), format!("`{s}` isn't a colour"))
                    .help("write it as \"#rrggbb\" or \"#rrggbbaa\", in sRGB")
            })?,
        };
        Ok(Box::new(Caption {
            interval,
            lines: text.split('\n').map(str::to_owned).collect(),
            font,
            size,
            x: num("x", 0.5)?,
            y: num("y", 0.5)?,
            align,
            color,
            line_height: num("line-height", 1.0)?,
            opacity: Envelope::read(node, r, None, "in", "out")?,
            text: None,
        }))
    }
}
