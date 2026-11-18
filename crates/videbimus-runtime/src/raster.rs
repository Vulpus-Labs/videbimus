//! Pixel work shared by the kinds: blending, drawing a picture fitted into
//! the frame, and the labelled placeholder for a missing frame.
//!
//! Everything here is premultiplied linear RGBA, as [`Image`] is.

use videbimus_core::Image;

/// How a layer combines with what is under it, in premultiplied linear
/// light. `s` is the layer's pixel scaled by its opacity, `d` what is
/// under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Blend {
    /// Over: `s + d·(1 − sₐ)`.
    #[default]
    Normal,
    /// Light adds: `s + d`, alpha capped at 1.
    Add,
    /// Multiply where both are opaque, over elsewhere:
    /// `s·d + s·(1 − dₐ) + d·(1 − sₐ)`.
    Multiply,
}

impl Blend {
    /// The names config uses, in order.
    pub const NAMES: [&'static str; 3] = ["normal", "add", "multiply"];

    /// The mode called `name` in config.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "normal" => Some(Self::Normal),
            "add" => Some(Self::Add),
            "multiply" => Some(Self::Multiply),
            _ => None,
        }
    }

    /// One pixel: `src` (already scaled by opacity) onto `dst`.
    #[must_use]
    pub fn pixel(self, src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
        let (sa, da) = (src[3], dst[3]);
        match self {
            Self::Normal => std::array::from_fn(|c| src[c] + dst[c] * (1.0 - sa)),
            Self::Add => {
                let mut o: [f32; 4] = std::array::from_fn(|c| src[c] + dst[c]);
                o[3] = o[3].min(1.0);
                o
            }
            Self::Multiply => {
                let mut o: [f32; 4] = std::array::from_fn(|c| {
                    src[c] * dst[c] + src[c] * (1.0 - da) + dst[c] * (1.0 - sa)
                });
                o[3] = sa + da - sa * da;
                o
            }
        }
    }
}

/// Layers `src` onto `dst` at `opacity` with `blend`. Both are the same
/// size.
///
/// # Panics
///
/// If they aren't.
pub fn composite(dst: &mut Image, src: &Image, opacity: f32, blend: Blend) {
    assert_eq!(
        (dst.width(), dst.height()),
        (src.width(), src.height()),
        "layers are the size of the frame"
    );
    if opacity <= 0.0 {
        return;
    }
    for (d, s) in dst.pixels_mut().iter_mut().zip(src.pixels()) {
        let s = s.map(|c| c * opacity);
        *d = blend.pixel(s, *d);
    }
}

/// How a picture is sized to the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Fit {
    /// As large as fits whole, keeping its shape.
    #[default]
    Contain,
    /// As small as covers the frame, keeping its shape; the rest is cut.
    Cover,
    /// Stretched to the frame.
    Fill,
    /// One picture pixel to one frame pixel.
    Native,
}

impl Fit {
    /// The names config uses, in order.
    pub const NAMES: [&'static str; 4] = ["contain", "cover", "fill", "native"];

    /// The fit called `name` in config.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "contain" => Some(Self::Contain),
            "cover" => Some(Self::Cover),
            "fill" => Some(Self::Fill),
            "native" => Some(Self::Native),
            _ => None,
        }
    }
}

/// Where a picture goes in the frame: sized by `fit` then `scale`, with its
/// centre at (`x`, `y`) as fractions of the frame (0.5, 0.5 is the middle).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// How it is sized to the frame.
    pub fit: Fit,
    /// A further scale on top of the fit.
    pub scale: f32,
    /// Its centre, across, as a fraction of the frame width.
    pub x: f32,
    /// Its centre, down, as a fraction of the frame height.
    pub y: f32,
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            fit: Fit::Contain,
            scale: 1.0,
            x: 0.5,
            y: 0.5,
        }
    }
}

