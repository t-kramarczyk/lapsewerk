//! Sensor dust: soft dark spots in the same place in every frame (dust on the sensor's cover glass or
//! the rear of the lens, sharper and darker the smaller the aperture).
//!
//! Dust takes away a fraction of the light that reaches the sensor: it multiplies the scene by a
//! fixed pattern. Divided by its own smooth version, each frame shows its fine detail as ratios
//! around 1; over many frames the scene's detail moves (clouds, water, light), while dust stays put.
//! The per-pixel median of those ratios is the dust pattern, plus whatever in the scene does not move
//! (mountains, trees). A spot counts as dust only when it is dark, round and compact, and lies on
//! surroundings without structure of their own ([`estimate`]): a dark rock in a rock face does not.
//!
//! The correction ([`DustMap::apply_srgb16`]) brightens each spot by the inverse of the darkening
//! measured with the spot itself left out of the background, in linear light, fading out at its
//! edge. Everything outside the spots is left untouched.

use crate::regional::{linear_to_srgb, srgb_to_linear};

/// A frame's brightness in linear light (luma), for finding dust.
#[derive(Debug, Clone)]
pub struct Luma {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    /// The frame's colour, 16-bit sRGB-encoded RGB (empty: grey, the luma in every channel). Dust is
    /// measured per channel: a developed frame darkens its colours by different amounts.
    pub rgb: Vec<u16>,
}

impl Luma {
    /// The luma (Rec. 709 weights, linear light) of a 16-bit sRGB-encoded RGB image, and its colour.
    #[must_use]
    pub fn from_srgb16(data: &[u16], width: usize, height: usize) -> Option<Self> {
        if width == 0 || height == 0 || data.len() < width * height * 3 {
            return None;
        }
        let decode = decode_table();
        let rgb = data.get(..width * height * 3)?.to_vec();
        let data = luma_of(&rgb, &decode);
        Some(Self {
            width,
            height,
            data,
            rgb,
        })
    }

    /// Linear value of channel `c` (0–2) at pixel `i`.
    fn channel(&self, i: usize, c: usize, decode: &[f32]) -> f32 {
        if self.rgb.is_empty() {
            return self.data.get(i).copied().unwrap_or(0.0);
        }
        self.rgb
            .get(i * 3 + c)
            .and_then(|v| decode.get(usize::from(*v)))
            .copied()
            .unwrap_or(0.0)
    }

    /// Multiplies pixel `i` by `gain` (per channel).
    fn scale(&mut self, i: usize, gain: [f32; 3], decode: &[f32]) {
        if self.rgb.is_empty() {
            if let Some(v) = self.data.get_mut(i) {
                *v *= gain[1];
            }
            return;
        }
        let mut lin = [0.0f32; 3];
        for ((c, l), g) in lin.iter_mut().enumerate().zip(gain) {
            if let Some(v) = self.rgb.get_mut(i * 3 + c) {
                *l = decode.get(usize::from(*v)).copied().unwrap_or(0.0) * g;
                *v = encode(*l);
            }
        }
        if let Some(v) = self.data.get_mut(i) {
            *v = 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
        }
    }
}

fn luma_of(rgb: &[u16], decode: &[f32]) -> Vec<f32> {
    let at = |v: &u16| decode.get(usize::from(*v)).copied().unwrap_or(0.0);
    rgb.as_chunks::<3>()
        .0
        .iter()
        .map(|[r, g, b]| 0.2126 * at(r) + 0.7152 * at(g) + 0.0722 * at(b))
        .collect()
}

fn encode(l: f32) -> u16 {
    (linear_to_srgb(l) * 65535.0).round().clamp(0.0, 65535.0) as u16
}

/// Gains per pixel (image index, per channel).
type Gains = Vec<(usize, [f32; 3])>;

/// How dust is looked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DustParams {
    /// The faintest darkening that counts (fraction of the light: 0.008 is 0.8 %).
    pub min_depth: f32,
    /// The largest spot radius, as a fraction of the frame's short side.
    pub max_radius: f32,
}

impl Default for DustParams {
    fn default() -> Self {
        Self {
            min_depth: 0.008,
            max_radius: 0.1,
        }
    }
}

/// A dust spot, in pixels of the map it was found in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    pub x: f32,
    pub y: f32,
    /// Radius of its core (where it darkens by at least a fifth of its depth).
    pub radius: f32,
    /// How far its darkening reaches (to a few percent of its depth): the correction's radius.
    pub reach: f32,
    /// Darkening at its darkest (fraction of the light).
    pub depth: f32,
}

/// The dust found over a sequence and the gains that remove it.
#[derive(Debug, Clone)]
pub struct DustMap {
    pub width: usize,
    pub height: usize,
    pub spots: Vec<Spot>,
    /// Noise of the ratio pattern (the faintest spot that could be told from it is a few times this).
    pub noise: f32,
    /// Gain per map pixel and channel (1 outside the spots).
    gain: Vec<[f32; 3]>,
}

/// Fewest frames to tell dust from the scene.
pub const MIN_FRAMES: usize = 5;

/// Spots this large (core radius, map pixels) or larger are corrected from the smoothed ratio.
const SMOOTH_FROM: f32 = 5.0;

/// Structure allowed around a spot beyond the uncertainty the frames leave (fraction of the light).
const FLAT: f32 = 0.002;

/// How rough a spot's surroundings may be in the frames (mean distance of the finest band-pass ratio
/// from 1): sky and mist stay well below; grass, rock and ripples lie above.
const ROUGH: f32 = 0.015;

/// The core of a spot: where it darkens by at least this fraction of its depth.
const CORE: f32 = 0.2;

