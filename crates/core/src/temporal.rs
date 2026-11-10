//! Temporal noise reduction: each pixel averaged with the same pixel in neighbouring frames, as far as
//! the picture there has not changed.
//!
//! A time-lapse frame's noise is new in every frame while most of the scene stays put, so averaging
//! `2r + 1` frames divides the noise by up to √(2r + 1) without blurring anything in a single frame.
//! Where something moves (clouds, stars, people, a tree in the wind) the neighbour differs from the
//! frame by more than noise explains, and its weight falls to zero: no ghosts. The test compares 3×3
//! means rather than single pixels (a third of the noise), against the noise measured in the frames
//! themselves ([`NoiseProfile`]), per brightness level (noise grows with brightness).
//!
//! Frames are 16-bit display-encoded RGB as rendered. Averaging encoded values keeps a frame's mean
//! brightness exactly (the average of noisy samples has the same expectation as one of them).

use crate::smooth::median;

/// One RGB pixel.
type Px = [u16; 3];

/// A 16-bit RGB frame, 3 samples a pixel, rows packed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image16 {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u16>,
}

impl Image16 {
    /// `None` unless `data` holds exactly `width × height × 3` samples.
    #[must_use]
    pub fn new(width: usize, height: usize, data: Vec<u16>) -> Option<Self> {
        (width.checked_mul(height)?.checked_mul(3)? == data.len()).then_some(Self { width, height, data })
    }
}

/// Brightness bins of a [`NoiseProfile`].
pub const NOISE_BINS: usize = 16;

/// Noise (standard deviation, encoded units 0…1) by brightness: bin `i` covers luma
/// `i / NOISE_BINS … (i + 1) / NOISE_BINS`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseProfile {
    pub sigma: [f32; NOISE_BINS],
}

impl NoiseProfile {
    /// Noise at encoded luma `l`.
    #[must_use]
    pub fn at(&self, l: f32) -> f32 {
        let i = ((l.clamp(0.0, 1.0) * NOISE_BINS as f32) as usize).min(NOISE_BINS - 1);
        self.sigma.get(i).copied().unwrap_or(0.0)
    }

    /// The largest bin: the noise level to report.
    #[must_use]
    pub fn max(&self) -> f32 {
        self.sigma.iter().copied().fold(0.0, f32::max)
    }
}

fn luma(r: u16, g: u16, b: u16) -> f32 {
    (0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)) / 65535.0
}

/// Measures the noise of two consecutive frames of the same size from their per-pixel differences.
///
/// A moving scene makes some differences large; the lower quartile of the absolute differences is
/// taken because static pixels (pure noise) fill it: for Gaussian noise it is 0.3186 σ of the
/// difference, and a difference holds the noise of two frames (√2 σ). Bins with too few pixels
/// borrow from their neighbours. `None` for frames of different sizes.
#[must_use]
pub fn estimate_noise(a: &Image16, b: &Image16) -> Option<NoiseProfile> {
    if a.width != b.width || a.height != b.height || a.data.len() != b.data.len() {
        return None;
    }
    let mut diffs: Vec<Vec<f64>> = vec![Vec::new(); NOISE_BINS];
    // About 400 000 pixels are plenty; an odd stride avoids following a row pattern.
    let (pa, pb) = (a.data.as_chunks::<3>().0, b.data.as_chunks::<3>().0);
    let pixels = pa.len();
    let stride = (pixels / 400_000).max(1) | 1;
    for (x, y) in pa.iter().zip(pb).step_by(stride) {
        let (la, lb) = (luma(x[0], x[1], x[2]), luma(y[0], y[1], y[2]));
        // Clipped pixels have no noise to measure.
        if la <= 0.0 || la >= 0.999 {
            continue;
        }
        let bin = ((0.5 * (la + lb) * NOISE_BINS as f32) as usize).min(NOISE_BINS - 1);
        if let Some(d) = diffs.get_mut(bin) {
            // per channel, so colour noise counts as well
            for (u, v) in x.iter().zip(y) {
                d.push(f64::from(u.abs_diff(*v)) / 65535.0);
            }
        }
    }
    let mut sigma = [f32::NAN; NOISE_BINS];
    for (s, d) in sigma.iter_mut().zip(&mut diffs) {
        if d.len() >= 300 {
            d.sort_by(f64::total_cmp);
            let q25 = d.get(d.len() / 4).copied().unwrap_or(0.0);
            *s = (q25 / 0.3186 / std::f64::consts::SQRT_2) as f32;
        }
    }
    // Empty bins take the nearest measured one; nothing measured at all: no noise.
    let known: Vec<(usize, f32)> = sigma
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_finite())
        .map(|(i, s)| (i, *s))
        .collect();
    for (i, s) in sigma.iter_mut().enumerate() {
        if !s.is_finite() {
            *s = known
                .iter()
                .min_by_key(|(j, _)| i.abs_diff(*j))
                .map_or(0.0, |(_, v)| *v);
        }
    }
    Some(NoiseProfile { sigma })
}

