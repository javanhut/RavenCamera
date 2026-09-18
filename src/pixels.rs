//! Pixel work: camera formats to RGBA, the screen's BGRA to RGBA, resizing,
//! the picture-in-picture overlay, and RGBA to the YUV the encoder takes.
//!
//! All of it plain loops over byte slices, unit-tested without a camera or a
//! compositor. RGBA is 8-bit, straight alpha, top row first, tightly packed.

/// A decoded picture.
#[derive(Clone)]
pub struct Rgba {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Rgba {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rgba({}x{})", self.width, self.height)
    }
}

impl Rgba {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0; width * height * 4],
        }
    }

    /// Solid opaque black.
    pub fn black(width: usize, height: usize) -> Self {
        let mut data = vec![0; width * height * 4];
        for px in data.chunks_exact_mut(4) {
            px[3] = 255;
        }
        Self {
            width,
            height,
            data,
        }
    }

    /// Flip left to right, in place: the mirror a person expects to see
    /// themselves in.
    pub fn mirror(&mut self) {
        let w = self.width;
        for row in self.data.chunks_exact_mut(w * 4) {
            for x in 0..w / 2 {
                let (a, b) = (x * 4, (w - 1 - x) * 4);
                for c in 0..4 {
                    row.swap(a + c, b + c);
                }
            }
        }
    }
}

fn clamp(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// BT.601 limited-range YCbCr to RGB, which is what UVC cameras send
/// uncompressed. Fixed point with 8 bits of fraction.
fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let c = (i32::from(y) - 16).max(0) * 298;
    let d = i32::from(u) - 128;
    let e = i32::from(v) - 128;
    [
        clamp((c + 409 * e + 128) >> 8),
        clamp((c - 100 * d - 208 * e + 128) >> 8),
        clamp((c + 516 * d + 128) >> 8),
    ]
}

/// Packed YUYV 4:2:2, the uncompressed format every UVC camera offers.
pub fn yuyv_to_rgba(src: &[u8], width: usize, height: usize, stride: usize) -> Rgba {
    let mut out = Rgba::new(width, height);
    let stride = stride.max(width * 2);
    for row in 0..height {
        let Some(line) = src.get(row * stride..row * stride + width * 2) else {
            break;
        };
        let dst = &mut out.data[row * width * 4..(row + 1) * width * 4];
        for (pair, px) in line.chunks_exact(4).zip(dst.chunks_exact_mut(8)) {
            let [y0, u, y1, v] = [pair[0], pair[1], pair[2], pair[3]];
            let a = yuv_to_rgb(y0, u, v);
            let b = yuv_to_rgb(y1, u, v);
            px.copy_from_slice(&[a[0], a[1], a[2], 255, b[0], b[1], b[2], 255]);
        }
    }
    out
}

/// Planar Y with interleaved CbCr at half resolution.
pub fn nv12_to_rgba(src: &[u8], width: usize, height: usize, stride: usize) -> Rgba {
    let mut out = Rgba::new(width, height);
    let stride = stride.max(width);
    let uv_base = stride * height;
    for row in 0..height {
        for col in 0..width {
            let y = src.get(row * stride + col).copied().unwrap_or(16);
            let uv = uv_base + (row / 2) * stride + (col & !1);
            let u = src.get(uv).copied().unwrap_or(128);
            let v = src.get(uv + 1).copied().unwrap_or(128);
            let [r, g, b] = yuv_to_rgb(y, u, v);
            let at = (row * width + col) * 4;
            out.data[at..at + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }
    out
}

/// Decode a JPEG — a webcam's MJPEG frame or a photo — to RGBA. Webcams
/// often leave the Huffman tables out of each frame; the decoder supplies
/// the standard ones, as the MJPEG convention expects.
pub fn decode_jpeg(bytes: &[u8]) -> anyhow::Result<Rgba> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_strict_mode(false);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(bytes, options);
    let data = decoder
        .decode()
        .map_err(|e| anyhow::anyhow!("a camera frame did not decode: {e:?}"))?;
    let (width, height) = decoder
        .dimensions()
        .ok_or_else(|| anyhow::anyhow!("a camera frame with no size"))?;
    if data.len() != width * height * 4 {
        anyhow::bail!("a camera frame decoded to the wrong size");
    }
    Ok(Rgba {
        width,
        height,
        data,
    })
}

/// Encode RGBA as a JPEG at `quality` (1–100).
pub fn encode_jpeg(image: &Rgba, quality: u8) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    let encoder = jpeg_encoder::Encoder::new(&mut out, quality);
    encoder.encode(
        &image.data,
        u16::try_from(image.width)?,
        u16::try_from(image.height)?,
        jpeg_encoder::ColorType::Rgba,
    )?;
    Ok(out)
}