/// How far the background measurement leaves out around a spot, in core radii (a soft spot's tail
/// reaches about 1.5 core radii before it falls to a few percent of its depth; further out, the ring
/// reaches into more of the scene).
const LEAVE_OUT: f32 = 2.0;

/// Finds the dust in `frames` (all the same size; others are left out). `None` with fewer than
/// [`MIN_FRAMES`] usable frames.
///
/// Spots are looked for scale by scale, in band-pass ratios (detail of about one size against its
/// own surroundings, so the sky's gradients and the lens's vignetting cancel out), then measured one
/// by one against a smooth surface fitted to the ring around each spot in every frame.
#[must_use]
pub fn estimate(frames: &[Luma], params: &DustParams) -> Option<DustMap> {
    let first = frames.first()?;
    let (w, h) = (first.width, first.height);
    let frames: Vec<&Luma> = frames
        .iter()
        .filter(|f| f.width == w && f.height == h && f.data.len() == w * h)
        .collect();
    if frames.len() < MIN_FRAMES || w < 16 || h < 16 {
        return None;
    }
    let rmax = (params.max_radius * w.min(h) as f32).max(2.0);
    // The finest scale tells flat surroundings (sky, water) from textured ones (rock, trees), where a
    // dark round thing is part of the scene.
    let (fine, fine_se, fine_rough) = median_band(&frames, 2.0);
    let noise = {
        let mut v = fine_se.clone();
        let mid = v.len() / 2;
        v.select_nth_unstable_by(mid, f32::total_cmp);
        v.get(mid).copied().unwrap_or(0.0)
    };
    let texture = Texture {
        band: &fine,
        se: &fine_se,
        rough: &fine_rough,
    };
    let mut candidates = Vec::new();
    let mut scale = 2.0f32;
    while scale <= rmax {
        let (band, se) = if scale <= 2.0 {
            (fine.clone(), fine_se.clone())
        } else {
            let (band, se, _) = median_band(&frames, scale);
            (band, se)
        };
        let deficit: Vec<f32> = band.iter().map(|b| 1.0 - b).collect();
        let view = Band {
            deficit: &deficit,
            se: &se,
        };
        candidates.extend(find_spots(&view, &texture, w, h, params, (4.0 * scale).min(rmax)));
        scale *= 2.0;
    }
    // One spot seen at several scales (and nearly the same place): the scale that answers most
    // strongly.
    candidates.sort_by(|a, b| b.depth.total_cmp(&a.depth));
    let mut spots: Vec<Spot> = Vec::new();
    for c in candidates {
        if !spots
            .iter()
            .any(|s| (s.x - c.x).hypot(s.y - c.y) < 0.3 * s.radius.max(c.radius) + 1.0)
        {
            spots.push(c);
        }
    }
    // Each measured, then taken off the frames one after the other, strongest first: a spot inside
    // another's tail is measured on frames already freed of the larger one (darkenings multiply).
    // Where views of one spot overlap, the one the measurement shows deepest stays.
    let mut order: Vec<(Spot, f32)> = spots
        .iter()
        .filter_map(|s| measure_refined(&frames, s, w, h).map(|m| (*s, m.0.depth)))
        .collect();
    order.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut work: Vec<Luma> = Vec::new();
    let decode = decode_table();
    let mut gain = vec![[1.0f32; 3]; w * h];
    let mut found: Vec<Spot> = Vec::new();
    for (candidate, _) in order {
        let view: Vec<&Luma> = if work.is_empty() {
            frames.clone()
        } else {
            work.iter().collect()
        };
        let Some((spot, gains)) = measure_refined(&view, &candidate, w, h) else {
            continue;
        };
        if spot.depth < 0.5 * params.min_depth
            || spot.depth > 0.6
            || found
                .iter()
                .any(|f| (f.x - spot.x).hypot(f.y - spot.y) < 0.7 * (f.radius + spot.radius))
        {
            continue;
        }
        if work.is_empty() {
            work = frames.iter().map(|f| (*f).clone()).collect();
        }
        for &(i, g) in &gains {
            if let Some(v) = gain.get_mut(i) {
                for (a, b) in v.iter_mut().zip(g) {
                    *a *= b;
                }
            }
            for f in &mut work {
                f.scale(i, g, &decode);
            }
        }
        found.push(spot);
    }
    Some(DustMap {
        width: w,
        height: h,
        spots: found,
        noise,
        gain,
    })
}

/// [`measure`], again where the spot's own profile puts it elsewhere or gives it another size (a
/// scale sees a spot a little off and wider than it is): the ring the background is fitted to then
/// lies where it should, just outside it.
fn measure_refined(frames: &[&Luma], s: &Spot, w: usize, h: usize) -> Option<(Spot, Gains)> {
    let mut measured = measure(frames, s, w, h)?;
    let mut last = *s;
    for _ in 0..3 {
        let m = measured.0;
        let moved = (m.x - last.x).hypot(m.y - last.y);
        if moved <= 0.15 * last.radius && (m.radius - last.radius).abs() <= 0.15 * last.radius {
            break;
        }
        // Within 30 % of the size it was found at: a larger ring reaches further into the scene, which
        // can make the spot look deeper and larger again without end.
        last = Spot {
            x: m.x,
            y: m.y,
            radius: m.radius.clamp(0.7 * s.radius, 1.3 * s.radius).max(1.5),
            ..last
        };
        match measure(frames, &last, w, h) {
            Some(again) => measured = again,
            None => break,
        }
    }
    Some(measured)
}