/// [`NoiseProfile`] of a sequence position: the median, bin by bin, of the profiles given (one per
/// consecutive pair near it), so one pair with a lot of motion does not decide.
#[must_use]
pub fn median_profile(profiles: &[NoiseProfile]) -> Option<NoiseProfile> {
    if profiles.is_empty() {
        return None;
    }
    let mut sigma = [0.0f32; NOISE_BINS];
    for (i, s) in sigma.iter_mut().enumerate() {
        let v: Vec<f64> = profiles
            .iter()
            .filter_map(|p| p.sigma.get(i).map(|x| f64::from(*x)))
            .collect();
        *s = median(&v).unwrap_or(0.0) as f32;
    }
    Some(NoiseProfile { sigma })
}

/// Per-channel 3×3 means of a frame (edges repeat the border), the values the motion test compares.
#[must_use]
pub fn box_means(img: &Image16) -> Vec<u16> {
    let (w, h) = (img.width, img.height);
    let mut out = vec![0u16; img.data.len()];
    if w == 0 || h == 0 {
        return out;
    }
    let at = |x: usize, y: usize, c: usize| -> u32 {
        img.data.get((y * w + x) * 3 + c).copied().map_or(0, u32::from)
    };
    parallel_rows(&mut out, w * 3, |y, row| {
        let (y0, y2) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for x in 0..w {
            let (x0, x2) = (x.saturating_sub(1), (x + 1).min(w - 1));
            for c in 0..3 {
                let sum = at(x0, y0, c)
                    + at(x, y0, c)
                    + at(x2, y0, c)
                    + at(x0, y, c)
                    + at(x, y, c)
                    + at(x2, y, c)
                    + at(x0, y2, c)
                    + at(x, y2, c)
                    + at(x2, y2, c);
                if let Some(o) = row.get_mut(x * 3 + c) {
                    *o = ((sum + 4) / 9) as u16;
                }
            }
        }
    });
    out
}

/// How strongly [`denoise`] averages.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenoiseParams {
    /// Frames on each side of the frame that are averaged with it (1…4).
    pub radius: usize,
    /// How much difference counts as "unchanged", in multiples of the noise: 1 is cautious (no
    /// ghosts even on slow motion), 2 averages more and lets faint motion blend.
    pub tolerance: f32,
}

impl Default for DenoiseParams {
    fn default() -> Self {
        Self {
            radius: 2,
            tolerance: 1.0,
        }
    }
}