/// Encode RGBA as a PNG.
pub fn encode_png(image: &Rgba) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width as u32, image.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&image.data)?;
    }
    Ok(out)
}

/// Decode a PNG to RGBA (thumbnails read back from the cache).
pub fn decode_png(bytes: &[u8]) -> anyhow::Result<Rgba> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buf)?;
    let (w, h) = (info.width as usize, info.height as usize);
    let data = match info.color_type {
        png::ColorType::Rgba => buf[..w * h * 4].to_vec(),
        png::ColorType::Rgb => buf[..w * h * 3]
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf[..w * h * 2]
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buf[..w * h].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => anyhow::bail!("indexed PNG after EXPAND"),
    };
    Ok(Rgba {
        width: w,
        height: h,
        data,
    })
}

/// A frame from the compositor: `argb8888` in memory is B, G, R, A. Copied
/// out of the shared buffer as RGBA, with `stride` honoured and alpha forced
/// opaque (the screen has no transparency; `xrgb8888` leaves it undefined).
pub fn bgra_to_rgba(src: &[u8], width: usize, height: usize, stride: usize) -> Rgba {
    let mut out = Rgba::new(width, height);
    for row in 0..height {
        let Some(line) = src.get(row * stride..row * stride + width * 4) else {
            break;
        };
        let dst = &mut out.data[row * width * 4..(row + 1) * width * 4];
        for (s, d) in line.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
            d.copy_from_slice(&[s[2], s[1], s[0], 255]);
        }
    }
    out
}

/// Resize to `width`×`height`. Shrinking averages every source pixel a
/// destination pixel covers (a box filter, so fine text does not shimmer);
/// growing interpolates bilinearly.
pub fn resize(src: &Rgba, width: usize, height: usize) -> Rgba {
    if src.width == width && src.height == height {
        return src.clone();
    }
    if width == 0 || height == 0 || src.width == 0 || src.height == 0 {
        return Rgba::new(width, height);
    }
    if width <= src.width && height <= src.height {
        box_shrink(src, width, height)
    } else {
        bilinear(src, width, height)
    }
}

fn box_shrink(src: &Rgba, width: usize, height: usize) -> Rgba {
    let mut out = Rgba::new(width, height);
    // Source columns each destination column covers, computed once.
    let spans: Vec<(usize, usize)> = (0..width)
        .map(|x| {
            let a = x * src.width / width;
            let b = ((x + 1) * src.width / width).max(a + 1);
            (a, b)
        })
        .collect();
    let mut acc = vec![0u32; width * 4];
    for y in 0..height {
        let y0 = y * src.height / height;
        let y1 = ((y + 1) * src.height / height).max(y0 + 1);
        acc.iter_mut().for_each(|a| *a = 0);
        for sy in y0..y1 {
            let row = &src.data[sy * src.width * 4..(sy + 1) * src.width * 4];
            for (x, &(a, b)) in spans.iter().enumerate() {
                let slot = &mut acc[x * 4..x * 4 + 4];
                for px in row[a * 4..b * 4].chunks_exact(4) {
                    slot[0] += u32::from(px[0]);
                    slot[1] += u32::from(px[1]);
                    slot[2] += u32::from(px[2]);
                    slot[3] += u32::from(px[3]);
                }
            }
        }
        let dst = &mut out.data[y * width * 4..(y + 1) * width * 4];
        for (x, &(a, b)) in spans.iter().enumerate() {
            let n = ((b - a) * (y1 - y0)) as u32;
            for c in 0..4 {
                dst[x * 4 + c] = ((acc[x * 4 + c] + n / 2) / n) as u8;
            }
        }
    }
    out
}

