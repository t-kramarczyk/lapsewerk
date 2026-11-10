//! Night sky: stars found and reduced, light pollution taken out, star trails.
//!
//! Everything works on the 16-bit sRGB-encoded frames of the frame stage ([`Image16`]), in linear
//! light (the sRGB curve decoded first), so it holds for SDR and HDR frames alike
//! ([`crate::hdr::encode_frame`] carries HDR as a scaled linear signal).
//!
//! - [`find_stars`]: point sources clearly above the local sky (≥ 5 σ of its noise), small (a star's
//!   image, not a lit window), with sub-pixel centres.
//! - [`reduce_stars`]: each star's light above its surroundings dimmed by a fraction, measured
//!   against the median of a ring around it, fading to nothing at the star's edge.
//! - [`fit_gradient`] / [`SkyGradient::apply`]: light pollution as a smooth surface (quadratic in x
//!   and y, per channel) fitted robustly to cell medians of the sky area, so stars, the Milky Way's
//!   detail and a tree reaching into the area do not pull it; taken out down to its darker part
//!   (its 10th percentile), so the bright side comes down and the dark side is not lifted. Fits of neighbouring frames are averaged
//!   ([`SkyGradient::mean`]) so it does not flicker.
//! - [`Trails`]: star trails: each frame the brightest of itself and the trail so far, the trail
//!   fading by a factor per frame (endless when it does not fade). Only light clearly above the
//!   local sky that was near a star in the frame before goes into a trail: the noise floor stays
//!   where it was, and an aircraft or a car's lights (far from where they were a frame ago) leave
//!   no dashed line.
//! - [`sky_matte`]: the sky found in a frame ([`crate::skymask`]): where trails form, stars are
//!   dimmed and light pollution is taken out, down to a horizon that is not straight.

use crate::deflicker::Rect;
use crate::regional::{linear_to_srgb, srgb_to_linear};
use crate::skymask::SkyMatte;
use crate::temporal::Image16;

/// The sky of a frame as a matte at its own size (0..255, 255 sky): `None` when none is found, or
/// it covers almost none (under 5 %) or almost all (over 97 %) of the frame (no horizon to go by).
/// At night, where land lit by a town is as smooth as the sky, none may be found (the whole frame
/// reads as sky): the area given is used then.
#[must_use]
pub fn sky_matte(img: &Image16) -> Option<SkyMatte> {
    let table = decode_table();
    let rgb: Vec<f32> = img.data.iter().map(|v| decode(&table, *v)).collect();
    let (w, h) = (img.width, img.height);
    // In a noisy night the sky's own grain reads as edges everywhere: looked for again in the
    // frame averaged over blocks of 2, 4 and 8 pixels, the matte then drawn back from the horizon by
    // a block (it errs into the sky, not onto the land).
    for k in [1usize, 2, 4, 8] {
        let (bw, bh) = (w / k, h / k);
        if bw < 16 || bh < 16 {
            break;
        }
        let small = if k == 1 {
            rgb.clone()
        } else {
            box_down(&rgb, w, h, k)
        };
        let Some(mut m) = crate::skymask::find_sky(&small, bw, bh, w, h) else {
            continue;
        };
        if !(0.05..=0.97).contains(&m.coverage()) {
            continue;
        }
        if k > 1 {
            draw_back(&mut m, k);
            soften(&mut m, k);
        }
        return Some(m);
    }
    None
}

/// `rgb` (3 floats a pixel, `w × h`) averaged over blocks of `k × k` pixels.
fn box_down(rgb: &[f32], w: usize, h: usize, k: usize) -> Vec<f32> {
    let (bw, bh) = (w / k, h / k);
    let mut out = vec![0.0f32; bw * bh * 3];
    let n = (k * k) as f32;
    for by in 0..bh {
        for bx in 0..bw {
            for c in 0..3 {
                let mut sum = 0.0f32;
                for y in by * k..(by + 1) * k {
                    for x in bx * k..(bx + 1) * k {
                        sum += rgb.get((y * w + x) * 3 + c).copied().unwrap_or(0.0);
                    }
                }
                if let Some(o) = out.get_mut((by * bw + bx) * 3 + c) {
                    *o = sum / n;
                }
            }
        }
    }
    out
}

/// The matte's edge softened over `r` pixels either way (a box blur across and down): the blocks
/// it was found in leave no steps along the horizon.
fn soften(m: &mut SkyMatte, r: usize) {
    let (w, h) = (m.width, m.height);
    let pass = |src: &[u8], across: bool| -> Vec<u8> {
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let (lo, hi, at): (usize, usize, Box<dyn Fn(usize) -> usize>) = if across {
                    (
                        x.saturating_sub(r),
                        (x + r + 1).min(w),
                        Box::new(move |v| y * w + v),
                    )
                } else {
                    (
                        y.saturating_sub(r),
                        (y + r + 1).min(h),
                        Box::new(move |v| v * w + x),
                    )
                };
                let (sum, n) = (lo..hi)
                    .filter_map(|v| src.get(at(v)))
                    .fold((0u32, 0u32), |(s, n), v| (s + u32::from(*v), n + 1));
                u8::try_from(sum / n.max(1)).unwrap_or(255)
            })
            .collect()
    };
    let across = pass(&m.data, true);
    m.data = pass(&across, false);
}

/// The matte drawn back upwards by `k` rows: each pixel the least of itself and the `k` below it.
fn draw_back(m: &mut SkyMatte, k: usize) {
    let (w, h) = (m.width, m.height);
    let old = m.data.clone();
    for x in 0..w {
        for y in 0..h {
            let least = (y..(y + k + 1).min(h))
                .filter_map(|yy| old.get(yy * w + x))
                .copied()
                .min()
                .unwrap_or(0);
            if let Some(v) = m.data.get_mut(y * w + x) {
                *v = least;
            }
        }
    }
}

/// How much of pixel (`x`, `y`) is sky in `m` (0..1); 0 outside it.
#[must_use]
pub fn sky_at(m: &SkyMatte, x: usize, y: usize) -> f32 {
    if x >= m.width {
        return 0.0;
    }
    m.data
        .get(y.saturating_mul(m.width).saturating_add(x))
        .map_or(0.0, |v| f32::from(*v) / 255.0)
}

/// Luminance weights (Rec. 709 / sRGB primaries; the HDR frames' Rec. 2020 differ little for this).
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Most stars [`find_stars`] returns (the brightest).
pub const MAX_STARS: usize = 20_000;

fn decode_table() -> Vec<f32> {
    (0..=u16::MAX)
        .map(|v| srgb_to_linear(f32::from(v) / 65535.0))
        .collect()
}

fn decode(table: &[f32], v: u16) -> f32 {
    table.get(usize::from(v)).copied().unwrap_or(0.0)
}