/// One frame of a sequence, denoised.
///
/// `frames` holds the frame and its neighbours (all the same size), `means` their [`box_means`],
/// `center` the index of the frame to denoise. Returns `None` when sizes do not match.
#[must_use]
pub fn denoise(
    frames: &[&Image16],
    means: &[&[u16]],
    center: usize,
    noise: &NoiseProfile,
    params: &DenoiseParams,
) -> Option<Image16> {
    let me = *frames.get(center)?;
    let my_means = *means.get(center)?;
    let n = me.data.len();
    if frames.len() != means.len()
        || frames.iter().any(|f| f.data.len() != n)
        || means.iter().any(|m| m.len() != n)
    {
        return None;
    }
    let tol = params.tolerance.max(0.1);
    // Difference of two independent 3×3 means of noise σ: variance 2σ²/9.
    let lut: Vec<f32> = (0..=NOISE_BINS * 4)
        .map(|i| {
            let s = noise.at(i as f32 / (NOISE_BINS * 4) as f32);
            (2.0 * s * s / 9.0).max(1e-12)
        })
        .collect();
    let others: Vec<(&Image16, &[u16])> = frames
        .iter()
        .zip(means)
        .enumerate()
        .filter(|(i, _)| *i != center)
        .map(|(_, (f, m))| (*f, *m))
        .collect();
    let w = me.width;
    let mine_px = me.data.as_chunks::<3>().0;
    let my_means_px = my_means.as_chunks::<3>().0;
    let others: Vec<(&[Px], &[Px])> = others
        .iter()
        .map(|(f, m)| (f.data.as_chunks::<3>().0, m.as_chunks::<3>().0))
        .collect();
    let mut out = vec![0u16; n];
    parallel_rows(&mut out, w * 3, |y, row| {
        for (x, o) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let p = y * w + x;
            let (Some(mine), Some(mm)) = (mine_px.get(p), my_means_px.get(p)) else {
                continue;
            };
            let l = luma(mm[0], mm[1], mm[2]);
            let var = lut
                .get((l.clamp(0.0, 1.0) * (NOISE_BINS * 4) as f32) as usize)
                .copied()
                .unwrap_or(1e-12);
            let mut acc = mine.map(f32::from);
            let mut total = 1.0f32;
            for (f, m) in &others {
                let (Some(theirs), Some(tm)) = (f.get(p), m.get(p)) else {
                    continue;
                };
                let mut d2 = 0.0f32;
                for (a, b) in mm.iter().zip(tm) {
                    let d = (f32::from(*a) - f32::from(*b)) / 65535.0;
                    d2 += d * d;
                }
                // Mean square difference over the channels, in units of what noise alone gives.
                let excess = (d2 / 3.0 / var - 1.0).max(0.0);
                let weight = (-excess / (tol * tol)).exp();
                if weight < 1e-3 {
                    continue;
                }
                total += weight;
                for (a, t) in acc.iter_mut().zip(theirs) {
                    *a += weight * f32::from(*t);
                }
            }
            *o = acc.map(|a| (a / total).round().clamp(0.0, 65535.0) as u16);
        }
    });
    Image16::new(me.width, me.height, out)
}

/// Frame blending: the weighted mean of `frames` in linear light, the motion blur a longer shutter
/// would have given (moving lights become smooth trails, not grey smears as in encoded values).
/// Frames are 16-bit sRGB-encoded; `weights` (one per frame, ≥ 0) need not sum to 1. `None` when
/// sizes do not match or no weight is positive.
#[must_use]
pub fn blend(frames: &[&Image16], weights: &[f32]) -> Option<Image16> {
    let first = *frames.first()?;
    let n = first.data.len();
    if frames.len() != weights.len() || frames.iter().any(|f| f.data.len() != n) {
        return None;
    }
    let total: f32 = weights.iter().filter(|w| w.is_finite() && **w > 0.0).sum();
    if total <= 0.0 {
        return None;
    }
    let decode: Vec<f32> = (0..=u16::MAX)
        .map(|v| crate::regional::srgb_to_linear(f32::from(v) / 65535.0))
        .collect();
    let parts: Vec<(&[u16], f32)> = frames
        .iter()
        .zip(weights)
        .filter(|(_, w)| w.is_finite() && **w > 0.0)
        .map(|(f, w)| (f.data.as_slice(), *w / total))
        .collect();
    let mut out = vec![0u16; n];
    parallel_rows(&mut out, first.width * 3, |y, row| {
        let start = y * first.width * 3;
        for (i, o) in row.iter_mut().enumerate() {
            let mut lin = 0.0f32;
            for (data, w) in &parts {
                let v = data.get(start + i).copied().unwrap_or(0);
                lin += w * decode.get(usize::from(v)).copied().unwrap_or(0.0);
            }
            *o = (crate::regional::linear_to_srgb(lin) * 65535.0)
                .round()
                .clamp(0.0, 65535.0) as u16;
        }
    });
    Image16::new(first.width, first.height, out)
}