/// Measures spot `s` in every frame against a quadratic surface fitted to the ring just outside it
/// (the background as if there were no spot: gradients and vignetting included), in each colour
/// channel. Returns the spot with its centre, core, depth and the radius the correction reaches, and
/// the gains for the pixels inside it: the inverse of the darkening, fading to 1 at the ring (where
/// the darkening is 1 by construction, so there is no edge).
fn measure(frames: &[&Luma], s: &Spot, w: usize, h: usize) -> Option<(Spot, Gains)> {
    let decode = decode_table();
    let inner = s.radius * LEAVE_OUT + 1.0;
    let outer = inner + s.radius.max(6.0);
    let x0 = (s.x - outer).floor().max(0.0) as usize;
    let x1 = ((s.x + outer).ceil().max(0.0) as usize).min(w - 1);
    let y0 = (s.y - outer).floor().max(0.0) as usize;
    let y1 = ((s.y + outer).ceil().max(0.0) as usize).min(h - 1);
    let (pw, ph) = (x1 - x0 + 1, y1 - y0 + 1);
    // (image index, patch index, x, y relative to the centre in units of `outer`, distance)
    let mut ring = Vec::new();
    let mut disk = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (dx, dy) = ((x as f32 - s.x) / outer, (y as f32 - s.y) / outer);
            let d = dx.hypot(dy) * outer;
            let at = (y * w + x, (y - y0) * pw + (x - x0), dx, dy, d);
            if d < inner {
                disk.push(at);
            } else if d <= outer {
                ring.push(at);
            }
        }
    }
    // Most of the ring in the frame: the surface is fitted, not extrapolated.
    let expected = std::f32::consts::PI * (outer * outer - inner * inner);
    if ring.len() < 12 || (ring.len() as f32) < 0.75 * expected || disk.is_empty() {
        return None;
    }
    // Ratios to the surface per frame: channels 0–2, luma 3.
    let mut per_pixel: Vec<[Vec<f32>; 4]> = vec![Default::default(); disk.len()];
    let mut residuals: Vec<Vec<f32>> = vec![Vec::with_capacity(frames.len()); ring.len()];
    'frames: for f in frames {
        let value = |i: usize, c: usize| {
            if c == 3 {
                f.data.get(i).copied().unwrap_or(0.0)
            } else {
                f.channel(i, c, &decode)
            }
        };
        let mut fits = [[0.0f64; 6]; 4];
        for (c, fit) in fits.iter_mut().enumerate() {
            let samples: Vec<(f64, f64, f64)> = ring
                .iter()
                .map(|&(i, _, dx, dy, _)| (f64::from(dx), f64::from(dy), f64::from(value(i, c))))
                .collect();
            match fit_robust(&samples) {
                Some(k) => *fit = k,
                None => continue 'frames,
            }
        }
        let surface = |k: &[f64; 6], dx: f32, dy: f32| {
            let (dx, dy) = (f64::from(dx), f64::from(dy));
            k[0] + k[1] * dx + k[2] * dy + k[3] * dx * dx + k[4] * dx * dy + k[5] * dy * dy
        };
        for (lists, &(i, _, dx, dy, _)) in per_pixel.iter_mut().zip(&disk) {
            for (c, list) in lists.iter_mut().enumerate() {
                if let Some(k) = fits.get(c) {
                    let z = surface(k, dx, dy);
                    if z > 1e-6 {
                        list.push((f64::from(value(i, c)) / z) as f32);
                    }
                }
            }
        }
        for (list, &(i, _, dx, dy, _)) in residuals.iter_mut().zip(&ring) {
            let z = surface(&fits[3], dx, dy);
            if z > 1e-6 {
                list.push((f64::from(value(i, 3)) / z) as f32);
            }
        }
    }
    // Flat surroundings: what the surface leaves in the ring, over the frames, is no more than the
    // frames leave uncertain (a rock face leaves its structure; passing clouds average out).
    let (mut sum, mut sum2, mut se_sum, mut n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for list in &mut residuals {
        if let Some((m, se)) = median_and_error(list) {
            sum += f64::from(m);
            sum2 += f64::from(m) * f64::from(m);
            se_sum += f64::from(se);
            n += 1.0;
        }
    }
    if n < 12.0 {
        return None;
    }
    let spread = ((sum2 / n) - (sum / n).powi(2)).max(0.0).sqrt() as f32;
    if spread > 3.0 * (se_sum / n) as f32 + FLAT {
        return None;
    }
    // Medians per pixel and channel; the standard error in the core says how surely the frames show it.
    let mut patches = vec![vec![1.0f32; pw * ph]; 4];
    let (mut core_se, mut core_n) = (0.0f32, 0u32);
    for (lists, &(_, pi, _, _, d)) in per_pixel.iter_mut().zip(&disk) {
        for (c, list) in lists.iter_mut().enumerate() {
            if list.len() < MIN_FRAMES {
                return None;
            }
            let (m, se) = median_and_error(list)?;
            if let Some(p) = patches.get_mut(c).and_then(|p| p.get_mut(pi)) {
                *p = m;
            }
            if c == 3 && d <= s.radius {
                core_se += se;
                core_n += 1;
            }
        }
    }
    let core_se = core_se / core_n.max(1) as f32;
    // Smoothed in proportion to the spot (a soft spot is smooth at a tenth of its size; the noise is
    // not), small ones only lightly: they would lose depth to it.
    for p in &mut patches {
        *p = if s.radius >= SMOOTH_FROM {
            blur(p, pw, ph, (s.radius / 10.0).max(1.0))
        } else {
            binomial3(p, pw, ph)
        };
    }
    let luma = patches.get(3)?;
    let ratio = |pi: usize| luma.get(pi).copied().unwrap_or(1.0);
    let depth = disk
        .iter()
        .filter(|a| a.4 <= s.radius)
        .map(|a| 1.0 - ratio(a.1))
        .fold(0.0f32, f32::max);
    if depth < 4.0 * core_se {
        return None;
    }
    // Mean darkening of one-pixel rings.
    let rings = inner.ceil() as usize + 1;
    let (mut sum, mut count) = (vec![0.0f32; rings], vec![0u32; rings]);
    for a in &disk {
        if let (Some(t), Some(c)) = (sum.get_mut(a.4 as usize), count.get_mut(a.4 as usize)) {
            *t += 1.0 - ratio(a.1);
            *c += 1;
        }
    }
    let mean_at = |k: usize| match (sum.get(k), count.get(k)) {
        (Some(t), Some(c)) if *c > 0 => Some(t / (*c as f32)),
        _ => None,
    };
    // Its centre and core, from its own profile (the core from its darkest ring outwards: dust out of
    // focus is a ring, darkest off its centre).
    let (mut mx, mut my, mut mw) = (0.0f32, 0.0f32, 0.0f32);
    for a in disk.iter().filter(|a| a.4 <= s.radius) {
        let d = (1.0 - ratio(a.1)).max(0.0);
        mx += a.2 * outer * d;
        my += a.3 * outer * d;
        mw += d;
    }
    let (cx, cy) = if mw > 0.0 {
        (s.x + mx / mw, s.y + my / mw)
    } else {
        (s.x, s.y)
    };
    let darkest = (0..rings)
        .filter_map(|k| mean_at(k).map(|m| (k, m)))
        .fold((0, 0.0f32), |a, b| if b.1 > a.1 { b } else { a });
    let core = (darkest.0..rings)
        .find(|&k| mean_at(k).is_some_and(|m| m < CORE * darkest.1))
        .map_or(inner, |k| (k as f32).max(1.5));
    // Full correction out to where it falls below a few percent of the depth, then fading to the ring.
    let floor = (0.03 * depth).max(2.0 * core_se);
    let full = (core.floor() as usize..rings)
        .find(|&k| mean_at(k).is_some_and(|m| m < floor))
        .map_or(inner, |k| k as f32 + 1.0)
        .min(0.85 * inner)
        .max((core * 1.1 + 1.0).min(0.85 * inner));
    let gains = disk
        .iter()
        .map(|a| {
            let t = fade(a.4, full, inner);
            let mut g = [1.0f32; 3];
            for (c, gc) in g.iter_mut().enumerate() {
                let r = patches
                    .get(c)
                    .and_then(|p| p.get(a.1))
                    .copied()
                    .unwrap_or(1.0)
                    .clamp(0.3, 1.0);
                *gc = 1.0 + t * (1.0 / r - 1.0);
            }
            (a.0, g)
        })
        .filter(|(_, g)| g.iter().any(|v| *v > 1.000_01))
        .collect();
    Some((
        Spot {
            x: cx,
            y: cy,
            radius: core,
            depth,
            reach: inner,
        },
        gains,
    ))
}

