//! Making a camera's picture look its best: the finishing a phone gives its
//! camera's pictures and a webcam does not.
//!
//! Each picture is measured first and corrected only as far as the
//! measurement says it is wrong, so a good camera's picture passes through
//! nearly as it came and a poor one's gets the help it needs:
//!
//! - **Noise**, measured on the flat parts of the picture (Immerkær's
//!   method). Video is denoised over time wherever nothing moved; a photo by
//!   merging several frames ([`stack`]). Where something moved, the newest
//!   frame is used as it is, so nothing leaves a trail.
//! - **Levels**, stretched so black is black and white is white, by at most
//!   about ×1.46 so a dark scene is not turned into noise.
//! - **Vibrance**: dull colour made richer, already vivid colour and skin
//!   left nearly alone, and none at all for a camera whose colour is rich.
//! - **Sharpening**, held back as the noise rises (sharpened noise is worse
//!   noise) and as the camera's own sharpening shows (twice-sharpened edges
//!   grow halos).
//!
//! All of it is integer loops over bands of rows, one band per core: a
//! Celeron finishes a 1080p frame in a few milliseconds.

use crate::pixels::Rgba;

/// How far each measurement of a video moves the ones in use towards it:
/// about a third of a second to follow a change of light at 30 fps.
const FOLLOW: f32 = 0.35;
/// Frames between measurements of a video.
const MEASURE_EVERY: u32 = 3;

/// Noise (σ, in 8-bit luma steps) up to which sharpening is at full
/// strength, and from which there is none.
const NOISE_SHARPEN_FULL: f32 = 1.2;
const NOISE_SHARPEN_NONE: f32 = 4.5;
/// Below this there is too little noise for denoising to be worth a pass.
const NOISE_DENOISE_FROM: f32 = 0.8;

/// Edge steepness ([`Measure::crisp`]) up to which the picture is soft and
/// sharpened fully, and from which the camera has sharpened it enough.
///
/// Measured on a 720p webcam at its own sharpness setting 0, 50 and 100:
/// 0.074, 0.110, 0.137. A hard synthetic edge reads about 0.24.
const CRISP_SOFT: f32 = 0.08;
const CRISP_SHARP: f32 = 0.15;
/// The share of fine detail added back at full strength, in 1/256ths.
const SHARPEN_MAX: f32 = 110.0;

/// Mean chroma (0–255) at or below which a picture is dull and gets full
/// vibrance, and from which it is rich enough to be left alone.
const COLOUR_DULL: f32 = 18.0;
const COLOUR_RICH: f32 = 45.0;
/// Vibrance at full strength: a grey-ish pixel's colour ×1.28, in 1/256ths.
const VIBRANCE_MAX: f32 = 72.0;

/// The most a previous video frame counts in a denoised one, in 1/256ths:
/// on a still scene this leaves noise at about half.
const TEMPORAL_MAX: i32 = 160;

/// What a picture needs, measured from the picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measure {
    /// The luma to become black and the luma to become white.
    pub lo: f32,
    pub hi: f32,
    /// Standard deviation of the sensor noise, in 8-bit luma steps.
    pub noise: f32,
    /// Mean chroma, the spread of R, G and B: 0 for grey, 255 for pure red.
    pub colour: f32,
    /// How steep the strongest edges are for their contrast (Laplacian over
    /// Sobel gradient). A soft lens reads low; a camera that sharpens hard
    /// reads high.
    pub crisp: f32,
}

impl Measure {
    fn follow(self, towards: Measure) -> Measure {
        let f = |a: f32, b: f32| a + (b - a) * FOLLOW;
        Measure {
            lo: f(self.lo, towards.lo),
            hi: f(self.hi, towards.hi),
            noise: f(self.noise, towards.noise),
            colour: f(self.colour, towards.colour),
            crisp: f(self.crisp, towards.crisp),
        }
    }
}