/// Weights for blending `count` frames: a triangle (the middle frame counts most), which blurs motion
/// without the stepped look of equal weights.
#[must_use]
pub fn blend_weights(count: usize) -> Vec<f32> {
    let c = count.max(1);
    let mid = (c as f32 - 1.0) / 2.0;
    (0..c).map(|i| 1.0 + mid - (i as f32 - mid).abs()).collect()
}

/// Runs `f(y, row)` over the rows of `out` (`row_len` samples each) on all cores.
pub(crate) fn parallel_rows<F>(out: &mut [u16], row_len: usize, f: F)
where
    F: Fn(usize, &mut [u16]) + Sync,
{
    if row_len == 0 {
        return;
    }
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let rows = out.len() / row_len;
    let per = rows.div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (k, chunk) in out.chunks_mut(per * row_len).enumerate() {
            let f = &f;
            s.spawn(move || {
                for (i, row) in chunk.chunks_mut(row_len).enumerate() {
                    f(k * per + i, row);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic Gaussian noise (xorshift + Box–Muller).
    struct Noise(u64);
    impl Noise {
        fn uniform(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }
        fn gauss(&mut self) -> f64 {
            let (u, v) = (self.uniform(), self.uniform());
            (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
        }
    }

    const W: usize = 96;
    const H: usize = 64;

    /// Frame `t` of a clean scene: a gradient with a bright square moving 3 px a frame.
    fn clean(t: usize) -> Vec<f64> {
        let mut v = Vec::with_capacity(W * H * 3);
        for y in 0..H {
            for x in 0..W {
                let inside = (10..26).contains(&y) && (5 + 3 * t..21 + 3 * t).contains(&x);
                let g = if inside {
                    0.85
                } else {
                    0.2 + 0.4 * x as f64 / W as f64
                };
                v.extend([g, g * 0.9, g * 0.8]);
            }
        }
        v
    }

    fn noisy(clean: &[f64], sigma: f64, noise: &mut Noise) -> Image16 {
        let data = clean
            .iter()
            .map(|v| ((v + sigma * noise.gauss()).clamp(0.0, 1.0) * 65535.0).round() as u16)
            .collect();
        Image16::new(W, H, data).unwrap()
    }

    fn rmse(img: &Image16, clean: &[f64], region: impl Fn(usize, usize) -> bool) -> f64 {
        let (mut s, mut n) = (0.0, 0usize);
        for y in 0..H {
            for x in 0..W {
                if region(x, y) {
                    for c in 0..3 {
                        let i = (y * W + x) * 3 + c;
                        let d = f64::from(img.data[i]) / 65535.0 - clean[i];
                        s += d * d;
                        n += 1;
                    }
                }
            }
        }
        (s / n as f64).sqrt()
    }

    #[test]
    fn measures_the_noise_it_was_given() {
        let mut rng = Noise(7);
        let c = clean(0);
        let (a, b) = (noisy(&c, 0.02, &mut rng), noisy(&c, 0.02, &mut rng));
        let p = estimate_noise(&a, &b).unwrap();
        let s = p.at(0.4);
        assert!((s - 0.02).abs() < 0.003, "{s}");
        // A moving square does not inflate it much (the lower quartile ignores it).
        let moved = noisy(&clean(4), 0.02, &mut rng);
        let s = estimate_noise(&a, &moved).unwrap().at(0.4);
        assert!((s - 0.02).abs() < 0.004, "{s}");
    }

    #[test]
    fn averages_noise_away_and_leaves_motion_alone() {
        let sigma = 0.03;
        let mut rng = Noise(42);
        let cleans: Vec<Vec<f64>> = (0..5).map(clean).collect();
        let frames: Vec<Image16> = cleans.iter().map(|c| noisy(c, sigma, &mut rng)).collect();
        let means: Vec<Vec<u16>> = frames.iter().map(box_means).collect();
        let profile = median_profile(
            &frames
                .windows(2)
                .filter_map(|w| estimate_noise(&w[0], &w[1]))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let refs: Vec<&Image16> = frames.iter().collect();
        let mrefs: Vec<&[u16]> = means.iter().map(Vec::as_slice).collect();
        let out = denoise(&refs, &mrefs, 2, &profile, &DenoiseParams::default()).unwrap();

        // Static background away from the square's path: noise down by well over half of √5.
        let still = |x: usize, y: usize| y >= 32 && (2..W - 2).contains(&x);
        let before = rmse(&frames[2], &cleans[2], still);
        let after = rmse(&out, &cleans[2], still);
        eprintln!("static: {before:.4} -> {after:.4}");
        assert!(after < before / 1.8, "static: {before:.4} -> {after:.4}");

        // The moving square and the background it uncovers: no ghost, so no worse than the frame
        // alone (a ghost of the square, 0.85 on 0.3, would be errors of several tenths).
        let path = |x: usize, y: usize| (10..26).contains(&y) && (5..36).contains(&x);
        let before = rmse(&frames[2], &cleans[2], path);
        let after = rmse(&out, &cleans[2], path);
        eprintln!("motion: {before:.4} -> {after:.4}");
        assert!(after < before * 1.15, "motion: {before:.4} -> {after:.4}");
        // The frame's mean brightness is kept.
        let mean =
            |img: &Image16| img.data.iter().map(|v| f64::from(*v)).sum::<f64>() / img.data.len() as f64;
        assert!((mean(&out) - mean(&frames[2])).abs() / 65535.0 < 0.002);
    }

    #[test]
    fn sizes_must_match() {
        let a = Image16::new(4, 4, vec![0; 48]).unwrap();
        let b = Image16::new(2, 4, vec![0; 24]).unwrap();
        assert!(estimate_noise(&a, &b).is_none());
        let p = NoiseProfile {
            sigma: [0.01; NOISE_BINS],
        };
        let ma = box_means(&a);
        let mb = box_means(&b);
        assert!(denoise(&[&a, &b], &[&ma, &mb], 0, &p, &DenoiseParams::default()).is_none());
        assert!(Image16::new(4, 4, vec![0; 47]).is_none());
    }

    #[test]
    fn blends_in_linear_light() {
        // A light on black in one frame, black in the other: half the light, not half the code value.
        let lit = Image16::new(1, 1, vec![65535, 65535, 65535]).unwrap();
        let dark = Image16::new(1, 1, vec![0, 0, 0]).unwrap();
        let b = blend(&[&lit, &dark], &[1.0, 1.0]).unwrap();
        let expect = (crate::regional::linear_to_srgb(0.5) * 65535.0).round() as u16;
        assert_eq!(b.data, vec![expect; 3]);
        assert!(
            expect > 45000,
            "half the light is encoded well above half the code range: {expect}"
        );
        // One frame with all the weight: unchanged (within rounding).
        let one = blend(&[&lit, &dark], &[1.0, 0.0]).unwrap();
        assert_eq!(one.data, lit.data);
        assert!(blend(&[&lit], &[0.0]).is_none());
        assert!(blend(&[&lit, &dark], &[1.0]).is_none());
        assert_eq!(blend_weights(3), vec![1.0, 2.0, 1.0]);
        assert_eq!(blend_weights(1), vec![1.0]);
    }

    #[test]
    fn one_frame_is_returned_as_it_is() {
        let mut rng = Noise(3);
        let a = noisy(&clean(0), 0.02, &mut rng);
        let m = box_means(&a);
        let p = NoiseProfile {
            sigma: [0.02; NOISE_BINS],
        };
        let out = denoise(&[&a], &[&m], 0, &p, &DenoiseParams::default()).unwrap();
        assert_eq!(out, a);
    }
}
