//! Colour: sRGB in and out, and PNG files.
//!
//! An [`Image`] is premultiplied linear RGBA in `f32`. PNG files are sRGB
//! with straight (unpremultiplied) alpha, so reading one decodes the sRGB
//! transfer curve on R, G and B, leaves alpha as it is (alpha is already
//! linear), and premultiplies. [`write_png`] does the reverse, to 8-bit
//! sRGB, for tests and debugging; the encoder for the finished video is
//! ticket 0007's.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::sync::OnceLock;

use videbimus_core::Image;

/// The sRGB transfer curve, decoded: an sRGB value in `0..=1` to linear
/// light.
#[must_use]
pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// The sRGB transfer curve, encoded: linear light in `0..=1` to sRGB.
#[must_use]
pub fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb8_table() -> &'static [f32; 256] {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0.0; 256];
        for (i, v) in t.iter_mut().enumerate() {
            *v = srgb_to_linear(f32::from(u8::try_from(i).unwrap_or(u8::MAX)) / 255.0);
        }
        t
    })
}

/// A straight-alpha sRGB colour to premultiplied linear.
#[must_use]
pub fn premultiplied_from_srgb(rgba: [f32; 4]) -> [f32; 4] {
    let a = rgba[3];
    [
        srgb_to_linear(rgba[0]) * a,
        srgb_to_linear(rgba[1]) * a,
        srgb_to_linear(rgba[2]) * a,
        a,
    ]
}

/// Parses `#rrggbb` or `#rrggbbaa` (sRGB, straight alpha) to premultiplied
/// linear.
#[must_use]
pub fn parse_hex(s: &str) -> Option<[f32; 4]> {
    let hex = s.strip_prefix('#')?;
    if !(hex.len() == 6 || hex.len() == 8) || !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    let a = if hex.len() == 8 { byte(6)? } else { 255 };
    Some(premultiplied_from_srgb([
        f32::from(byte(0)?) / 255.0,
        f32::from(byte(2)?) / 255.0,
        f32::from(byte(4)?) / 255.0,
        f32::from(a) / 255.0,
    ]))
}

/// Reads a PNG into premultiplied linear RGBA. Any PNG colour type and bit
/// depth; R, G and B are taken to be sRGB.
///
/// # Errors
///
/// If the file can't be opened or isn't a PNG it can decode, as a message
/// naming the file.
pub fn read_png(path: &Path) -> Result<Image, String> {
    let fail = |e: &dyn std::fmt::Display| format!("can't read {}: {e}", path.display());
    let file = File::open(path).map_err(|e| fail(&e))?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| fail(&e))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| fail(&"the image is too large"))?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf).map_err(|e| fail(&e))?;
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err(fail(&"a palette survived expansion")),
    };
    let wide = info.bit_depth == png::BitDepth::Sixteen;
    let table = srgb8_table();
    let mut image = Image::new(info.width, info.height);
    let width = info.width as usize;
    for (y, row) in buf
        .chunks(info.line_size)
        .take(info.height as usize)
        .enumerate()
    {
        for x in 0..width {
            // each sample as 0..=1, and the colour ones linearised
            let sample = |c: usize| -> (f32, f32) {
                if wide {
                    let i = (x * channels + c) * 2;
                    let v = f32::from(u16::from_be_bytes([row[i], row[i + 1]])) / 65535.0;
                    (v, srgb_to_linear(v))
                } else {
                    let v = row[x * channels + c];
                    (f32::from(v) / 255.0, table[v as usize])
                }
            };
            let (rgb, a) = match channels {
                1 => ([sample(0).1; 3], 1.0),
                2 => ([sample(0).1; 3], sample(1).0),
                3 => ([sample(0).1, sample(1).1, sample(2).1], 1.0),
                _ => ([sample(0).1, sample(1).1, sample(2).1], sample(3).0),
            };
            image.pixels_mut()[y * width + x] = [rgb[0] * a, rgb[1] * a, rgb[2] * a, a];
        }
    }
    Ok(image)
}

/// The 8-bit straight-alpha sRGB bytes of an image, row by row, RGBA.
/// Values outside `0..=1` are clipped.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn to_srgb8(image: &Image) -> Vec<u8> {
    let mut out = Vec::with_capacity(image.pixels().len() * 4);
    for p in image.pixels() {
        let a = p[3].clamp(0.0, 1.0);
        for &c in &p[..3] {
            let straight = if a > 0.0 { c / a } else { 0.0 };
            let v = linear_to_srgb(straight.clamp(0.0, 1.0));
            out.push((v * 255.0).round() as u8);
        }
        out.push((a * 255.0).round() as u8);
    }
    out
}

/// Writes an image as an 8-bit sRGB PNG with straight alpha: for tests and
/// debugging (the video's own encoder is ticket 0007's).
///
/// # Errors
///
/// If the file can't be written, as a message naming it.
pub fn write_png(image: &Image, path: &Path) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("can't write {}: {e}", path.display());
    let file = File::create(path).map_err(|e| fail(&e))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), image.width(), image.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header().map_err(|e| fail(&e))?;
    writer
        .write_image_data(&to_srgb8(image))
        .map_err(|e| fail(&e))?;
    writer.finish().map_err(|e| fail(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transfer_curve_round_trips() {
        for i in 0..=255u8 {
            let v = f32::from(i) / 255.0;
            assert!((linear_to_srgb(srgb_to_linear(v)) - v).abs() < 1e-5);
        }
        // mid-grey in sRGB is about 21% linear light
        assert!((srgb_to_linear(0.5) - 0.214).abs() < 1e-3);
    }

    #[test]
    fn hex_colours() {
        assert_eq!(parse_hex("#ffffff"), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(parse_hex("#00000000"), Some([0.0; 4]));
        let half = parse_hex("#ff000080").unwrap();
        assert!((half[0] - half[3]).abs() < 1e-6, "premultiplied");
        assert_eq!(parse_hex("ffffff"), None);
        assert_eq!(parse_hex("#fff"), None);
    }

    #[test]
    fn png_round_trip_is_srgb_and_premultiplied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        let mut img = Image::new(2, 1);
        img.pixels_mut()[0] = [0.214_041_14, 0.0, 0.0, 1.0];
        img.pixels_mut()[1] = [0.25, 0.25, 0.25, 0.5];
        write_png(&img, &path).unwrap();
        let bytes = to_srgb8(&img);
        assert_eq!(&bytes[..4], &[128, 0, 0, 255]);
        assert_eq!(bytes[7], 128);
        let back = read_png(&path).unwrap();
        for (a, b) in back.pixels().iter().zip(img.pixels()) {
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() < 4e-3, "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn missing_files_name_themselves() {
        let e = read_png(Path::new("/nonexistent/x.png")).unwrap_err();
        assert!(e.contains("/nonexistent/x.png"), "{e}");
    }
}