/// Finishes a video, frame after frame.
#[derive(Debug, Default)]
pub struct Enhancer {
    measure: Option<Measure>,
    /// Frames since the scene was last measured.
    since: u32,
    /// The last frame after denoising and before anything else, which the
    /// next is denoised against.
    previous: Option<Rgba>,
}

impl Enhancer {
    pub fn frame(&mut self, img: &mut Rgba) {
        // The scene is measured every third frame: the measurements move
        // slowly anyway, and it saves a Celeron a few milliseconds a frame.
        let m = match self.measure {
            Some(m) if self.since < MEASURE_EVERY => {
                self.since += 1;
                m
            }
            Some(m) => {
                self.since = 1;
                m.follow(measure(img))
            }
            None => {
                self.since = 1;
                measure(img)
            }
        };
        self.measure = Some(m);
        let mut noise = m.noise;
        match &mut self.previous {
            Some(prev) if prev.width == img.width && prev.height == img.height => {
                if temporal(img, prev, m.noise) {
                    // On the still parts, which is where noise shows.
                    noise *= 0.55;
                }
                prev.data.copy_from_slice(&img.data);
            }
            _ => self.previous = Some(img.clone()),
        }
        finish(img, &Measure { noise, ..m });
    }
}

/// Finish one photo, as [`Enhancer`] does a video frame.
pub fn photo(img: &mut Rgba) {
    let m = measure(img);
    finish(img, &m);
}

/// Merge a burst of frames of the same scene into one with less noise: each
/// pixel is the average of the frames that agree with the first there, so
/// what moved between frames is taken from the first alone. The frames must
/// all be one size; `None` if there are none.
pub fn stack(frames: &[Rgba]) -> Option<Rgba> {
    let (first, rest) = frames.split_first()?;
    let rest: Vec<&Rgba> = rest
        .iter()
        .filter(|f| f.width == first.width && f.height == first.height)
        .collect();
    if rest.is_empty() {
        return Some(first.clone());
    }
    let weights = agreement_weights(measure(first).noise, 256);
    let mut out = first.clone();
    let w = first.width;
    for_bands(&mut out, |first_row, band| {
        let at0 = first_row * w * 4;
        for (i, px) in band.chunks_exact_mut(4).enumerate() {
            let at = at0 + i * 4;
            let l0 = luma(px[0], px[1], px[2]);
            let mut sum = [
                i32::from(px[0]) * 256,
                i32::from(px[1]) * 256,
                i32::from(px[2]) * 256,
            ];
            let mut weight = 256;
            for f in &rest {
                let p = &f.data[at..at + 3];
                let wt = weights[(luma(p[0], p[1], p[2]) - l0).unsigned_abs() as usize];
                if wt > 0 {
                    for c in 0..3 {
                        sum[c] += i32::from(p[c]) * wt;
                    }
                    weight += wt;
                }
            }
            for c in 0..3 {
                px[c] = ((sum[c] + weight / 2) / weight) as u8;
            }
        }
    });
    Some(out)
}

/// The luma difference between frames past which they are taken to show
/// something different rather than the same thing with different noise:
/// three noise deviations, with a floor for a nearly noiseless camera.
fn agreement(noise: f32) -> i32 {
    (3.0 * noise + 3.0).round() as i32
}

/// How much another frame counts, by its luma difference: `max` when they
/// are the same, falling straight to nothing at [`agreement`].
fn agreement_weights(noise: f32, max: i32) -> [i32; 256] {
    let limit = agreement(noise);
    std::array::from_fn(|diff| {
        let diff = diff as i32;
        if diff < limit {
            max * (limit - diff) / limit
        } else {
            0
        }
    })
}