fn encode(l: f32) -> u16 {
    (linear_to_srgb(l.clamp(0.0, 1.0)) * 65535.0)
        .round()
        .clamp(0.0, 65535.0) as u16
}

/// Linear light → 16-bit sRGB by table: [`encode`] for every pixel of a frame is too slow (a power
/// per sample). Indexed by √l, where the curve is nearly straight; interpolated, within 2 codes.
struct Encoder(Vec<f32>);

impl Encoder {
    const STEPS: usize = 16_384;

    fn new() -> Self {
        Self(
            (0..=Self::STEPS)
                .map(|i| linear_to_srgb((i as f32 / Self::STEPS as f32).powi(2)) * 65535.0)
                .collect(),
        )
    }

    fn encode(&self, l: f32) -> u16 {
        let x = l.clamp(0.0, 1.0).sqrt() * Self::STEPS as f32;
        let i = (x as usize).min(Self::STEPS - 1);
        let f = x - i as f32;
        let (a, b) = (
            self.0.get(i).copied().unwrap_or(0.0),
            self.0.get(i + 1).copied().unwrap_or(65535.0),
        );
        (a + (b - a) * f).round().clamp(0.0, 65535.0) as u16
    }
}

/// Linear luminance of every pixel.
fn luminance(img: &Image16, table: &[f32]) -> Vec<f32> {
    img.data
        .as_chunks::<3>()
        .0
        .iter()
        .map(|[r, g, b]| {
            LUMA[0] * decode(table, *r) + LUMA[1] * decode(table, *g) + LUMA[2] * decode(table, *b)
        })
        .collect()
}

/// Mean over the (2r+1)² box around each pixel (cut at the edges), from a summed-area table.
fn box_mean(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut sat = vec![0.0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0.0f64;
        for x in 0..w {
            row += f64::from(v.get(y * w + x).copied().unwrap_or(0.0));
            let above = sat.get(y * (w + 1) + x + 1).copied().unwrap_or(0.0);
            if let Some(s) = sat.get_mut((y + 1) * (w + 1) + x + 1) {
                *s = above + row;
            }
        }
    }
    let at = |x: usize, y: usize| sat.get(y * (w + 1) + x).copied().unwrap_or(0.0);
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(h));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(w));
            let sum = at(x1, y1) - at(x0, y1) - at(x1, y0) + at(x0, y0);
            out.push((sum / ((x1 - x0) * (y1 - y0)) as f64) as f32);
        }
    }
    out
}

/// Median of `v` (reordered); `None` when empty.
fn median(v: &mut [f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    let mid = v.len() / 2;
    v.select_nth_unstable_by(mid, f32::total_cmp);
    v.get(mid).copied()
}

/// One star.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Star {
    /// Centre, pixels (sub-pixel), origin at the top left pixel's centre.
    pub x: f32,
    pub y: f32,
    /// Light above the sky at the brightest pixel (linear).
    pub peak: f32,
    /// Light above the sky summed over the 5 × 5 pixels around the centre.
    pub flux: f32,
    /// Radius of the area brighter than half the peak, pixels.
    pub radius: f32,
}

/// How [`find_stars`] decides what a star is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StarParams {
    /// How far above the local sky, in multiples of the sky's noise.
    pub threshold: f32,
    /// Largest half-peak radius, pixels: anything wider is not a point (a lamp, a lit window).
    pub max_radius: f32,
}

impl Default for StarParams {
    fn default() -> Self {
        Self {
            threshold: 5.0,
            max_radius: 3.0,
        }
    }
}

/// The local sky (background) of each pixel and the sky's noise: a box mean (radius 15) of the
/// luminance with the stars clipped out, and the robust spread of what is left.
fn sky(lum: &[f32], w: usize, h: usize) -> (Vec<f32>, f32) {
    let first = box_mean(lum, w, h, 15);
    let resid = |bg: &[f32]| -> Vec<f32> { lum.iter().zip(bg).step_by(7).map(|(l, b)| l - b).collect() };
    let spread = |mut r: Vec<f32>| -> f32 {
        let m = median(&mut r).unwrap_or(0.0);
        let mut dev: Vec<f32> = r.iter().map(|v| (v - m).abs()).collect();
        1.4826 * median(&mut dev).unwrap_or(0.0)
    };
    let sigma = spread(resid(&first)).max(1e-6);
    let clipped: Vec<f32> = lum
        .iter()
        .zip(&first)
        .map(|(l, b)| l.min(b + 3.0 * sigma))
        .collect();
    let bg = box_mean(&clipped, w, h, 15);
    let sigma = spread(resid(&bg)).max(1e-6);
    (bg, sigma)
}