/// [`fit_quadratic`] twice, the second time without the samples more than three robust standard
/// deviations off the first fit (a bright mountain top, the edge of a cloud, a bird in the ring).
fn fit_robust(samples: &[(f64, f64, f64)]) -> Option<[f64; 6]> {
    let c = fit_quadratic(samples)?;
    let at = |x: f64, y: f64| c[0] + c[1] * x + c[2] * y + c[3] * x * x + c[4] * x * y + c[5] * y * y;
    let mut dev: Vec<f64> = samples.iter().map(|&(x, y, z)| (z - at(x, y)).abs()).collect();
    let mid = dev.len() / 2;
    dev.select_nth_unstable_by(mid, f64::total_cmp);
    let limit = 3.0 * 1.4826 * dev.get(mid).copied()?;
    let kept: Vec<(f64, f64, f64)> = samples
        .iter()
        .copied()
        .filter(|&(x, y, z)| (z - at(x, y)).abs() <= limit)
        .collect();
    if kept.len() < samples.len() / 2 {
        return Some(c);
    }
    fit_quadratic(&kept).or(Some(c))
}

/// Least-squares fit of `z = c0 + c1 x + c2 y + c3 x² + c4 x y + c5 y²` to (x, y, z) samples.
fn fit_quadratic(samples: &[(f64, f64, f64)]) -> Option<[f64; 6]> {
    let mut a = [[0.0f64; 7]; 6];
    for &(x, y, z) in samples {
        let t = [1.0, x, y, x * x, x * y, y * y];
        for (row, ti) in a.iter_mut().zip(t) {
            for (cell, tj) in row.iter_mut().zip(t) {
                *cell += ti * tj;
            }
            if let Some(last) = row.get_mut(6) {
                *last += ti * z;
            }
        }
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..6 {
        let pivot = (col..6).max_by(|&i, &j| {
            let v = |r: usize| a.get(r).and_then(|row| row.get(col)).map_or(0.0, |v| v.abs());
            v(i).total_cmp(&v(j))
        })?;
        a.swap(col, pivot);
        let p = *a.get(col)?.get(col)?;
        if p.abs() < 1e-12 {
            return None;
        }
        let pivot_row = *a.get(col)?;
        for (r, row) in a.iter_mut().enumerate() {
            if r != col {
                let f = row.get(col).copied().unwrap_or(0.0) / p;
                for (cell, pv) in row.iter_mut().zip(pivot_row) {
                    *cell -= f * pv;
                }
            }
        }
    }
    let mut c = [0.0f64; 6];
    for (i, ci) in c.iter_mut().enumerate() {
        let row = a.get(i)?;
        *ci = row.get(6)? / row.get(i)?;
    }
    Some(c)
}

impl DustMap {
    /// Whether no dust was found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spots.is_empty()
    }

    /// The gain at map position (`x`, `y`) (pixel centres at whole numbers), bilinear, per channel.
    fn gain_at(&self, x: f32, y: f32) -> [f32; 3] {
        let (w, h) = (self.width, self.height);
        let x = x.clamp(0.0, (w - 1) as f32);
        let y = y.clamp(0.0, (h - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let g = |x: usize, y: usize| self.gain.get(y * w + x).copied().unwrap_or([1.0; 3]);
        let (a, b, c, d) = (g(x0, y0), g(x1, y0), g(x0, y1), g(x1, y1));
        let mut out = [1.0f32; 3];
        for ((((o, a), b), c), d) in out.iter_mut().zip(a).zip(b).zip(c).zip(d) {
            let top = a * (1.0 - fx) + b * fx;
            let bottom = c * (1.0 - fx) + d * fx;
            *o = top * (1.0 - fy) + bottom * fy;
        }
        out
    }

    /// Removes the dust from a 16-bit sRGB-encoded RGB image of the whole frame (any size: the map
    /// is scaled to it). Only the spots' surroundings are touched, each pixel once (spots may overlap).
    pub fn apply_srgb16(&self, data: &mut [u16], width: usize, height: usize) {
        if self.spots.is_empty() || width == 0 || height == 0 || data.len() < width * height * 3 {
            return;
        }
        let decode = decode_table();
        // Map pixels per image pixel.
        let (sx, sy) = (
            self.width as f32 / width as f32,
            self.height as f32 / height as f32,
        );
        let mut done = vec![false; width * height];
        for s in &self.spots {
            let reach = s.reach + 2.0;
            let x0 = ((s.x - reach) / sx).floor().max(0.0) as usize;
            let x1 = (((s.x + reach) / sx).ceil().max(0.0) as usize).min(width - 1);
            let y0 = ((s.y - reach) / sy).floor().max(0.0) as usize;
            let y1 = (((s.y + reach) / sy).ceil().max(0.0) as usize).min(height - 1);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    match done.get_mut(y * width + x) {
                        Some(d) if !*d => *d = true,
                        _ => continue,
                    }
                    let g = self.gain_at((x as f32 + 0.5) * sx - 0.5, (y as f32 + 0.5) * sy - 0.5);
                    if g.iter().all(|v| *v <= 1.000_01) {
                        continue;
                    }
                    let i = (y * width + x) * 3;
                    if let Some(px) = data.get_mut(i..i + 3) {
                        for (v, gc) in px.iter_mut().zip(g) {
                            *v = encode(decode.get(usize::from(*v)).copied().unwrap_or(0.0) * gc);
                        }
                    }
                }
            }
        }
    }
}