/// Blend `img` towards the previous frame wherever the two agree. `false`
/// if the picture is too clean to bother.
fn temporal(img: &mut Rgba, prev: &Rgba, noise: f32) -> bool {
    if noise < NOISE_DENOISE_FROM {
        return false;
    }
    let weights = agreement_weights(noise, TEMPORAL_MAX);
    let w = img.width;
    for_bands(img, |first_row, band| {
        let at0 = first_row * w * 4;
        for (i, px) in band.chunks_exact_mut(4).enumerate() {
            let at = at0 + i * 4;
            let p = &prev.data[at..at + 3];
            let diff = luma(px[0], px[1], px[2]) - luma(p[0], p[1], p[2]);
            let wt = weights[diff.unsigned_abs() as usize];
            if wt > 0 {
                for c in 0..3 {
                    let cur = i32::from(px[c]);
                    px[c] = (cur + (((i32::from(p[c]) - cur) * wt + 128) >> 8)) as u8;
                }
            }
        }
    });
    true
}

/// Measure what `img` needs, from about 40 000 points spread over it.
pub fn measure(img: &Rgba) -> Measure {
    let (w, h) = (img.width, img.height);
    let plain = Measure {
        lo: 0.0,
        hi: 255.0,
        noise: 0.0,
        colour: COLOUR_RICH,
        crisp: CRISP_SHARP,
    };
    if w < 3 || h < 3 {
        return plain;
    }
    let step = (((w * h) as f32 / 40_000.0).sqrt() as usize).max(1);
    let y = |x: usize, r: usize| {
        let at = (r * w + x) * 4;
        luma(img.data[at], img.data[at + 1], img.data[at + 2])
    };
    // The darkest channel of each point and the brightest: black is where
    // all three are low, and a bright red is as bright as its red, not its
    // luma, so stretching by luma would clip it.
    let mut darkest = [0u32; 256];
    let mut brightest = [0u32; 256];
    let mut chroma = 0u64;
    let mut count = 0u32;
    // (gradient, noise response) where noise can show: not crushed to
    // black or blown to white, where every point is the same.
    let mut unclipped: Vec<(i32, i32)> = Vec::with_capacity(45_000);
    // (gradient, Laplacian) everywhere, for crispness.
    let mut edges: Vec<(i32, i32)> = Vec::with_capacity(45_000);
    for r in (1..h - 1).step_by(step) {
        for x in (1..w - 1).step_by(step) {
            let [a, b, c] = [y(x - 1, r - 1), y(x, r - 1), y(x + 1, r - 1)];
            let [d, e, f] = [y(x - 1, r), y(x, r), y(x + 1, r)];
            let [g, i, j] = [y(x - 1, r + 1), y(x, r + 1), y(x + 1, r + 1)];
            let at = (r * w + x) * 4;
            let px = &img.data[at..at + 3];
            let hi = px[0].max(px[1]).max(px[2]);
            let lo = px[0].min(px[1]).min(px[2]);
            darkest[lo as usize] += 1;
            brightest[hi as usize] += 1;
            chroma += u64::from(hi - lo);
            count += 1;
            let gx = (c + 2 * f + j) - (a + 2 * d + g);
            let gy = (g + 2 * i + j) - (a + 2 * b + c);
            let grad = gx.abs() + gy.abs();
            let lap = 4 * e - (b + d + f + i);
            edges.push((grad, lap.abs()));
            let (min, max) = (
                a.min(b).min(c).min(d).min(e).min(f).min(g).min(i).min(j),
                a.max(b).max(c).max(d).max(e).max(f).max(g).max(i).max(j),
            );
            if min > 10 && max < 245 {
                // Immerkær's kernel: blind to flat areas, ramps and
                // straight edges, so what it sees on flat ground is noise.
                let n = (a + c + g + j) - 2 * (b + d + f + i) + 4 * e;
                unclipped.push((grad, n.abs()));
            }
        }
    }
    if count == 0 {
        return plain;
    }

    // Levels: the value below which 0.4% of the darkest channels lie and
    // above which 0.4% of the brightest do, held back so the stretch is at
    // most about ×1.46.
    let cut = (count / 250).max(1);
    let percentile = |hist: &[u32; 256], from_top: bool| {
        let mut acc = 0;
        for k in 0..256 {
            let k = if from_top { 255 - k } else { k };
            acc += hist[k];
            if acc >= cut {
                return k as f32;
            }
        }
        if from_top {
            255.0
        } else {
            0.0
        }
    };
    let lo = percentile(&darkest, false).min(40.0);
    let hi = percentile(&brightest, true).max(215.0);

    // Noise on the flattest quarter of what is not clipped, where neither
    // texture nor edges pass for noise.
    let noise = if unclipped.len() < 200 {
        0.0
    } else {
        let quarter = unclipped.len() / 4;
        unclipped.select_nth_unstable_by_key(quarter, |p| p.0);
        let flat = &unclipped[..quarter];
        let mean = flat.iter().map(|p| f64::from(p.1)).sum::<f64>() / flat.len() as f64;
        (std::f64::consts::FRAC_PI_2.sqrt() / 6.0 * mean) as f32
    };

    // Crispness on the strongest tenth, if they are edges at all.
    let tenth = edges.len() / 10;
    let crisp = if tenth < 50 {
        (CRISP_SOFT + CRISP_SHARP) / 2.0
    } else {
        let k = edges.len() - tenth;
        edges.select_nth_unstable_by_key(k, |p| p.0);
        let strong: Vec<&(i32, i32)> = edges[k..].iter().filter(|p| p.0 >= 48).collect();
        if strong.len() < 50 {
            // Nothing to judge by: neither hold back nor push.
            (CRISP_SOFT + CRISP_SHARP) / 2.0
        } else {
            let lap: i64 = strong.iter().map(|p| i64::from(p.1)).sum();
            let grad: i64 = strong.iter().map(|p| i64::from(p.0)).sum();
            lap as f32 / grad.max(1) as f32
        }
    };

    Measure {
        lo,
        hi,
        noise,
        colour: chroma as f32 / count as f32,
        crisp,
    }
}