fn bilinear(src: &Rgba, width: usize, height: usize) -> Rgba {
    let mut out = Rgba::new(width, height);
    let sx = src.width as f32 / width as f32;
    let sy = src.height as f32 / height as f32;
    for y in 0..height {
        let fy = ((y as f32 + 0.5) * sy - 0.5).max(0.0);
        let y0 = (fy as usize).min(src.height - 1);
        let y1 = (y0 + 1).min(src.height - 1);
        let wy = fy - y0 as f32;
        for x in 0..width {
            let fx = ((x as f32 + 0.5) * sx - 0.5).max(0.0);
            let x0 = (fx as usize).min(src.width - 1);
            let x1 = (x0 + 1).min(src.width - 1);
            let wx = fx - x0 as f32;
            let p =
                |xx: usize, yy: usize, c: usize| f32::from(src.data[(yy * src.width + xx) * 4 + c]);
            for c in 0..4 {
                let top = p(x0, y0, c) * (1.0 - wx) + p(x1, y0, c) * wx;
                let bottom = p(x0, y1, c) * (1.0 - wx) + p(x1, y1, c) * wx;
                out.data[(y * width + x) * 4 + c] = (top * (1.0 - wy) + bottom * wy + 0.5) as u8;
            }
        }
    }
    out
}

/// Scale `src` to fit inside `width`×`height` keeping its shape, centred on
/// black. A window that is resized during a recording goes through this, so
/// the video keeps one size and the window never looks stretched.
pub fn fit(src: &Rgba, width: usize, height: usize) -> Rgba {
    if src.width == width && src.height == height {
        return src.clone();
    }
    let (w, h) = fit_size(src.width, src.height, width, height);
    let scaled = resize(src, w, h);
    let mut out = Rgba::black(width, height);
    let (ox, oy) = ((width - w) / 2, (height - h) / 2);
    for row in 0..h {
        let d = ((oy + row) * width + ox) * 4;
        out.data[d..d + w * 4].copy_from_slice(&scaled.data[row * w * 4..(row + 1) * w * 4]);
    }
    out
}

/// The largest `w`×`h` with `src`'s aspect ratio that fits the box, at
/// least 1×1.
pub fn fit_size(sw: usize, sh: usize, bw: usize, bh: usize) -> (usize, usize) {
    if sw == 0 || sh == 0 {
        return (bw.max(1), bh.max(1));
    }
    if sw * bh > sh * bw {
        (bw.max(1), (sh * bw / sw).max(1))
    } else {
        ((sw * bh / sh).max(1), bh.max(1))
    }
}

/// The size a video is finished at: the recorded size, shrunk so its shorter
/// side is at most `limit`, and rounded down to even (H.264 4:2:0 needs even
/// dimensions).
pub fn output_size(width: usize, height: usize, limit: Option<u32>) -> (usize, usize) {
    let (mut w, mut h) = (width, height);
    if let Some(limit) = limit.map(|l| l as usize) {
        let short = w.min(h);
        if short > limit {
            w = w * limit / short;
            h = h * limit / short;
        }
    }
    ((w & !1).max(2), (h & !1).max(2))
}

/// Draw `src` onto `dst` at (`x`, `y`) with rounded corners of `radius`,
/// antialiased, and a thin light ring so the overlay reads against any
/// background.
pub fn overlay_rounded(dst: &mut Rgba, src: &Rgba, x: usize, y: usize, radius: f32) {
    let ring = (src.width.min(src.height) as f32 / 90.0).clamp(1.0, 3.0);
    for sy in 0..src.height {
        let dy = y + sy;
        if dy >= dst.height {
            break;
        }
        for sx in 0..src.width {
            let dx = x + sx;
            if dx >= dst.width {
                break;
            }
            // Distance outside the rounded rectangle's inset corner arc.
            let cx = (sx as f32 + 0.5).clamp(radius, src.width as f32 - radius);
            let cy = (sy as f32 + 0.5).clamp(radius, src.height as f32 - radius);
            let dist = ((sx as f32 + 0.5 - cx).powi(2) + (sy as f32 + 0.5 - cy).powi(2)).sqrt();
            let coverage = (radius + 0.5 - dist).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            // Distance to the nearest straight edge, for the ring.
            let edge = [
                sx as f32 + 0.5,
                sy as f32 + 0.5,
                src.width as f32 - sx as f32 - 0.5,
                src.height as f32 - sy as f32 - 0.5,
            ]
            .into_iter()
            .fold(f32::MAX, f32::min)
            .min(radius - dist + 0.5);
            let s = (sy * src.width + sx) * 4;
            let mut px = [
                f32::from(src.data[s]),
                f32::from(src.data[s + 1]),
                f32::from(src.data[s + 2]),
            ];
            if edge < ring {
                let t = 0.55;
                for c in &mut px {
                    *c = *c * (1.0 - t) + 255.0 * t;
                }
            }
            let d = (dy * dst.width + dx) * 4;
            for (c, value) in px.iter().enumerate() {
                let under = f32::from(dst.data[d + c]);
                dst.data[d + c] = (under * (1.0 - coverage) + value * coverage + 0.5) as u8;
            }
            dst.data[d + 3] = 255;
        }
    }
}