fn decode_table() -> Vec<f32> {
    (0..=u16::MAX)
        .map(|v| srgb_to_linear(f32::from(v) / 65535.0))
        .collect()
}

/// 1 up to `inner`, 0 from `outer`, smooth in between.
fn fade(d: f32, inner: f32, outer: f32) -> f32 {
    if d <= inner {
        1.0
    } else if d >= outer {
        0.0
    } else {
        let t = (outer - d) / (outer - inner);
        t * t * (3.0 - 2.0 * t)
    }
}

/// Calls `f(index, distance)` for the pixels within `radius` of (`cx`, `cy`).
fn for_disk(cx: f32, cy: f32, radius: f32, w: usize, h: usize, mut f: impl FnMut(usize, f32)) {
    let y0 = (cy - radius).floor().max(0.0) as usize;
    let y1 = ((cy + radius).ceil().max(0.0) as usize).min(h.saturating_sub(1));
    let x0 = (cx - radius).floor().max(0.0) as usize;
    let x1 = ((cx + radius).ceil().max(0.0) as usize).min(w.saturating_sub(1));
    for y in y0..=y1 {
        for x in x0..=x1 {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            if d <= radius {
                f(y * w + x, d);
            }
        }
    }
}

/// Per-pixel median over the frames of each frame's band-pass ratio at `scale`: the frame smoothed
/// by `scale` / 2 divided by the frame smoothed by 2 · `scale` (about 1, below it where something of
/// about that size is darker than its surroundings), and the median's standard error (from the
/// spread over the frames: large where clouds pass, small where nothing moves).
fn median_band(frames: &[&Luma], scale: f32) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let Some(first) = frames.first() else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    let (w, h) = (first.width, first.height);
    let ratios: Vec<Vec<f32>> = frames
        .iter()
        .map(|f| {
            let y: Vec<f32> = f.data.iter().map(|v| v.max(1e-5)).collect();
            let fine = blur(&y, w, h, scale * 0.5);
            let coarse = blur(&y, w, h, scale * 2.0);
            fine.iter().zip(&coarse).map(|(a, b)| a / b.max(1e-5)).collect()
        })
        .collect();
    let mut out = vec![[1.0f32, 0.0, 0.0]; w * h];
    parallel_rows(&mut out, w, |y, row| {
        let mut values = Vec::with_capacity(ratios.len());
        for (x, o) in row.iter_mut().enumerate() {
            values.clear();
            values.extend(ratios.iter().filter_map(|r| r.get(y * w + x).copied()));
            let mut rough: Vec<f32> = values.iter().map(|v| (v - 1.0).abs()).collect();
            let mid = rough.len() / 2;
            let roughness = if rough.is_empty() {
                0.0
            } else {
                rough.select_nth_unstable_by(mid, f32::total_cmp);
                rough.get(mid).copied().unwrap_or(0.0)
            };
            if let Some((m, se)) = median_and_error(&mut values) {
                *o = [m, se, roughness];
            }
        }
    });
    let mut median = Vec::with_capacity(w * h);
    let mut se = Vec::with_capacity(w * h);
    let mut rough = Vec::with_capacity(w * h);
    for [m, e, r] in out {
        median.push(m);
        se.push(e);
        rough.push(r);
    }
    (median, se, rough)
}