/// 0 at `none`, 1 at `full`, straight between; either way round.
fn ramp(v: f32, full: f32, none: f32) -> f32 {
    ((v - none) / (full - none)).clamp(0.0, 1.0)
}

/// Levels, vibrance and sharpening, as far as `m` says.
fn finish(img: &mut Rgba, m: &Measure) {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 {
        return;
    }
    let span = (m.hi - m.lo).max(1.0);
    let lut: [u8; 256] =
        std::array::from_fn(|i| ((i as f32 - m.lo) * 255.0 / span).round().clamp(0.0, 255.0) as u8);
    let sharpen = (SHARPEN_MAX
        * ramp(m.noise, NOISE_SHARPEN_FULL, NOISE_SHARPEN_NONE)
        * ramp(m.crisp, CRISP_SOFT, CRISP_SHARP)) as i32;
    // Finer than this is noise, and is not sharpened.
    let threshold = (2.5 * m.noise).round().max(2.0) as i32;
    let vibrance = (VIBRANCE_MAX * ramp(m.colour, COLOUR_DULL, COLOUR_RICH)) as i32;
    let boosts: [i32; 256] = std::array::from_fn(|spread| vibrance * (255 - spread as i32) / 255);

    // Levels, and the luma sharpening looks at.
    let mut y = vec![0u8; w * h];
    {
        let band = band_rows(h);
        std::thread::scope(|s| {
            for (data, ys) in img
                .data
                .chunks_mut(band * w * 4)
                .zip(y.chunks_mut(band * w))
            {
                s.spawn(|| {
                    for (px, l) in data.chunks_exact_mut(4).zip(ys) {
                        let [r, g, b] = [
                            lut[px[0] as usize],
                            lut[px[1] as usize],
                            lut[px[2] as usize],
                        ];
                        px[0] = r;
                        px[1] = g;
                        px[2] = b;
                        *l = luma(r, g, b) as u8;
                    }
                });
            }
        });
    }
    if sharpen == 0 && vibrance == 0 {
        return;
    }
    let y = &y;
    for_bands(img, |first_row, band| {
        for (r, line) in band.chunks_exact_mut(w * 4).enumerate() {
            let row = first_row + r;
            let cur = &y[row * w..(row + 1) * w];
            let inner = row > 0 && row + 1 < h;
            let (up, down) = if inner {
                (&y[(row - 1) * w..row * w], &y[(row + 1) * w..(row + 2) * w])
            } else {
                (cur, cur)
            };
            for (col, px) in line.chunks_exact_mut(4).enumerate() {
                let l = i32::from(cur[col]);
                // Unsharp mask on luma alone, added equally to R, G and B
                // so edges get crisper without coloured fringes.
                let mut detail = 0;
                if sharpen > 0 && inner && col > 0 && col + 1 < w {
                    let around = i32::from(cur[col - 1])
                        + i32::from(cur[col + 1])
                        + i32::from(up[col])
                        + i32::from(down[col]);
                    let d = l - (around + 2) / 4;
                    if d.abs() > threshold {
                        detail = ((d - d.signum() * threshold) * sharpen) >> 8;
                    }
                }
                let boost = if vibrance > 0 {
                    vibrance_for(px[0], px[1], px[2], &boosts)
                } else {
                    0
                };
                for c in &mut px[..3] {
                    let v = i32::from(*c);
                    *c = clamp(l + (((v - l) * (256 + boost)) >> 8) + detail);
                }
            }
        }
    });
}