/// Draws `src` over `dst` (normal blend) at `opacity`, placed by
/// `placement`. Magnified pictures are sampled bilinearly; reduced ones
/// average a grid of bilinear samples per pixel, so detail doesn't alias.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn draw(dst: &mut Image, src: &Image, placement: Placement, opacity: f32) {
    if src.width() == 0 || src.height() == 0 || opacity <= 0.0 {
        return;
    }
    let (fw, fh) = (dst.width() as f32, dst.height() as f32);
    let (sw, sh) = (src.width() as f32, src.height() as f32);
    let (kx, ky) = match placement.fit {
        Fit::Contain => {
            let k = (fw / sw).min(fh / sh);
            (k, k)
        }
        Fit::Cover => {
            let k = (fw / sw).max(fh / sh);
            (k, k)
        }
        Fit::Fill => (fw / sw, fh / sh),
        Fit::Native => (1.0, 1.0),
    };
    let (kx, ky) = (kx * placement.scale, ky * placement.scale);
    if kx <= 0.0 || ky <= 0.0 {
        return;
    }
    let (w, h) = (sw * kx, sh * ky);
    let left = placement.x * fw - w / 2.0;
    let top = placement.y * fh - h / 2.0;
    // frame pixels whose centres fall inside the picture
    let x0 = (left - 0.5).ceil().max(0.0) as u32;
    let y0 = (top - 0.5).ceil().max(0.0) as u32;
    let x1 = ((left + w - 0.5).ceil().max(0.0) as u32).min(dst.width());
    let y1 = ((top + h - 0.5).ceil().max(0.0) as u32).min(dst.height());
    // samples per pixel, each way, when reducing
    let nx = (1.0 / kx).ceil().clamp(1.0, 8.0) as u32;
    let ny = (1.0 / ky).ceil().clamp(1.0, 8.0) as u32;
    let weight = opacity / (nx * ny) as f32;
    let width = dst.width() as usize;
    for y in y0..y1 {
        for x in x0..x1 {
            let mut acc = [0.0f32; 4];
            for j in 0..ny {
                for i in 0..nx {
                    let px = x as f32 + (i as f32 + 0.5) / nx as f32;
                    let py = y as f32 + (j as f32 + 0.5) / ny as f32;
                    let s = bilinear(src, (px - left) / kx - 0.5, (py - top) / ky - 0.5);
                    for c in 0..4 {
                        acc[c] += s[c];
                    }
                }
            }
            let d = &mut dst.pixels_mut()[y as usize * width + x as usize];
            *d = Blend::Normal.pixel(acc.map(|c| c * weight), *d);
        }
    }
}

/// `src` at continuous pixel coordinates (pixel centres at whole numbers),
/// clamped to its edges.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::similar_names
)]
fn bilinear(src: &Image, u: f32, v: f32) -> [f32; 4] {
    let maxx = src.width() as f32 - 1.0;
    let maxy = src.height() as f32 - 1.0;
    let (u, v) = (u.clamp(0.0, maxx), v.clamp(0.0, maxy));
    let (xa, ya) = (u.floor(), v.floor());
    let (fx, fy) = (u - xa, v - ya);
    let (xa, ya) = (xa as usize, ya as usize);
    let xb = (xa + 1).min(src.width() as usize - 1);
    let yb = (ya + 1).min(src.height() as usize - 1);
    let w = src.width() as usize;
    let p = src.pixels();
    let (a, b, c, d) = (
        p[ya * w + xa],
        p[ya * w + xb],
        p[yb * w + xa],
        p[yb * w + xb],
    );
    std::array::from_fn(|k| {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bottom = c[k] + (d[k] - c[k]) * fx;
        top + (bottom - top) * fy
    })
}

/// A 3×5 bitmap font for placeholder labels: rows top to bottom, three
/// bits each, the high bit on the left. Characters it lacks are blank.
fn glyph(c: char) -> [u8; 5] {
    match c.to_ascii_uppercase() {
        '0' => [7, 5, 5, 5, 7],
        '1' => [2, 6, 2, 2, 7],
        '2' => [7, 1, 7, 4, 7],
        '3' => [7, 1, 3, 1, 7],
        '4' => [5, 5, 7, 1, 1],
        '5' => [7, 4, 7, 1, 7],
        '6' => [7, 4, 7, 5, 7],
        '7' => [7, 1, 1, 2, 2],
        '8' => [7, 5, 7, 5, 7],
        '9' => [7, 5, 7, 1, 7],
        'A' => [2, 5, 7, 5, 5],
        'B' => [6, 5, 6, 5, 6],
        'C' => [7, 4, 4, 4, 7],
        'D' => [6, 5, 5, 5, 6],
        'E' => [7, 4, 6, 4, 7],
        'F' => [7, 4, 6, 4, 4],
        'G' => [7, 4, 5, 5, 7],
        'H' => [5, 5, 7, 5, 5],
        'I' => [7, 2, 2, 2, 7],
        'K' => [5, 5, 6, 5, 5],
        'L' => [4, 4, 4, 4, 7],
        'M' => [5, 7, 7, 5, 5],
        'N' => [6, 5, 5, 5, 5],
        'O' => [2, 5, 5, 5, 2],
        'P' => [6, 5, 6, 4, 4],
        'R' => [6, 5, 6, 5, 5],
        'S' => [3, 4, 2, 1, 6],
        'T' => [7, 2, 2, 2, 2],
        'U' => [5, 5, 5, 5, 7],
        'Y' => [5, 5, 2, 2, 2],
        '-' => [0, 0, 7, 0, 0],
        ':' => [0, 2, 0, 2, 0],
        '.' => [0, 0, 0, 0, 2],
        '/' => [1, 1, 2, 4, 4],
        _ => [0; 5],
    }
}