/// The median of `values` (reordered) and its standard error: 1.25 · 1.48 · MAD / √n.
fn median_and_error(values: &mut [f32]) -> Option<(f32, f32)> {
    let n = values.len();
    if n == 0 {
        return None;
    }
    let mid = n / 2;
    values.select_nth_unstable_by(mid, f32::total_cmp);
    let m = *values.get(mid)?;
    for v in values.iter_mut() {
        *v = (*v - m).abs();
    }
    values.select_nth_unstable_by(mid, f32::total_cmp);
    let mad = *values.get(mid)?;
    Some((m, 1.2533 * 1.4826 * mad / (n as f32).sqrt()))
}

/// One scale's band-pass ratio as darkening, with its standard error per pixel.
struct Band<'a> {
    deficit: &'a [f32],
    se: &'a [f32],
}

/// The finest scale's band-pass ratio, its standard error, and how rough each frame is there
/// (median over the frames of the fine ratio's distance from 1): texture.
struct Texture<'a> {
    band: &'a [f32],
    se: &'a [f32],
    rough: &'a [f32],
}

/// The dark, round, compact spots of `deficit` on flat surroundings (`ratio`: the median ratio).
///
/// Areas above the seed level are taken deepest point first: a spot's core is what darkens by at least
/// [`CORE`] of that point's depth around it, so a deep spot does not run out along its soft edge into
/// the scene's faint leftovers, and a large area of those yields no spot.
fn find_spots(
    band: &Band<'_>,
    texture: &Texture<'_>,
    w: usize,
    h: usize,
    params: &DustParams,
    rmax: f32,
) -> Vec<Spot> {
    let deficit = band.deficit;
    let se = |i: usize| band.se.get(i).copied().unwrap_or(f32::INFINITY);
    let seed = |i: usize| params.min_depth.max(4.0 * se(i));
    let grow = |i: usize| (params.min_depth * 0.5).max(2.0 * se(i));
    let max_area = (std::f32::consts::PI * (1.5 * rmax).powi(2)) as usize;
    let level = |i: usize| deficit.get(i).copied().unwrap_or(0.0);
    let mut seeded = vec![false; w * h];
    let mut claimed = vec![false; w * h];
    let mut spots = Vec::new();
    for start in 0..w * h {
        if seeded.get(start).copied().unwrap_or(true) || level(start) < seed(start) {
            continue;
        }
        let mut area = flood(start, w, h, |i| {
            let open = level(i) >= seed(i) && !seeded.get(i).copied().unwrap_or(true);
            if open && let Some(v) = seeded.get_mut(i) {
                *v = true;
            }
            open
        });
        area.sort_unstable_by(|a, b| level(*b).total_cmp(&level(*a)));
        for &top in &area {
            if claimed.get(top).copied().unwrap_or(true) {
                continue;
            }
            let peak = level(top);
            let core = flood(top, w, h, |i| {
                let open = level(i) >= grow(i).max(CORE * peak) && !claimed.get(i).copied().unwrap_or(true);
                if open && let Some(v) = claimed.get_mut(i) {
                    *v = true;
                }
                open
            });
            if core.len() > max_area {
                continue;
            }
            if let Some(s) = judge(&core, band, texture, w, h, rmax) {
                // Its tail is not another spot.
                for_disk(s.x, s.y, s.radius * LEAVE_OUT, w, h, |i, _| {
                    if let Some(v) = claimed.get_mut(i) {
                        *v = true;
                    }
                });
                spots.push(s);
            }
        }
    }
    spots
}

/// The 4-connected pixels from `start` for which `take` (called once per pixel reached, which marks
/// it) says yes.
fn flood(start: usize, w: usize, h: usize, mut take: impl FnMut(usize) -> bool) -> Vec<usize> {
    let mut out = Vec::new();
    if !take(start) {
        return out;
    }
    let mut stack = vec![start];
    while let Some(i) = stack.pop() {
        out.push(i);
        let (x, y) = (i % w, i / w);
        let neighbours = [
            (x > 0).then(|| i - 1),
            (x + 1 < w).then_some(i + 1),
            (y > 0).then(|| i - w),
            (y + 1 < h).then_some(i + w),
        ];
        for n in neighbours.into_iter().flatten() {
            if take(n) {
                stack.push(n);
            }
        }
    }
    out
}