/// The saturation boost for one pixel, in 1/256ths, from `boosts` by how
/// saturated it is already: most for a nearly grey one, none for a fully
/// saturated one, and little for skin, whose colour looks wrong the moment
/// it is pushed.
fn vibrance_for(r: u8, g: u8, b: u8, boosts: &[i32; 256]) -> i32 {
    let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
    let spread = i32::from(hi - lo);
    let boost = boosts[spread as usize];
    // Skin, whatever its depth: red over green over blue, with a hue of
    // 6°–48°, between red-orange and yellow-orange.
    if r >= g && g >= b && spread > 12 {
        let gb = 60 * i32::from(g - b);
        if gb >= 6 * spread && gb <= 48 * spread {
            return boost / 4;
        }
    }
    boost
}

fn luma(r: u8, g: u8, b: u8) -> i32 {
    ((77 * u32::from(r) + 150 * u32::from(g) + 29 * u32::from(b) + 128) >> 8) as i32
}

fn clamp(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Rows per band: one band per core, and not so thin that starting a
/// thread costs more than the band.
fn band_rows(h: usize) -> usize {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
    h.div_ceil(threads).max(32)
}

/// Run `f` on bands of `img`'s rows at once, with each band's first row.
fn for_bands(img: &mut Rgba, f: impl Fn(usize, &mut [u8]) + Sync) {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 {
        return;
    }
    let band = band_rows(h);
    let f = &f;
    std::thread::scope(|s| {
        for (i, data) in img.data.chunks_mut(band * w * 4).enumerate() {
            s.spawn(move || f(i * band, data));
        }
    });
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

    /// A repeatable scene: soft blocks of colour with a few hard edges.
    fn scene(w: usize, h: usize) -> Rgba {
        let mut img = Rgba::new(w, h);
        for r in 0..h {
            for c in 0..w {
                let at = (r * w + c) * 4;
                let block = ((r / 40) + (c / 40)) % 3;
                let v = [
                    (90 + block * 40) as u8,
                    (80 + (c * 60 / w)) as u8,
                    (70 + (r * 50 / h)) as u8,
                ];
                img.data[at..at + 4].copy_from_slice(&[v[0], v[1], v[2], 255]);
            }
        }
        img
    }

    /// Deterministic noise of about `sigma` steps.
    pub(super) fn noisy(img: &Rgba, sigma: f32, seed: u64) -> Rgba {
        let mut out = img.clone();
        let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        for px in out.data.chunks_exact_mut(4) {
            for c in &mut px[..3] {
                // Sum of four uniforms: close enough to Gaussian.
                let mut u = 0.0;
                for _ in 0..4 {
                    s = s
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    u += (s >> 40) as f32 / (1u64 << 24) as f32 - 0.5;
                }
                *c = clamp((f32::from(*c) + u * sigma * 1.732).round() as i32);
            }
        }
        out
    }

    pub(super) fn rms_diff(a: &Rgba, b: &Rgba) -> f32 {
        let sum: f64 = a
            .data
            .iter()
            .zip(&b.data)
            .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
            .sum();
        (sum / a.data.len() as f64).sqrt() as f32
    }

    #[test]
    fn noise_is_measured_and_edges_are_not_noise() {
        let clean = scene(320, 240);
        assert!(measure(&clean).noise < 0.5, "{:?}", measure(&clean));
        // σ 4 on each channel is σ 2.7 in luma, which mixes them.
        let m = measure(&noisy(&clean, 4.0, 1));
        assert!((2.2..3.3).contains(&m.noise), "{m:?}");
    }

    #[test]
    fn stacking_removes_noise_and_keeps_what_moved() {
        let clean = scene(160, 120);
        let frames: Vec<Rgba> = (0..5).map(|i| noisy(&clean, 4.0, i)).collect();
        let merged = stack(&frames).unwrap();
        assert!(rms_diff(&merged, &clean) < rms_diff(&frames[0], &clean) * 0.7);
        // A white square that is only in the first frame stays, sharp.
        let mut moved = frames.clone();
        for r in 40..60 {
            for c in 40..60 {
                let at = (r * 160 + c) * 4;
                moved[0].data[at..at + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
        let merged = stack(&moved).unwrap();
        let at = (50 * 160 + 50) * 4;
        assert_eq!(&merged.data[at..at + 3], &[255, 255, 255]);
    }

    #[test]
    fn video_denoising_settles_on_a_still_scene() {
        let clean = scene(160, 120);
        let mut e = Enhancer::default();
        let mut plain_err = 0.0;
        let mut denoised_err = 0.0;
        for i in 0..12 {
            let frame = noisy(&clean, 5.0, i);
            let mut single = frame.clone();
            photo(&mut single);
            let mut video = frame;
            e.frame(&mut video);
            let mut reference = clean.clone();
            photo(&mut reference);
            if i >= 6 {
                plain_err += rms_diff(&single, &reference);
                denoised_err += rms_diff(&video, &reference);
            }
        }
        assert!(
            denoised_err < plain_err * 0.8,
            "{denoised_err} vs {plain_err}"
        );
    }

    #[test]
    fn a_noisy_picture_is_not_sharpened() {
        let m = Measure {
            lo: 0.0,
            hi: 255.0,
            noise: 6.0,
            colour: COLOUR_RICH,
            crisp: CRISP_SOFT,
        };
        let img = noisy(&solid(64, 64, [128, 128, 128, 255]), 6.0, 3);
        let mut out = img.clone();
        finish(&mut out, &m);
        assert_eq!(out.data, img.data);
    }

    #[test]
    fn soft_edges_are_sharpened_and_crisp_ones_left() {
        // A blurred vertical edge.
        let mut soft = Rgba::new(64, 64);
        for (i, px) in soft.data.chunks_exact_mut(4).enumerate() {
            let x = (i % 64) as i32;
            let v = (60 + (x - 28).clamp(0, 8) * 15) as u8;
            px.copy_from_slice(&[v, v, v, 255]);
        }
        let base = Measure {
            lo: 0.0,
            hi: 255.0,
            noise: 0.5,
            colour: COLOUR_RICH,
            crisp: CRISP_SOFT,
        };
        let mut sharpened = soft.clone();
        finish(&mut sharpened, &base);
        assert_ne!(sharpened.data, soft.data);
        let mut left = soft.clone();
        finish(
            &mut left,
            &Measure {
                crisp: CRISP_SHARP,
                ..base
            },
        );
        assert_eq!(left.data, soft.data);
    }

    #[test]
    fn crispness_tells_a_soft_picture_from_a_sharp_one() {
        let mut sharp = Rgba::new(128, 128);
        let mut soft = Rgba::new(128, 128);
        for (i, (a, b)) in sharp
            .data
            .chunks_exact_mut(4)
            .zip(soft.data.chunks_exact_mut(4))
            .enumerate()
        {
            let (x, y) = ((i % 128) as i32, (i / 128) as i32);
            let t = (x / 16 + y / 16) % 2 == 0;
            let v = if t { 200 } else { 50 };
            a.copy_from_slice(&[v, v, v, 255]);
            // The same squares, their edges ramped over 6 pixels.
            let dx = (x % 16).min(15 - x % 16);
            let dy = (y % 16).min(15 - y % 16);
            let k = dx.min(dy).min(3);
            let v = 125 + (i32::from(v) - 125) * k / 3;
            b.copy_from_slice(&[v as u8, v as u8, v as u8, 255]);
        }
        let (s, f) = (measure(&sharp).crisp, measure(&soft).crisp);
        assert!(s > f, "sharp {s} soft {f}");
    }

    #[test]
    fn vibrance_spares_skin_and_vivid_colour() {
        let boosts: [i32; 256] = std::array::from_fn(|s| 90 * (255 - s as i32) / 255);
        let dull = vibrance_for(130, 120, 125, &boosts);
        let skin = vibrance_for(200, 150, 120, &boosts);
        let vivid = vibrance_for(250, 20, 20, &boosts);
        assert!(dull > skin && skin > vivid, "{dull} {skin} {vivid}");
        // A camera with rich colour gets none.
        let mut img = solid(8, 8, [200, 40, 40, 255]);
        let before = img.clone();
        finish(
            &mut img,
            &Measure {
                lo: 0.0,
                hi: 255.0,
                noise: 6.0,
                colour: COLOUR_RICH,
                crisp: CRISP_SHARP,
            },
        );
        assert_eq!(img.data, before.data);
    }

    #[test]
    fn a_good_picture_comes_through_nearly_unchanged() {
        // Black to white, rich colour, crisp edges, no noise: what a good
        // camera sends.
        let colours = [
            [250, 30, 30, 255],
            [10, 30, 230, 255],
            [2, 2, 2, 255],
            [253, 253, 253, 255],
            [40, 160, 60, 255],
        ];
        let mut good = Rgba::new(128, 128);
        for (i, px) in good.data.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i % 128 / 16, i / 128 / 16);
            px.copy_from_slice(&colours[(x + 2 * y) % colours.len()]);
        }
        let mut out = good.clone();
        photo(&mut out);
        // All it may do is take 2–253 to 0–255.
        assert!(rms_diff(&out, &good) < 2.5, "{}", rms_diff(&out, &good));
    }

    #[test]
    fn washed_out_grey_is_stretched() {
        let mut img = Rgba::new(151, 4);
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            let v = 50 + (i % 151) as u8;
            px.copy_from_slice(&[v, v, v, 255]);
        }
        photo(&mut img);
        let lum: Vec<u8> = img.data.chunks_exact(4).map(|p| p[0]).collect();
        assert!(*lum.iter().min().unwrap() < 20);
        assert!(*lum.iter().max().unwrap() > 220);
        assert!(img
            .data
            .chunks_exact(4)
            .all(|p| p[0] == p[1] && p[1] == p[2]));
    }
}

/// Real frames through the pipeline: `ENHANCE_FRAMES=dir` holding folders of
/// JPEG frames; writes `before.png`, `after.png` (the video path, last
/// frame) and `photo.png` (stacked) into each, and prints the measurements.
#[cfg(test)]
mod real_frames {
    use super::*;

    #[test]
    #[ignore]
    fn run() {
        let Ok(root) = std::env::var("ENHANCE_FRAMES") else {
            return;
        };
        let mut dirs: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let mut files: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jpg"))
                .collect();
            files.sort();
            let frames: Vec<Rgba> = files
                .iter()
                .map(|f| crate::pixels::decode_jpeg(&std::fs::read(f).unwrap()).unwrap())
                .collect();
            let last = frames.last().unwrap().clone();
            let m = measure(&last);
            let mut e = Enhancer::default();
            let mut video = last.clone();
            let copies: Vec<Rgba> = frames
                .iter()
                .chain(&frames)
                .chain(&frames)
                .cloned()
                .collect();
            let n = copies.len() as u32;
            let t = std::time::Instant::now();
            for f in copies {
                video = f;
                e.frame(&mut video);
            }
            let per = t.elapsed() / n;
            let burst = &frames[frames.len() - 6..];
            let mut photo_img = stack(burst).unwrap();
            let stacked_noise = measure(&photo_img).noise;
            photo(&mut photo_img);
            println!(
                "{}: {m:?}\n    stacked noise {stacked_noise:.2}, video {per:?}/frame, after {:?}",
                dir.file_name().unwrap().to_string_lossy(),
                measure(&video)
            );
            let save = |name: &str, img: &Rgba| {
                std::fs::write(dir.join(name), crate::pixels::encode_png(img).unwrap()).unwrap()
            };
            save("before.png", &last);
            save("after.png", &video);
            save("photo.png", &photo_img);

            // Timing by stage.
            let t = std::time::Instant::now();
            for _ in 0..10 {
                std::hint::black_box(measure(&last));
            }
            let t_measure = t.elapsed() / 10;
            let t = std::time::Instant::now();
            for _ in 0..10 {
                let mut c = last.clone();
                finish(&mut c, &m);
            }
            let t_finish = t.elapsed() / 10;
            let mut c = last.clone();
            let t = std::time::Instant::now();
            for _ in 0..10 {
                temporal(&mut c, &last, 3.0);
            }
            let t_temporal = t.elapsed() / 10;
            println!("    measure {t_measure:?}, finish {t_finish:?}, temporal {t_temporal:?}");

            // The same scene from a noisier camera: known noise on each frame.
            for sigma in [2.0f32, 4.0] {
                let noisy_frames: Vec<Rgba> = frames
                    .iter()
                    .enumerate()
                    .map(|(k, f)| super::tests::noisy(f, sigma, k as u64 + 7))
                    .collect();
                let got = measure(noisy_frames.last().unwrap()).noise;
                let mut e = Enhancer::default();
                let mut out = Rgba::new(0, 0);
                for f in &noisy_frames {
                    out = f.clone();
                    e.frame(&mut out);
                }
                let mut clean_ref = last.clone();
                let mut e2 = Enhancer::default();
                for f in &frames {
                    clean_ref = f.clone();
                    e2.frame(&mut clean_ref);
                }
                let mut single = noisy_frames.last().unwrap().clone();
                photo(&mut single);
                let first = noisy_frames.len() - 6;
                let stacked = stack(&noisy_frames[first..]).unwrap();
                println!(
                    "    +noise {sigma}: measured {got:.2}; error vs clean: single {:.2}, video {:.2}, stacked raw {:.2} (noisy raw {:.2})",
                    super::tests::rms_diff(&single, &clean_ref),
                    super::tests::rms_diff(&out, &clean_ref),
                    super::tests::rms_diff(&stacked, &frames[first]),
                    super::tests::rms_diff(&noisy_frames[first], &frames[first]),
                );
                if sigma == 4.0 {
                    save("noisy-before.png", noisy_frames.last().unwrap());
                    save("noisy-after.png", &out);
                    let mut st = stacked.clone();
                    photo(&mut st);
                    save("noisy-photo.png", &st);
                }
            }
        }
    }
}