/// Fills `out` with the placeholder for a frame that can't be shown: a
/// dark checkerboard in magenta and grey, with `label` written across the
/// middle in white block capitals. A placeholder is never mistaken for
/// footage, and says why it is there (ADR 0008 §5).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn placeholder(out: &mut Image, label: &str) {
    let (w, h) = (out.width() as usize, out.height() as usize);
    let cell = (h / 6).max(1);
    for y in 0..h {
        for x in 0..w {
            let c = if (x / cell + y / cell) % 2 == 0 {
                [0.12, 0.0, 0.12, 1.0]
            } else {
                [0.05, 0.05, 0.05, 1.0]
            };
            out.pixels_mut()[y * w + x] = c;
        }
    }
    let chars: Vec<char> = label.chars().collect();
    if chars.is_empty() {
        return;
    }
    // each character is 4 cells wide with its gap; fit 90% of the width
    let cells = chars.len() * 4 - 1;
    let px = ((w * 9 / 10) / cells).min(h / 8).max(1);
    let left = w.saturating_sub(cells * px) / 2;
    let top = h.saturating_sub(5 * px) / 2;
    for (i, &c) in chars.iter().enumerate() {
        for (row, bits) in glyph(c).iter().enumerate() {
            for col in 0..3 {
                if bits & (4 >> col) == 0 {
                    continue;
                }
                for dy in 0..px {
                    for dx in 0..px {
                        let x = left + (i * 4 + col) * px + dx;
                        let y = top + row * px + dy;
                        if x < w && y < h {
                            out.pixels_mut()[y * w + x] = [1.0; 4];
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn blend_modes() {
        let red = [1.0, 0.0, 0.0, 1.0];
        let grey = [0.5, 0.5, 0.5, 1.0];
        let half_red = [0.5, 0.0, 0.0, 0.5];
        assert!(close(
            Blend::Normal.pixel(half_red, grey),
            [0.75, 0.25, 0.25, 1.0]
        ));
        assert!(close(Blend::Add.pixel(red, grey), [1.5, 0.5, 0.5, 1.0]));
        assert!(close(
            Blend::Multiply.pixel(red, grey),
            [0.5, 0.0, 0.0, 1.0]
        ));
        // onto nothing, every mode leaves the layer as it is
        for b in [Blend::Normal, Blend::Add, Blend::Multiply] {
            assert!(close(b.pixel(half_red, [0.0; 4]), half_red), "{b:?}");
        }
    }

    #[test]
    fn contain_letterboxes_and_cover_crops() {
        let mut src = Image::new(2, 1);
        src.fill([1.0; 4]);
        let mut dst = Image::new(4, 4);
        draw(&mut dst, &src, Placement::default(), 1.0);
        // 2:1 into 4×4: rows 1 and 2 covered, 0 and 3 left alone
        let row = |img: &Image, y: usize| img.pixels()[y * 4][3];
        assert_eq!(
            [row(&dst, 0), row(&dst, 1), row(&dst, 2), row(&dst, 3)],
            [0.0, 1.0, 1.0, 0.0]
        );
        let mut dst = Image::new(4, 4);
        let cover = Placement {
            fit: Fit::Cover,
            ..Placement::default()
        };
        draw(&mut dst, &src, cover, 1.0);
        assert!(dst.pixels().iter().all(|p| p[3] == 1.0));
    }

    #[test]
    fn reduction_averages() {
        // a 1-pixel checkerboard reduced 4× is mid-grey, not aliased
        let mut src = Image::new(16, 16);
        for (i, p) in src.pixels_mut().iter_mut().enumerate() {
            let v = if (i % 16 + i / 16) % 2 == 0 { 1.0 } else { 0.0 };
            *p = [v, v, v, 1.0];
        }
        let mut dst = Image::new(4, 4);
        draw(&mut dst, &src, Placement::default(), 1.0);
        for p in dst.pixels() {
            assert!((p[0] - 0.5).abs() < 0.05, "{p:?}");
        }
    }

    #[test]
    fn placeholders_are_labelled() {
        let mut out = Image::new(64, 36);
        placeholder(&mut out, "MISSING 42");
        assert!(out.pixels().contains(&[1.0; 4]), "the label");
        assert!(out.pixels().iter().all(|p| p[3] == 1.0), "opaque");
    }
}