/// Where a picture-in-picture of `ow`×`oh` goes in a `w`×`h` frame, inset
/// from the corner by a margin proportional to the frame.
pub fn corner_position(
    corner: crate::settings::Corner,
    w: usize,
    h: usize,
    ow: usize,
    oh: usize,
) -> (usize, usize) {
    use crate::settings::Corner;
    let margin = (w.min(h) / 40).max(8);
    let right = w.saturating_sub(ow + margin);
    let bottom = h.saturating_sub(oh + margin);
    match corner {
        Corner::TopLeft => (margin, margin),
        Corner::TopRight => (right, margin),
        Corner::BottomLeft => (margin, bottom),
        Corner::BottomRight => (right, bottom),
    }
}

/// `219/255 × (Kr, 1 − Kr − Kb, Kb)` for BT.709, scaled by 2^16 — the same
/// conversion `raven-export` uses, so a screen recording finished by either
/// comes out the same colour.
const Y709: [i32; 3] = [11966, 40254, 4064];
const CB709: [i32; 3] = [-6597, -22187, 28784];
const CR709: [i32; 3] = [28784, -26148, -2636];

/// A YUV 4:2:0 picture: what the H.264 encoder takes.
#[derive(Debug)]
pub struct I420 {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
}

impl I420 {
    /// For frames of even `width`×`height`.
    pub fn new(width: usize, height: usize) -> Self {
        debug_assert!(width.is_multiple_of(2) && height.is_multiple_of(2));
        Self {
            width,
            height,
            y: vec![16; width * height],
            cb: vec![128; width / 2 * height / 2],
            cr: vec![128; width / 2 * height / 2],
        }
    }