/// The stars of a frame, brightest first (at most [`MAX_STARS`]).
#[must_use]
pub fn find_stars(img: &Image16, params: &StarParams) -> Vec<Star> {
    let (w, h) = (img.width, img.height);
    if w < 8 || h < 8 {
        return Vec::new();
    }
    let table = decode_table();
    let lum = luminance(img, &table);
    let (bg, sigma) = sky(&lum, w, h);
    let excess = |x: usize, y: usize| -> f32 {
        let i = y * w + x;
        lum.get(i).copied().unwrap_or(0.0) - bg.get(i).copied().unwrap_or(0.0)
    };
    let min_excess = params.threshold.max(1.0) * sigma;
    let max_area = std::f32::consts::PI * params.max_radius * params.max_radius;
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let rows: Vec<usize> = (3..h - 3).collect();
    let per = rows.len().div_ceil(threads).max(1);
    let excess = &excess;
    let mut stars: Vec<Star> = std::thread::scope(|scope| {
        let handles: Vec<_> = rows
            .chunks(per)
            .map(|band| {
                scope.spawn(move || {
                    let mut stars = Vec::new();
                    for &y in band {
                        for x in 3..w - 3 {
                            let e = excess(x, y);
                            if e <= min_excess {
                                continue;
                            }
                            // Brightest in its 5 × 5 (ties go to the first).
                            let mut is_max = true;
                            'n: for dy in 0..5 {
                                for dx in 0..5 {
                                    let (nx, ny) = (x + dx - 2, y + dy - 2);
                                    if (nx, ny) == (x, y) {
                                        continue;
                                    }
                                    let n = excess(nx, ny);
                                    if n > e || (n == e && (ny, nx) < (y, x)) {
                                        is_max = false;
                                        break 'n;
                                    }
                                }
                            }
                            if !is_max {
                                continue;
                            }
                            // Point-like: few pixels above half the peak in the 7 × 7 around it, and none of them on
                            // its edge (the corner of a bright area is a local maximum too, but does not fall off).
                            let mut area = 0.0f32;
                            let mut reaches_edge = false;
                            for dy in 0..7 {
                                for dx in 0..7 {
                                    if excess(x + dx - 3, y + dy - 3) > e / 2.0 {
                                        area += 1.0;
                                        reaches_edge |= dx == 0 || dx == 6 || dy == 0 || dy == 6;
                                    }
                                }
                            }
                            if area > max_area || reaches_edge {
                                continue;
                            }
                            let (mut sum, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32);
                            for dy in 0..5 {
                                for dx in 0..5 {
                                    let (nx, ny) = (x + dx - 2, y + dy - 2);
                                    let v = excess(nx, ny).max(0.0);
                                    sum += v;
                                    sx += v * nx as f32;
                                    sy += v * ny as f32;
                                }
                            }
                            if sum <= 0.0 {
                                continue;
                            }
                            stars.push(Star {
                                x: sx / sum,
                                y: sy / sum,
                                peak: e,
                                flux: sum,
                                radius: (area / std::f32::consts::PI).sqrt(),
                            });
                        }
                    }
                    stars
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    });
    stars.sort_by(|a, b| b.flux.total_cmp(&a.flux));
    stars.truncate(MAX_STARS);
    stars
}

/// 1 up to `inner`, 0 from `outer`, smooth in between.
fn fade(d: f32, inner: f32, outer: f32) -> f32 {
    if d <= inner {
        1.0
    } else if d >= outer {
        0.0
    } else {
        let t = (d - inner) / (outer - inner);
        1.0 - t * t * (3.0 - 2.0 * t)
    }
}

/// Dims each star's light above its surroundings by `amount` (0: unchanged, 1: down to the sky
/// around it). Returns how many stars were changed.
pub fn reduce_stars(img: &mut Image16, stars: &[Star], amount: f32) -> usize {
    let amount = amount.clamp(0.0, 1.0);
    let (w, h) = (img.width, img.height);
    if amount == 0.0 || w == 0 || h == 0 {
        return 0;
    }
    let table = decode_table();
    let mut changed = 0;
    for s in stars {
        let reach = (2.5 * s.radius + 1.5).max(2.5);
        let ring = (reach + 1.0, reach + 3.0);
        let span = ring.1.ceil() as isize;
        let (cx, cy) = (s.x.round() as isize, s.y.round() as isize);
        let pixels = || {
            (-span..=span)
                .flat_map(move |dy| (-span..=span).map(move |dx| (cx + dx, cy + dy)))
                .filter_map(move |(x, y)| {
                    let (ux, uy) = (usize::try_from(x).ok()?, usize::try_from(y).ok()?);
                    (ux < w && uy < h).then(|| {
                        let d = ((x as f32 - s.x).powi(2) + (y as f32 - s.y).powi(2)).sqrt();
                        ((uy * w + ux) * 3, d)
                    })
                })
        };
        // The sky around the star, per channel: median of the ring.
        let mut sky = [0.0f32; 3];
        let mut ok = true;
        for (c, slot) in sky.iter_mut().enumerate() {
            let mut ring_values: Vec<f32> = pixels()
                .filter(|(_, d)| (ring.0..=ring.1).contains(d))
                .filter_map(|(i, _)| img.data.get(i + c).map(|v| decode(&table, *v)))
                .collect();
            match median(&mut ring_values) {
                Some(m) if ring_values.len() >= 6 => *slot = m,
                _ => ok = false,
            }
        }
        if !ok {
            continue;
        }
        for (i, d) in pixels().filter(|(_, d)| *d < reach) {
            let k = amount * fade(d, 0.6 * reach, reach);
            for (c, bg) in sky.iter().enumerate() {
                if let Some(v) = img.data.get_mut(i + c) {
                    let lin = decode(&table, *v);
                    let over = lin - bg;
                    if over > 0.0 {
                        *v = encode(bg + over * (1.0 - k));
                    }
                }
            }
        }
        changed += 1;
    }
    changed
}

/// Light pollution over the sky area: per channel, `c0 + c1·u + c2·v + c3·u² + c4·u·v + c5·v²` in
/// linear light, `u` and `v` running 0..1 across the area.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyGradient {
    pub area: Rect,
    pub coef: [[f64; 6]; 3],
}

/// Cells across and down the sky area that [`fit_gradient`] fits to.
const CELLS: (usize, usize) = (24, 16);

fn basis(u: f64, v: f64) -> [f64; 6] {
    [1.0, u, v, u * u, u * v, v * v]
}

/// Solves the 6 × 6 system `a · x = b` (Gaussian elimination with partial pivoting).
fn solve6(a: [[f64; 6]; 6], b: [f64; 6]) -> Option<[f64; 6]> {
    // Augmented rows: the six coefficients, then the right-hand side.
    let mut m: Vec<[f64; 7]> = a
        .iter()
        .zip(b)
        .map(|(row, rhs)| {
            let mut r = [0.0; 7];
            for (d, v) in r.iter_mut().zip(row.iter().chain(std::iter::once(&rhs))) {
                *d = *v;
            }
            r
        })
        .collect();
    let at = |m: &[[f64; 7]], r: usize, c: usize| m.get(r).and_then(|row| row.get(c)).copied().unwrap_or(0.0);
    for col in 0..6 {
        let pivot = (col..6).max_by(|&i, &j| at(&m, i, col).abs().total_cmp(&at(&m, j, col).abs()))?;
        m.swap(col, pivot);
        let prow = *m.get(col)?;
        let p = *prow.get(col)?;
        if p.abs() < 1e-12 {
            return None;
        }
        for row in m.iter_mut().skip(col + 1) {
            let f = row.get(col).copied()? / p;
            for (x, y) in row.iter_mut().zip(prow.iter()).skip(col) {
                *x -= f * y;
            }
        }
    }
    let mut x = [0.0; 6];
    for r in (0..6).rev() {
        let row = m.get(r)?;
        let known: f64 = row.iter().zip(x.iter()).skip(r + 1).map(|(a, b)| a * b).sum();
        let v = (row.get(6)? - known) / row.get(r)?;
        *x.get_mut(r)? = v;
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// Least squares fit of [`basis`] to `points` (u, v, value).
fn fit(points: &[(f64, f64, f64)]) -> Option<[f64; 6]> {
    let mut a = [[0.0; 6]; 6];
    let mut b = [0.0; 6];
    for &(u, v, y) in points {
        let p = basis(u, v);
        for ((row, rhs), pi) in a.iter_mut().zip(b.iter_mut()).zip(p) {
            for (cell, pj) in row.iter_mut().zip(p) {
                *cell += pi * pj;
            }
            *rhs += pi * y;
        }
    }
    solve6(a, b)
}

/// The area's pixel bounds, `None` when it holds less than a cell a side.
fn area_pixels(area: Rect, w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let px = |v: f32, size: usize| ((v.clamp(0.0, 1.0) * size as f32).round() as usize).min(size);
    let (x0, x1) = (px(area.x, w), px(area.x + area.width, w));
    let (y0, y1) = (px(area.y, h), px(area.y + area.height, h));
    (x1 >= x0 + CELLS.0 && y1 >= y0 + CELLS.1).then_some((x0, y0, x1, y1))
}

/// Fits the light pollution in `area` of the frame. Cells' medians (stars do not move a median)
/// are fitted, and cells far off the fit (foreground, the Milky Way's core, a lit cloud) are left
/// out over three rounds. `None` for an area too small or a fit that cannot be made.
#[must_use]
pub fn fit_gradient(img: &Image16, area: Rect) -> Option<SkyGradient> {
    let (w, h) = (img.width, img.height);
    let (x0, y0, x1, y1) = area_pixels(area, w, h)?;
    let table = decode_table();
    let (cw, ch) = ((x1 - x0) / CELLS.0, (y1 - y0) / CELLS.1);
    let step = ((cw * ch) / 400).max(1);
    // Per cell: centre (u, v) and the median of each channel and of the luminance.
    let mut cells: Vec<(f64, f64, [f64; 4])> = Vec::with_capacity(CELLS.0 * CELLS.1);
    for cy in 0..CELLS.1 {
        for cx in 0..CELLS.0 {
            let mut chans: [Vec<f32>; 3] = Default::default();
            let mut k = 0usize;
            for y in y0 + cy * ch..y0 + (cy + 1) * ch {
                for x in x0 + cx * cw..x0 + (cx + 1) * cw {
                    k += 1;
                    if !k.is_multiple_of(step) {
                        continue;
                    }
                    for (c, list) in chans.iter_mut().enumerate() {
                        if let Some(v) = img.data.get((y * w + x) * 3 + c) {
                            list.push(decode(&table, *v));
                        }
                    }
                }
            }
            let [r, g, b] = chans.map(|mut v| f64::from(median(&mut v).unwrap_or(0.0)));
            let lum = f64::from(LUMA[0]) * r + f64::from(LUMA[1]) * g + f64::from(LUMA[2]) * b;
            let u = (cx as f64 + 0.5) / CELLS.0 as f64;
            let v = (cy as f64 + 0.5) / CELLS.1 as f64;
            cells.push((u, v, [r, g, b, lum]));
        }
    }
    let mut keep = vec![true; cells.len()];
    let mut lum_fit = [0.0; 6];
    for _ in 0..3 {
        let points: Vec<(f64, f64, f64)> = cells
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|((u, v, m), _)| (*u, *v, m[3]))
            .collect();
        if points.len() < 12 {
            return None;
        }
        lum_fit = fit(&points)?;
        let resid: Vec<f64> = cells
            .iter()
            .map(|(u, v, m)| m[3] - eval(&lum_fit, *u, *v))
            .collect();
        let mut kept: Vec<f32> = resid
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(r, _)| r.abs() as f32)
            .collect();
        let spread = 1.4826 * f64::from(median(&mut kept).unwrap_or(0.0));
        let limit = (2.5 * spread).max(1e-6);
        for (k, r) in keep.iter_mut().zip(&resid) {
            *k = r.abs() <= limit;
        }
    }
    let _ = lum_fit;
    let mut coef = [[0.0; 6]; 3];
    for (c, slot) in coef.iter_mut().enumerate() {
        let points: Vec<(f64, f64, f64)> = cells
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .filter_map(|((u, v, m), _)| Some((*u, *v, *m.get(c)?)))
            .collect();
        if points.len() < 12 {
            return None;
        }
        *slot = fit(&points)?;
    }
    Some(SkyGradient { area, coef })
}

fn eval(c: &[f64; 6], u: f64, v: f64) -> f64 {
    basis(u, v).iter().zip(c).map(|(b, k)| b * k).sum()
}

impl SkyGradient {
    /// The value of channel `c` at (`u`, `v`) of the area.
    #[must_use]
    pub fn at(&self, c: usize, u: f64, v: f64) -> f64 {
        self.coef.get(c).map_or(0.0, |k| eval(k, u, v))
    }

    /// The mean of channel `c` over the area.
    #[must_use]
    pub fn mean_of(&self, c: usize) -> f64 {
        // Exact for a quadratic: the means of u, v, u², u·v, v² over the unit square.
        const MEANS: [f64; 6] = [1.0, 0.5, 0.5, 1.0 / 3.0, 0.25, 1.0 / 3.0];
        self.coef
            .get(c)
            .map_or(0.0, |k| k.iter().zip(MEANS).map(|(a, m)| a * m).sum())
    }

    /// The surface's low level in channel `c`: its lowest inside the area (a grid of 15 × 15
    /// points short of its edges), but not below a quarter of its mean (a fit bending down in a corner is no sky that
    /// dark). Light pollution brightens a sky, so it is taken out down to here: the dark side stays
    /// as it is, not lifted to the mean.
    #[must_use]
    pub fn low_of(&self, c: usize) -> f64 {
        // Inside the area (not its very edges, where a quadratic bends most and no cell measured it).
        let lowest = (1..16)
            .flat_map(|j| (1..16).map(move |i| (f64::from(i) / 16.0, f64::from(j) / 16.0)))
            .map(|(u, w)| self.at(c, u, w))
            .fold(f64::INFINITY, f64::min);
        let mean = self.mean_of(c);
        if lowest.is_finite() {
            lowest.max(0.25 * mean)
        } else {
            mean
        }
    }

    /// The weighted mean of several fits (neighbouring frames), so the correction does not flicker.
    #[must_use]
    pub fn mean(fits: &[(Self, f64)]) -> Option<Self> {
        let total: f64 = fits.iter().map(|(_, w)| w).sum();
        let first = fits.first()?.0;
        if total <= 0.0 {
            return None;
        }
        let mut coef = [[0.0; 6]; 3];
        for (g, w) in fits {
            for (acc, k) in coef.iter_mut().zip(&g.coef) {
                for (a, v) in acc.iter_mut().zip(k) {
                    *a += v * w / total;
                }
            }
        }
        Some(Self {
            area: first.area,
            coef,
        })
    }

    /// Takes `amount` (0..1) of the light pollution out of the frame: in the sky area the surface
    /// minus its low level ([`Self::low_of`]), so the sky becomes even at the brightness of its
    /// darker part (the bright side is brought down, the dark side is not lifted). Outside the area the
    /// correction fades out over [`POLLUTION_FEATHER`] of the frame: the land below a light dome
    /// (lit by the moon, by the town) keeps its light; the dome's fit is no measure of it.
    pub fn apply(&self, img: &mut Image16, amount: f32) {
        self.apply_in(img, amount, None);
    }

    /// As [`Self::apply`], but where the frame's sky is (`sky`, at the frame's size): the correction
    /// reaches down to the horizon however it runs and fades with the matte's soft edge, so no band
    /// is left between the area's lower edge and the land; the land keeps its light.
    pub fn apply_in(&self, img: &mut Image16, amount: f32, sky: Option<&SkyMatte>) {
        let sky = sky.filter(|m| m.width == img.width && m.height == img.height);
        let amount = f64::from(amount.clamp(0.0, 1.0));
        let (w, h) = (img.width, img.height);
        if amount == 0.0 || w == 0 || h == 0 {
            return;
        }
        let table = decode_table();
        let enc = Encoder::new();
        let a = self.area;
        let means = [self.low_of(0), self.low_of(1), self.low_of(2)];
        let us: Vec<f64> = (0..w)
            .map(|x| (((x as f64 + 0.5) / w as f64 - f64::from(a.x)) / f64::from(a.width)).clamp(0.0, 1.0))
            .collect();
        // How far outside the area a column lies (as a share of the frame's height).
        let aspect = w as f64 / h as f64;
        let outside_x: Vec<f64> = (0..w)
            .map(|x| {
                let fx = (x as f64 + 0.5) / w as f64;
                let left = f64::from(a.x) - fx;
                let right = fx - f64::from(a.x + a.width);
                left.max(right).max(0.0) * aspect
            })
            .collect();
        crate::temporal::parallel_rows(&mut img.data, w * 3, |y, row| {
            let fy = (y as f64 + 0.5) / h as f64;
            let v = ((fy - f64::from(a.y)) / f64::from(a.height)).clamp(0.0, 1.0);
            let outside_y = (f64::from(a.y) - fy).max(fy - f64::from(a.y + a.height)).max(0.0);
            for (x, ((px, u), ox)) in row
                .as_chunks_mut::<3>()
                .0
                .iter_mut()
                .zip(&us)
                .zip(&outside_x)
                .enumerate()
            {
                let weight = match sky {
                    Some(m) => f64::from(sky_at(m, x, y)),
                    None => (1.0 - outside_y.max(*ox) / POLLUTION_FEATHER).clamp(0.0, 1.0),
                };
                if weight == 0.0 {
                    continue;
                }
                for ((s, c), mean) in px.iter_mut().zip(0..3).zip(means) {
                    let lin = f64::from(decode(&table, *s));
                    let out = lin - amount * weight * (self.at(c, *u, v) - mean);
                    *s = enc.encode(out.max(0.0) as f32);
                }
            }
        });
    }
}

/// How far outside the sky area the light pollution's correction fades out (share of the frame's
/// height).
const POLLUTION_FEATHER: f64 = 0.10;

/// Star trails: each frame the brightest of itself and the trail so far.
pub struct Trails {
    /// Linear light of the trail so far.
    acc: Vec<f32>,
    size: (usize, usize),
    decay: f32,
    /// Where trails form (the sky); `None`: the whole frame.
    area: Option<Rect>,
    /// The sky found in the frames, in place of `area` (a horizon that is not straight).
    sky: Option<SkyMatte>,
    /// The star light of the frame before ([`star_excess`]); `None` before the first frame.
    prev: Option<Vec<f32>>,
    table: Vec<f32>,
    enc: Encoder,
}

/// How far (pixels, at least) a star may move from one frame to the next and still go on with its
/// trail: the sky turns a few pixels between frames, an aircraft or a car crosses many.
const TRAIL_REACH: usize = 3;

/// How far above its local sky light must stand to count as a star's for the trails (multiples of
/// the sky's noise): at 3 σ one pixel in a thousand of plain noise counted, and grown by how far
/// a star moves that let noise near every pixel.
const TRAIL_SIGMA: f32 = 5.0;

/// Each pixel's light above its local sky (luminance), 0 where it is not a star's (under
/// [`TRAIL_SIGMA`]); `None` for a frame too small to tell a star from its sky (all of it goes into
/// the trails, as before).
fn star_excess(img: &Image16, table: &[f32]) -> Option<Vec<f32>> {
    let (w, h) = (img.width, img.height);
    let lum = luminance(img, table);
    if w < 8 || h < 8 || lum.len() != w * h {
        return None;
    }
    let (bg, sigma) = sky(&lum, w, h);
    Some(
        lum.iter()
            .zip(&bg)
            .map(|(l, b)| {
                let e = l - b;
                if e > TRAIL_SIGMA * sigma { e } else { 0.0 }
            })
            .collect(),
    )
}

/// The largest of `v` within `r` pixels either way (a square), in two passes.
fn max_near(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if w == 0 || v.len() != w * h {
        return v.to_vec();
    }
    let rows: Vec<f32> = (0..v.len())
        .map(|i| {
            let (x, y) = (i % w, i / w);
            (x.saturating_sub(r)..(x + r + 1).min(w))
                .filter_map(|xx| v.get(y * w + xx))
                .fold(0.0f32, |m, a| m.max(*a))
        })
        .collect();
    (0..v.len())
        .map(|i| {
            let (x, y) = (i % w, i / w);
            (y.saturating_sub(r)..(y + r + 1).min(h))
                .filter_map(|yy| rows.get(yy * w + x))
                .fold(0.0f32, |m, a| m.max(*a))
        })
        .collect()
}

/// `mask` grown by `r` pixels either way (a square), in two passes.
fn grow(mask: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    if w == 0 || mask.len() != w * h {
        return mask.to_vec();
    }
    let pass = |src: &[bool], along_rows: bool| -> Vec<bool> {
        let (len, lines) = if along_rows { (w, h) } else { (h, w) };
        let at = |line: usize, i: usize| if along_rows { line * w + i } else { i * w + line };
        let mut out = vec![false; src.len()];
        for line in 0..lines {
            // Ones within r of i, kept as a running count.
            let mut count = 0usize;
            for i in 0..len.min(r) {
                count += usize::from(src.get(at(line, i)).copied().unwrap_or(false));
            }
            for i in 0..len {
                if i + r < len {
                    count += usize::from(src.get(at(line, i + r)).copied().unwrap_or(false));
                }
                if i > r {
                    count -= usize::from(src.get(at(line, i - r - 1)).copied().unwrap_or(false));
                }
                if let Some(o) = out.get_mut(at(line, i)) {
                    *o = count > 0;
                }
            }
        }
        out
    };
    pass(&pass(mask, true), false)
}

/// Width of the soft edge of a trails area, as a fraction of the frame's height.
const TRAILS_FEATHER: f32 = 0.02;

/// How much of the trail shows at pixel (`x`, `y`): 1 inside `area`, 0 outside, a soft edge of
/// [`TRAILS_FEATHER`] inside the area's border (none along the frame's own edges).
fn trail_weight(area: Option<Rect>, x: usize, y: usize, w: usize, h: usize) -> f32 {
    let Some(a) = area else { return 1.0 };
    let (fx, fy) = ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
    let feather_x = TRAILS_FEATHER * h as f32 / w as f32;
    let side = |pos: f32, start: f32, size: f32, feather: f32| -> f32 {
        let end = start + size;
        let from_start = if start <= 0.0 { f32::INFINITY } else { pos - start };
        let to_end = if end >= 1.0 { f32::INFINITY } else { end - pos };
        (from_start.min(to_end) / feather).clamp(0.0, 1.0)
    };
    side(fx, a.x, a.width, feather_x) * side(fy, a.y, a.height, TRAILS_FEATHER)
}

impl Trails {
    /// Trails fading to 5 % over `length` frames; `0`: endless (they never fade).
    #[must_use]
    pub fn new(length: usize) -> Self {
        Self {
            acc: Vec::new(),
            size: (0, 0),
            decay: if length == 0 {
                1.0
            } else {
                0.05f32.powf(1.0 / length as f32)
            },
            area: None,
            sky: None,
            prev: None,
            table: decode_table(),
            enc: Encoder::new(),
        }
    }

    /// Trails only in the sky found in the frames (`sky`, at the frames' size), and within the area
    /// if one is given too: a mountain or a building reaching into the sky keeps no trails.
    #[must_use]
    pub fn in_sky(mut self, sky: SkyMatte) -> Self {
        self.sky = Some(sky);
        self
    }

    /// Sets or clears the area trails keep to (beside the sky found).
    pub fn set_area(&mut self, area: Option<Rect>) {
        self.area = area;
    }

    /// As [`Self::in_sky`], on trails already made (the sky is found in the first frame).
    pub fn set_sky(&mut self, sky: SkyMatte) {
        self.sky = Some(sky);
    }

    /// Trails only in `area` (the sky): the foreground shows each frame as it is, so headlights and
    /// lit windows leave no trails. The area's border is soft.
    #[must_use]
    pub fn in_area(mut self, area: Rect) -> Self {
        self.area = Some(area);
        self
    }

    /// The next frame with the trails so far.
    pub fn push(&mut self, img: &Image16) -> Image16 {
        if self.size != (img.width, img.height) || self.acc.len() != img.data.len() {
            self.size = (img.width, img.height);
            self.acc = vec![0.0; img.data.len()];
            self.prev = None;
        }
        // What goes into the trails: star light near a star of the frame before (the first frame:
        // none, as an aircraft in it could not be told from a star).
        let pixels = img.width * img.height;
        let reach = TRAIL_REACH.max(img.width / 320);
        let (w, h) = (img.width, img.height);
        let takes: Vec<bool> = match (star_excess(img, &self.table), self.prev.take()) {
            (None, _) => vec![true; pixels],
            (Some(now), Some(prev)) => {
                // Near a star at least half as bright a frame ago: the same star, moved on (an
                // aircraft passing over faint stars is brighter than any of them).
                let before = max_near(&prev, w, h, reach);
                let core: Vec<bool> = now
                    .iter()
                    .zip(&before)
                    .map(|(e, b)| *e > 0.0 && *b >= 0.5 * e)
                    .collect();
                self.prev = Some(now);
                // With its rim.
                grow(&core, w, h, 1)
            }
            (Some(now), None) => {
                self.prev = Some(now);
                vec![false; pixels]
            }
        };
        let sky = self
            .sky
            .as_ref()
            .filter(|m| m.width == img.width && m.height == img.height);
        let mut data = vec![0u16; img.data.len()];
        let (w, h) = (img.width, img.height);
        let row = w * 3;
        let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        // Whole rows per thread, so each knows where its pixels are.
        let per = (h.div_ceil(threads).max(1) * row).max(1);
        let (decay, area, table, enc, takes) = (self.decay, self.area, &self.table, &self.enc, &takes);
        std::thread::scope(|scope| {
            for (k, ((acc, src), out)) in self
                .acc
                .chunks_mut(per)
                .zip(img.data.chunks(per))
                .zip(data.chunks_mut(per))
                .enumerate()
            {
                scope.spawn(move || {
                    for (i, ((a, v), o)) in acc.iter_mut().zip(src).zip(out).enumerate() {
                        let now = decode(table, *v);
                        let p = (k * per + i) / 3;
                        *a *= decay;
                        if takes.get(p).copied().unwrap_or(false) {
                            *a = a.max(now);
                        }
                        let shown = a.max(now);
                        let (x, y) = (p % w.max(1), p / w.max(1));
                        // In the sky found and in the area given (a camera that moves takes
                        // its foreground into the first frame's sky: the area keeps it out).
                        let t = match sky {
                            Some(m) => sky_at(m, x, y) * trail_weight(area, x, y, w, h),
                            None => trail_weight(area, x, y, w, h),
                        };
                        *o = if t >= 1.0 {
                            enc.encode(shown)
                        } else if t <= 0.0 {
                            *v
                        } else {
                            enc.encode(now + t * (shown - now))
                        };
                    }
                });
            }
        });
        Image16 {
            width: img.width,
            height: img.height,
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic noise: uniform -1..1 (xorshift).
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
        }
    }

    /// A night sky in linear light: light pollution rising towards the bottom right, noise, and
    /// Gaussian stars (σ 0.9 px) at the given places.
    fn night(
        w: usize,
        h: usize,
        stars: &[(f32, f32, f32)],
        pollution: f32,
        seed: u64,
    ) -> (Image16, Vec<f32>) {
        let mut rng = Rng(seed | 1);
        let mut sky = Vec::with_capacity(w * h * 3);
        let mut data = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
                let glow = pollution * (0.3 * u + 0.7 * v * v);
                let base = [0.010 + glow * 1.4, 0.010 + glow, 0.012 + glow * 0.5];
                let mut star = 0.0;
                for &(sx, sy, peak) in stars {
                    let d2 = (x as f32 - sx).powi(2) + (y as f32 - sy).powi(2);
                    if d2 < 40.0 {
                        star += peak * (-d2 / (2.0 * 0.9 * 0.9)).exp();
                    }
                }
                for b in base {
                    sky.push(b);
                    let lin = (b + star + 0.0015 * rng.next()).max(0.0);
                    data.push(encode(lin));
                }
            }
        }
        (Image16::new(w, h, data).unwrap(), sky)
    }

    fn star_field(n: usize, w: usize, h: usize) -> Vec<(f32, f32, f32)> {
        let mut rng = Rng(77);
        (0..n)
            .map(|_| {
                let x = 8.0 + (rng.next() * 0.5 + 0.5) * (w as f32 - 16.0);
                let y = 8.0 + (rng.next() * 0.5 + 0.5) * (h as f32 - 16.0);
                let peak = 0.03 + 0.3 * (rng.next() * 0.5 + 0.5);
                (x, y, peak)
            })
            .collect()
    }

    #[test]
    fn finds_the_stars_where_they_are() {
        let (w, h) = (320, 200);
        let truth = star_field(60, w, h);
        // Stars closer than 4 px merge into one; count only those with room.
        let separate: Vec<&(f32, f32, f32)> = truth
            .iter()
            .filter(|a| {
                truth
                    .iter()
                    .all(|b| std::ptr::eq(*a, b) || (a.0 - b.0).hypot(a.1 - b.1) > 4.0)
            })
            .collect();
        let (img, _) = night(w, h, &truth, 0.05, 9);
        let found = find_stars(&img, &StarParams::default());
        let mut hits = 0;
        let mut worst = 0.0f32;
        for t in &separate {
            if let Some(d) = found
                .iter()
                .map(|s| (s.x - t.0).hypot(s.y - t.1))
                .min_by(f32::total_cmp)
                .filter(|d| *d < 1.5)
            {
                hits += 1;
                worst = worst.max(d);
            }
        }
        let false_finds = found
            .iter()
            .filter(|s| truth.iter().all(|t| (s.x - t.0).hypot(s.y - t.1) > 3.0))
            .count();
        assert!(
            hits * 100 >= separate.len() * 95,
            "{hits} of {} found",
            separate.len()
        );
        assert!(worst < 0.35, "centre off by {worst} px");
        assert!(false_finds <= 1, "{false_finds} false stars");
    }

    #[test]
    fn a_lamp_or_a_bright_area_is_not_a_star() {
        let (w, h) = (120, 80);
        let (mut img, _) = night(w, h, &[], 0.0, 3);
        // A lit window: 9 × 9 pixels, bright.
        for y in 30..39 {
            for x in 50..59 {
                for c in 0..3 {
                    img.data[(y * w + x) * 3 + c] = encode(0.5);
                }
            }
        }
        assert!(find_stars(&img, &StarParams::default()).is_empty());
    }

    #[test]
    fn reducing_dims_stars_and_leaves_the_sky() {
        let (w, h) = (160, 100);
        let truth = vec![(40.0, 30.0, 0.3), (110.0, 70.0, 0.15)];
        let (img, _) = night(w, h, &truth, 0.0, 5);
        let stars = find_stars(&img, &StarParams::default());
        assert_eq!(stars.len(), 2);
        let mut out = img.clone();
        assert_eq!(reduce_stars(&mut out, &stars, 0.8), 2);
        let table = decode_table();
        let lum_at = |im: &Image16, x: usize, y: usize| decode(&table, im.data[(y * w + x) * 3 + 1]);
        for &(x, y, _) in &truth {
            let (x, y) = (x as usize, y as usize);
            let before = lum_at(&img, x, y) - 0.010;
            let after = lum_at(&out, x, y) - 0.010;
            assert!(after < 0.3 * before, "star at {x},{y}: {before} -> {after}");
        }
        // Away from the stars nothing changed.
        let far = (0..w * h)
            .filter(|p| {
                truth
                    .iter()
                    .all(|t| ((p % w) as f32 - t.0).hypot((p / w) as f32 - t.1) > 12.0)
            })
            .all(|p| (0..3).all(|c| img.data[p * 3 + c] == out.data[p * 3 + c]));
        assert!(far);
    }

    #[test]
    fn the_land_below_the_sky_area_keeps_its_light() {
        let (w, h) = (240, 160);
        let (mut img, _) = night(w, h, &star_field(40, w, h), 0.08, 5);
        // A lit hillside in the lower half.
        for y in h / 2..h {
            for x in 0..w {
                for c in 0..3 {
                    img.data[(y * w + x) * 3 + c] = 20_000;
                }
            }
        }
        let sky = Rect::new(0.0, 0.0, 1.0, 0.5).unwrap();
        let g = fit_gradient(&img, sky).unwrap();
        let mut out = img.clone();
        g.apply(&mut out, 1.0);
        // Below the soft edge (a tenth of the height) the land is as it was.
        let y = h / 2 + h / 8;
        for x in 0..w {
            let i = (y * w + x) * 3;
            assert_eq!(out.data[i..i + 3], img.data[i..i + 3], "x {x}");
        }
    }

    #[test]
    fn light_pollution_comes_out_and_the_dark_side_stays() {
        let (w, h) = (240, 160);
        let stars = star_field(80, w, h);
        let (img, sky) = night(w, h, &stars, 0.08, 11);
        let area = Rect::new(0.0, 0.0, 1.0, 1.0).unwrap();
        let g = fit_gradient(&img, area).unwrap();
        let mut out = img.clone();
        g.apply(&mut out, 1.0);
        // The sky (true background, green) before and after: its spread across the frame, from a
        // grid of 8 × 8 cell medians of the frame (stars do not move a median).
        let table = decode_table();
        let cells = |im: &Image16| -> Vec<f32> {
            let mut v = Vec::new();
            for cy in 0..8 {
                for cx in 0..8 {
                    let mut px: Vec<f32> = (cy * h / 8..(cy + 1) * h / 8)
                        .flat_map(|y| (cx * w / 8..(cx + 1) * w / 8).map(move |x| (y * w + x) * 3 + 1))
                        .map(|i| decode(&table, im.data[i]))
                        .collect();
                    v.push(median(&mut px).unwrap());
                }
            }
            v
        };
        let span = |v: &[f32]| {
            v.iter().copied().fold(f32::MIN, f32::max) - v.iter().copied().fold(f32::MAX, f32::min)
        };
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
        let (before, after) = (cells(&img), cells(&out));
        let truth_span = span(&sky.iter().skip(1).step_by(3).copied().collect::<Vec<_>>());
        assert!(span(&before) > 0.8 * truth_span);
        assert!(
            span(&after) < 0.08 * span(&before),
            "{} -> {}",
            span(&before),
            span(&after)
        );
        // Brought down, never lifted: darker on average, the darkest part about as it was.
        assert!(mean(&after) < mean(&before));
        let low = |v: &[f32]| v.iter().copied().fold(f32::MAX, f32::min);
        // (The darkest cell is a median over an eighth of the frame, the surface's lowest a corner:
        // a little darker, never lighter.)
        assert!(
            low(&after) <= 1.03 * low(&before) && low(&after) >= 0.75 * low(&before),
            "{} -> {}",
            low(&before),
            low(&after)
        );
        // Averaging identical fits changes nothing.
        let m = SkyGradient::mean(&[(g, 1.0), (g, 3.0)]).unwrap();
        for c in 0..3 {
            for k in 0..6 {
                assert!((m.coef[c][k] - g.coef[c][k]).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn the_table_encoder_is_within_two_codes() {
        let enc = Encoder::new();
        for i in 0..=200_000 {
            let l = i as f32 / 200_000.0;
            let d = i32::from(enc.encode(l)) - i32::from(encode(l));
            assert!(d.abs() <= 2, "{l}: {d}");
        }
        assert_eq!((enc.encode(-1.0), enc.encode(2.0)), (0, 65535));
    }

    #[test]
    fn trails_stay_in_the_sky() {
        // 20 × 100: a dot moving down column 5 through sky (upper half) and foreground.
        let (w, h) = (20, 100);
        let dot = |y: usize| {
            let mut d = vec![0u16; w * h * 3];
            for c in 0..3 {
                d[(y * w + 5) * 3 + c] = 65535;
            }
            Image16::new(w, h, d).unwrap()
        };
        let sky = Rect::new(0.0, 0.0, 1.0, 0.5).unwrap();
        let mut t = Trails::new(0).in_area(sky);
        let mut out = None;
        // A star moves a pixel or two a frame.
        for y in (10..=90).step_by(2) {
            out = Some(t.push(&dot(y)));
        }
        let out = out.unwrap();
        let at = |y: usize| out.data[(y * w + 5) * 3];
        assert_eq!((at(12), at(30)), (65535, 65535), "trail in the sky");
        assert_eq!(at(70), 0, "no trail in the foreground");
        assert_eq!(at(90), 65535, "the frame itself shows");
        // The soft edge: full trail well inside, none below the area.
        assert_eq!(trail_weight(Some(sky), 5, 40, w, h), 1.0);
        assert_eq!(trail_weight(Some(sky), 5, 50, w, h), 0.0);
        assert!((0.0..1.0).contains(&trail_weight(Some(sky), 5, 49, w, h)));
        assert_eq!(
            trail_weight(Some(sky), 0, 0, w, h),
            1.0,
            "no edge along the frame's own border"
        );
    }

    #[test]
    fn trails_leave_out_noise_and_aircraft() {
        let (w, h) = (64, 48);
        let mut rng = Rng(7);
        // A dark sky with noise, a star drifting a pixel a frame along row 10, and an aircraft's
        // light jumping 15 pixels a frame along row 30.
        let frame = |i: usize, rng: &mut Rng| {
            let mut d = vec![0u16; w * h * 3];
            for y in 0..h {
                for x in 0..w {
                    let v = 0.01 + 0.002 * rng.next();
                    let star = x == 5 + i && y == 10;
                    let plane = x == 2 + 15 * i && y == 30;
                    let l = if star || plane { 0.8 } else { v };
                    for c in 0..3 {
                        d[(y * w + x) * 3 + c] = encode(l);
                    }
                }
            }
            Image16::new(w, h, d).unwrap()
        };
        let mut t = Trails::new(0);
        let mut out = None;
        for i in 0..4 {
            out = Some(t.push(&frame(i, &mut rng)));
        }
        let out = out.unwrap();
        let table = decode_table();
        let at = |x: usize, y: usize| decode(&table, out.data[(y * w + x) * 3]);
        // The star's trail is there.
        for x in 6..9 {
            assert!(at(x, 10) > 0.7, "star trail at {x}: {}", at(x, 10));
        }
        // The aircraft shows where it is now, not where it was.
        assert!(at(2 + 15 * 3, 30) > 0.7);
        for x in [2, 17, 32] {
            assert!(at(x, 30) < 0.02, "aircraft left a mark at {x}: {}", at(x, 30));
        }
        // The noise floor stays: the sky's mean is as in one frame, not the brightest of four.
        let sky: f32 = (20..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(_, y)| y != 30)
            .map(|(x, y)| at(x, y))
            .sum::<f32>()
            / ((h - 21) * w) as f32;
        assert!((sky - 0.01).abs() < 0.0015, "noise floor lifted to {sky}");
    }

    #[test]
    fn trails_follow_the_sky_found_not_a_rectangle() {
        // 20 × 20, the sky the upper 8 rows but rows 0-15 in columns 15-19 (a sky over a valley).
        let (w, h) = (20, 20);
        let mut data = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = if y < 8 || (x >= 15 && y < 16) { 255 } else { 0 };
            }
        }
        let m = SkyMatte {
            width: w,
            height: h,
            data,
        };
        assert_eq!(sky_at(&m, 17, 12), 1.0);
        assert_eq!(sky_at(&m, 5, 12), 0.0);
        assert_eq!(sky_at(&m, 25, 0), 0.0, "outside the matte");
        let lit = |x: usize, y: usize| {
            let mut d = vec![0u16; w * h * 3];
            for c in 0..3 {
                d[(y * w + x) * 3 + c] = 65535;
            }
            Image16::new(w, h, d).unwrap()
        };
        // A star setting into the valley's sky, and one setting behind the land.
        let mut valley = Trails::new(0).in_sky(m.clone());
        for (x, y) in [(17, 10), (17, 11), (17, 12), (17, 13)] {
            valley.push(&lit(x, y));
        }
        let out = valley.push(&lit(17, 14));
        assert_eq!(
            out.data[(12 * w + 17) * 3],
            65535,
            "a trail in the sky below the rectangle's edge"
        );
        let mut land = Trails::new(0).in_sky(m);
        for (x, y) in [(5, 6), (5, 7), (5, 8), (5, 9), (5, 10)] {
            land.push(&lit(x, y));
        }
        let out = land.push(&lit(5, 11));
        assert_eq!(out.data[(7 * w + 5) * 3], 65535, "in the sky");
        assert_eq!(out.data[(9 * w + 5) * 3], 0, "none on the land");
    }

    #[test]
    fn a_matte_found_in_blocks_is_drawn_back_into_the_sky() {
        // Averaging blocks of 2: means of each block.
        let rgb: Vec<f32> = (0..4 * 4 * 3).map(|i| (i / 3 % 4) as f32).collect();
        let small = box_down(&rgb, 4, 4, 2);
        assert_eq!(small.len(), 2 * 2 * 3);
        assert!((small[0] - 0.5).abs() < 1e-6 && (small[3] - 2.5).abs() < 1e-6);
        // Sky down to row 5 of 10, drawn back by 2: down to row 3.
        let mut m = SkyMatte {
            width: 1,
            height: 10,
            data: (0..10).map(|y| if y < 6 { 255 } else { 0 }).collect(),
        };
        draw_back(&mut m, 2);
        assert_eq!(m.data, [255, 255, 255, 255, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn trails_keep_the_brightest_and_fade() {
        let dot = |x: usize| {
            let mut d = vec![0u16; 8 * 3];
            d[x * 3] = 65535;
            Image16::new(8, 1, d).unwrap()
        };
        let mut endless = Trails::new(0);
        endless.push(&dot(1));
        endless.push(&dot(3));
        let out = endless.push(&dot(5));
        assert_eq!([out.data[3], out.data[9], out.data[15]], [65535; 3]);
        let mut fading = Trails::new(2);
        fading.push(&dot(1));
        fading.push(&dot(3));
        let out = fading.push(&dot(5));
        let table = decode_table();
        // Two frames old: 5 % of its light.
        assert!((decode(&table, out.data[3]) - 0.05).abs() < 0.002);
        assert_eq!(out.data[15], 65535);
    }
}