/// The spot `pixels` form, if it looks like dust.
fn judge(
    pixels: &[usize],
    band: &Band<'_>,
    texture: &Texture<'_>,
    w: usize,
    h: usize,
    rmax: f32,
) -> Option<Spot> {
    let deficit = band.deficit;
    let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0, usize::MAX, 0);
    let (mut sx, mut sy, mut sw, mut peak) = (0.0f64, 0.0f64, 0.0f64, 0.0f32);
    for &i in pixels {
        let (x, y) = (i % w, i / w);
        x0 = x0.min(x);
        x1 = x1.max(x);
        y0 = y0.min(y);
        y1 = y1.max(y);
        let d = deficit.get(i).copied().unwrap_or(0.0);
        sx += x as f64 * f64::from(d);
        sy += y as f64 * f64::from(d);
        sw += f64::from(d);
        peak = peak.max(d);
    }
    if sw <= 0.0 {
        return None;
    }
    let (bw, bh) = ((x1 - x0 + 1) as f32, (y1 - y0 + 1) as f32);
    let radius = bw.max(bh) / 2.0;
    let (cx, cy) = ((sx / sw) as f32, (sy / sw) as f32);
    // Round and compact (a ring of dust out of focus fills less of its circle than a disc).
    let fill = pixels.len() as f32 / (std::f32::consts::PI * radius * radius);
    if radius < 1.5 || radius > rmax || bw.max(bh) / bw.min(bh) > 2.2 || fill < 0.3 || peak > 0.6 {
        return None;
    }
    // Flat surroundings: no structure in the ring around it, beyond what the frames leave uncertain
    // (clouds passing leave some), at its own scale and at the finest.
    let (inner, outer) = (radius * 1.6 + 1.0, radius * 2.6 + 3.0);
    let ring = Ring {
        cx,
        cy,
        inner,
        outer,
        w,
        h,
    };
    let (_, fine, fine_se) = ring.stats(texture.band, texture.se)?;
    if fine > 3.0 * fine_se + FLAT {
        return None;
    }
    // Smooth surroundings in every frame (sky, mist, still water), where dust shows: on grass, rock
    // or rippling water it is lost in the texture, and a dark spot there is the scene's.
    let (rough, _, _) = ring.stats(texture.rough, texture.se)?;
    if rough > ROUGH {
        return None;
    }
    Some(Spot {
        x: cx,
        y: cy,
        radius,
        reach: radius,
        depth: peak,
    })
}

/// Gaussian blur of `sigma` (three box passes). The edges average what is inside the image.
fn blur(values: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    box3(values, w, h, sigma.round().max(1.0) as usize)
}

/// The ring between `inner` and `outer` around (`cx`, `cy`).
struct Ring {
    cx: f32,
    cy: f32,
    inner: f32,
    outer: f32,
    w: usize,
    h: usize,
}

impl Ring {
    /// Mean and standard deviation of `values` over the ring, and the mean of `se` there. `None` when
    /// less than 80 % of the ring lies in the frame.
    fn stats(&self, values: &[f32], se: &[f32]) -> Option<(f32, f32, f32)> {
        let (mut n, mut s, mut s2, mut e) = (0usize, 0.0f64, 0.0f64, 0.0f64);
        for_disk(self.cx, self.cy, self.outer, self.w, self.h, |i, d| {
            if d >= self.inner {
                let v = f64::from(values.get(i).copied().unwrap_or(0.0));
                n += 1;
                s += v;
                s2 += v * v;
                e += f64::from(se.get(i).copied().unwrap_or(0.0));
            }
        });
        let expected = std::f64::consts::PI * f64::from(self.outer.powi(2) - self.inner.powi(2));
        if n < 8 || (n as f64) < 0.8 * expected {
            return None;
        }
        let n = n as f64;
        let mean = s / n;
        Some((
            mean as f32,
            ((s2 / n) - mean * mean).max(0.0).sqrt() as f32,
            (e / n) as f32,
        ))
    }
}

/// 3 × 3 binomial smoothing (1 2 1): takes the edge off the noise without flattening small spots.
fn binomial3(values: &[f32], w: usize, h: usize) -> Vec<f32> {
    let at = |x: usize, y: usize| values.get(y * w + x).copied().unwrap_or(1.0);
    let mut out = vec![1.0f32; w * h];
    parallel_rows(&mut out, w, |y, row| {
        let (ya, yb) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for (x, o) in row.iter_mut().enumerate() {
            let (xa, xb) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let line = |y: usize| at(xa, y) + 2.0 * at(x, y) + at(xb, y);
            *o = (line(ya) + 2.0 * line(y) + line(yb)) / 16.0;
        }
    });
    out
}

fn box3(values: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut v = values.to_vec();
    for _ in 0..3 {
        v = box_rows(&v, w, h, r);
        v = transpose(&v, w, h);
        v = box_rows(&v, h, w, r);
        v = transpose(&v, h, w);
    }
    v
}

/// The mean over ±`r` along each row (`w` wide, `h` rows), of the part inside the row.
fn box_rows(values: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    parallel_rows(&mut out, w, |y, row| {
        let src = values.get(y * w..(y + 1) * w).unwrap_or(&[]);
        let mut prefix = Vec::with_capacity(w + 1);
        prefix.push(0.0f64);
        let mut acc = 0.0f64;
        for v in src {
            acc += f64::from(*v);
            prefix.push(acc);
        }
        for (x, o) in row.iter_mut().enumerate() {
            let (a, b) = (x.saturating_sub(r), (x + r + 1).min(src.len()));
            let sum = prefix.get(b).copied().unwrap_or(0.0) - prefix.get(a).copied().unwrap_or(0.0);
            *o = (sum / (b - a).max(1) as f64) as f32;
        }
    });
    out
}

fn transpose(values: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    parallel_rows(&mut out, h, |x, row| {
        for (y, o) in row.iter_mut().enumerate() {
            *o = values.get(y * w + x).copied().unwrap_or(0.0);
        }
    });
    out
}