    /// Convert `rgba`, which must be this picture's size. BT.709 limited
    /// range; chroma is the average of each 2×2 block.
    pub fn fill(&mut self, rgba: &Rgba) {
        debug_assert_eq!((rgba.width, rgba.height), (self.width, self.height));
        let w = self.width;
        let px = |x: usize, y: usize| {
            let at = (y * w + x) * 4;
            [
                i32::from(rgba.data[at]),
                i32::from(rgba.data[at + 1]),
                i32::from(rgba.data[at + 2]),
            ]
        };
        let dot = |k: &[i32; 3], p: [i32; 3]| k[0] * p[0] + k[1] * p[1] + k[2] * p[2];
        for (i, luma) in self.y.iter_mut().enumerate() {
            let at = i * 4;
            let p = [
                i32::from(rgba.data[at]),
                i32::from(rgba.data[at + 1]),
                i32::from(rgba.data[at + 2]),
            ];
            *luma = (16 + ((dot(&Y709, p) + 32_768) >> 16)) as u8;
        }
        let cw = w / 2;
        for row in 0..self.height / 2 {
            for col in 0..cw {
                let (mut cb, mut cr) = (0, 0);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let p = px(col * 2 + dx, row * 2 + dy);
                    cb += dot(&CB709, p);
                    cr += dot(&CR709, p);
                }
                self.cb[row * cw + col] = (128 + ((cb + 131_072) >> 18)) as u8;
                self.cr[row * cw + col] = (128 + ((cr + 131_072) >> 18)) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, rgba: [u8; 4]) -> Rgba {
        Rgba {
            width: w,
            height: h,
            data: rgba.repeat(w * h),
        }
    }

    #[test]
    fn yuyv_grey_and_white() {
        // Two pixels of mid grey (Y 126), then two of white (Y 235).
        let src = [126, 128, 126, 128, 235, 128, 235, 128];
        let out = yuyv_to_rgba(&src, 4, 1, 8);
        assert_eq!(&out.data[0..4], &[128, 128, 128, 255]);
        assert_eq!(&out.data[8..12], &[255, 255, 255, 255]);
    }

    #[test]
    fn bgra_is_swapped_and_opaque() {
        let src = [1, 2, 3, 0, 9, 9, 9, 9 /* stride padding */];
        let out = bgra_to_rgba(&src, 1, 1, 8);
        assert_eq!(out.data, [3, 2, 1, 255]);
    }

    #[test]
    fn shrinking_averages() {
        let mut src = solid(4, 2, [0, 0, 0, 255]);
        // Left half white.
        for y in 0..2 {
            for x in 0..2 {
                let at = (y * 4 + x) * 4;
                src.data[at..at + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
        let out = resize(&src, 2, 1);
        assert_eq!(&out.data[0..4], &[255, 255, 255, 255]);
        assert_eq!(&out.data[4..8], &[0, 0, 0, 255]);
        let half = resize(&src, 1, 1);
        assert_eq!(half.data[0], 128);
    }

    #[test]
    fn growing_keeps_solid_colour() {
        let out = resize(&solid(2, 2, [10, 20, 30, 255]), 5, 3);
        assert!(out.data.chunks_exact(4).all(|p| p == [10, 20, 30, 255]));
    }

    #[test]
    fn fitting_letterboxes() {
        assert_eq!(fit_size(1920, 1080, 1280, 1280), (1280, 720));
        assert_eq!(fit_size(1080, 1920, 1280, 720), (405, 720));
        let out = fit(&solid(2, 1, [255, 255, 255, 255]), 4, 4);
        // Top row is the letterbox.
        assert_eq!(&out.data[0..4], &[0, 0, 0, 255]);
        // Row 1, column 1 is the picture.
        let at = (4 + 1) * 4;
        assert_eq!(&out.data[at..at + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn output_size_limits_the_short_side_and_stays_even() {
        assert_eq!(output_size(1920, 1080, None), (1920, 1080));
        assert_eq!(output_size(1921, 1081, None), (1920, 1080));
        assert_eq!(output_size(2560, 1440, Some(1080)), (1920, 1080));
        assert_eq!(output_size(1920, 1080, Some(720)), (1280, 720));
        assert_eq!(output_size(1280, 720, Some(1080)), (1280, 720));
        // Portrait: the short side is the width.
        assert_eq!(output_size(1080, 1920, Some(720)), (720, 1280));
    }

    #[test]
    fn mirror_flips_rows() {
        let mut img = Rgba {
            width: 3,
            height: 1,
            data: vec![1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3],
        };
        img.mirror();
        assert_eq!(img.data, vec![3, 3, 3, 3, 2, 2, 2, 2, 1, 1, 1, 1]);
    }

    #[test]
    fn i420_matches_raven_export() {
        let mut pic = I420::new(2, 2);
        pic.fill(&solid(2, 2, [255, 0, 0, 255]));
        assert_eq!((pic.y[0], pic.cb[0], pic.cr[0]), (63, 102, 240));
        pic.fill(&solid(2, 2, [255, 255, 255, 255]));
        assert_eq!((pic.y[0], pic.cb[0], pic.cr[0]), (235, 128, 128));
    }

    #[test]
    fn overlay_corners_are_rounded_and_centre_is_covered() {
        let mut dst = solid(40, 40, [0, 0, 0, 255]);
        let src = solid(20, 20, [200, 0, 0, 255]);
        overlay_rounded(&mut dst, &src, 10, 10, 6.0);
        // The overlay's own corner stays background.
        assert_eq!(
            &dst.data[(10 * 40 + 10) * 4..(10 * 40 + 10) * 4 + 3],
            &[0, 0, 0]
        );
        // Its centre is the overlay.
        let c = (20 * 40 + 20) * 4;
        assert_eq!(dst.data[c], 200);
    }

    #[test]
    fn png_round_trips() {
        let img = solid(3, 2, [1, 2, 3, 255]);
        let back = decode_png(&encode_png(&img).unwrap()).unwrap();
        assert_eq!((back.width, back.height), (3, 2));
        assert_eq!(back.data, img.data);
    }

    #[test]
    fn jpeg_round_trips_close_enough() {
        let img = solid(16, 16, [120, 60, 200, 255]);
        let back = decode_jpeg(&encode_jpeg(&img, 95).unwrap()).unwrap();
        assert_eq!((back.width, back.height), (16, 16));
        for (a, b) in back.data.iter().zip(&img.data) {
            assert!((i32::from(*a) - i32::from(*b)).abs() <= 4, "{a} vs {b}");
        }
    }
}