fn parallel_rows<T: Send, F>(out: &mut [T], row_len: usize, f: F)
where
    F: Fn(usize, &mut [T]) + Sync,
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

    const W: usize = 400;
    const H: usize = 300;

    /// Deterministic noise in −1…1.
    fn noise(seed: &mut u64) -> f32 {
        *seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((*seed >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }

    /// Planted dust: (x, y, radius, depth, ring). A ring is dust out of focus: darkest at its rim.
    const DUST: [(f32, f32, f32, f32, bool); 3] = [
        (100.0, 80.0, 8.0, 0.05, false),
        (250.0, 100.0, 12.0, 0.03, true),
        (330.0, 60.0, 3.0, 0.04, false),
    ];

    fn dust_factor(x: f32, y: f32) -> f32 {
        let mut f = 1.0;
        for (dx, dy, r, depth, ring) in DUST {
            let d = ((x - dx).powi(2) + (y - dy).powi(2)).sqrt() / r;
            let shape = if ring {
                (-((d - 0.7) / 0.25).powi(2)).exp()
            } else {
                (-(d * d) * 2.0).exp()
            };
            f *= 1.0 - depth * shape;
        }
        f
    }

    /// Frame `k`: a sky (gradient with clouds drifting across, noise) over a static rock face (fixed
    /// texture with a round dark rock in it, at (200, 250)), times the dust.
    fn frame(k: usize, with_dust: bool) -> Luma {
        let mut seed = 1 + k as u64 * 7919;
        let mut tex_seed = 99u64;
        // Clouds: a different pattern in every frame (frames minutes apart share none).
        let p: Vec<f32> = (0..3).map(|_| noise(&mut seed) * 500.0).collect();
        let texture: Vec<f32> = (0..W * H).map(|_| noise(&mut tex_seed)).collect();
        let mut data = Vec::with_capacity(W * H);
        for y in 0..H {
            for x in 0..W {
                let (xf, yf) = (x as f32, y as f32);
                let clouds = 1.0
                    + 0.03 * ((xf + p[0]) / 37.0).sin() * ((yf + p[1]) / 23.0).cos()
                    + 0.015 * ((xf * 0.7 + yf * 0.3 + p[2]) / 11.0).sin();
                let mut v = (0.5 - yf / H as f32 * 0.2) * clouds;
                if y >= 200 {
                    let rock = ((xf - 200.0).powi(2) + (yf - 250.0).powi(2)).sqrt() < 7.0;
                    v = 0.2 * (1.0 + 0.15 * texture[y * W + x]) * if rock { 0.9 } else { 1.0 };
                }
                v *= 1.0 + 0.01 * noise(&mut seed);
                if with_dust {
                    v *= dust_factor(xf, yf);
                }
                data.push(v);
            }
        }
        Luma {
            width: W,
            height: H,
            data,
            rgb: Vec::new(),
        }
    }

    fn to_srgb16(l: &Luma) -> Vec<u16> {
        l.data
            .iter()
            .flat_map(|v| {
                let e = (linear_to_srgb(*v) * 65535.0).round() as u16;
                [e, e, e]
            })
            .collect()
    }

    #[test]
    fn finds_the_planted_dust_not_the_rock_and_removes_it() {
        let frames: Vec<Luma> = (0..20).map(|k| frame(k, true)).collect();
        let map = estimate(&frames, &DustParams::default()).unwrap();
        assert_eq!(map.spots.len(), DUST.len(), "{:?}", map.spots);
        for (x, y, _, depth, _) in DUST {
            let s = map
                .spots
                .iter()
                .find(|s| (s.x - x).abs() < 3.0 && (s.y - y).abs() < 3.0)
                .unwrap_or_else(|| panic!("no spot at {x},{y}: {:?}", map.spots));
            assert!(s.depth > 0.5 * depth, "{s:?}");
        }
        assert!(map.spots.iter().all(|s| s.y < 190.0), "nothing in the rock face");

        // Removing it: a dusty frame comes out as the clean one, within a fraction of a percent (the clouds
        // leave a faint pattern in the median, about 0.3 %, that no estimate gets below).
        let k = 7;
        let mut img = to_srgb16(&frame(k, true));
        map.apply_srgb16(&mut img, W, H);
        let fixed = Luma::from_srgb16(&img, W, H).unwrap();
        let clean = frame(k, false);
        for (x, y, r, _, _) in DUST {
            let mut worst = 0.0f32;
            let mut sum = 0.0f32;
            let mut n = 0.0f32;
            for_disk(x, y, r, W, H, |i, _| {
                let e = fixed.data[i] / clean.data[i] - 1.0;
                worst = worst.max(e.abs());
                sum += e;
                n += 1.0;
            });
            let mean = sum / n;
            assert!(mean.abs() < 0.006, "spot at {x},{y}: mean residual {mean}");
            assert!(worst < 0.02, "spot at {x},{y}: worst pixel {worst}");
        }
        // Away from the spots nothing changes.
        let untouched = to_srgb16(&frame(k, true));
        assert_eq!(img[(150 * W + 30) * 3], untouched[(150 * W + 30) * 3]);
    }

    #[test]
    fn a_clean_sequence_has_no_dust() {
        let frames: Vec<Luma> = (0..20).map(|k| frame(k, false)).collect();
        let map = estimate(&frames, &DustParams::default()).unwrap();
        assert!(map.is_empty(), "{:?}", map.spots);
    }

    #[test]
    fn black_and_flat_frames_have_no_dust() {
        for v in [0u16, 30_000, 65_535] {
            let img = vec![v; W * H * 3];
            let frames: Vec<Luma> = (0..8).map(|_| Luma::from_srgb16(&img, W, H).unwrap()).collect();
            let map = estimate(&frames, &DustParams::default()).unwrap();
            assert!(map.is_empty(), "{v}: {:?}", map.spots);
            let mut copy = img.clone();
            map.apply_srgb16(&mut copy, W, H);
            assert_eq!(copy, img);
        }
    }

    #[test]
    fn too_few_frames_tell_nothing() {
        let frames: Vec<Luma> = (0..4).map(|k| frame(k, true)).collect();
        assert!(estimate(&frames, &DustParams::default()).is_none());
        assert!(estimate(&[], &DustParams::default()).is_none());
        assert!(Luma::from_srgb16(&[0; 5], 2, 2).is_none());
    }
}
