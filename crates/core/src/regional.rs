//! Regional deflicker: flicker that differs across the frame (the sky flickers, the foreground does
//! not; a lamp-lit street flickers with the mains), corrected tile by tile.
//!
//! The frame is divided into a grid of tiles. Each tile's brightness is measured in every frame and
//! smoothed over the sequence like the whole frame's ([`crate::deflicker::corrections`]); what a
//! tile needs beyond the whole frame's correction is its regional correction. Corrections are
//! applied to the rendered frame in linear light, interpolated smoothly between tile centres, so
//! there are no tile edges. Because they act on the finished image, the tone curve does not bend
//! them (unlike exposure).

use crate::deflicker::{self, DeflickerParams, ImageView, MeasureOptions, Rect};

/// Tiles across and down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
}

impl Grid {
    /// `None` unless both are 1…32.
    #[must_use]
    pub fn new(cols: usize, rows: usize) -> Option<Self> {
        ((1..=32).contains(&cols) && (1..=32).contains(&rows)).then_some(Self { cols, rows })
    }

    #[must_use]
    pub fn tiles(&self) -> usize {
        self.cols * self.rows
    }
}

/// Largest regional correction, in stops: more would be a real change of light, not flicker.
pub const MAX_REGIONAL_STOPS: f64 = 0.5;

/// Brightness of each tile of `image` (row by row, top left first), as [`deflicker::measure`] gives it.
#[must_use]
pub fn measure_tiles(image: ImageView<'_>, grid: Grid, opts: &MeasureOptions) -> Vec<Option<f64>> {
    let mut out = Vec::with_capacity(grid.tiles());
    for r in 0..grid.rows {
        for c in 0..grid.cols {
            let roi = Rect {
                x: c as f32 / grid.cols as f32,
                y: r as f32 / grid.rows as f32,
                width: 1.0 / grid.cols as f32,
                height: 1.0 / grid.rows as f32,
            };
            let o = MeasureOptions {
                roi: Some(roi),
                ..*opts
            };
            out.push(deflicker::measure(image, &o).map(|m| m.stops));
        }
    }
    out
}

/// Regional corrections in stops, per frame and tile: each tile's deflicker correction minus the
/// whole frame's (`global`, measured on the same frames), limited to ±[`MAX_REGIONAL_STOPS`] and
/// centred on 0 per frame. A tile or frame without a measurement gets 0.
#[must_use]
pub fn corrections(
    tiles: &[Vec<Option<f64>>],
    global: &[Option<f64>],
    params: &DeflickerParams,
) -> Vec<Vec<f64>> {
    let frames = tiles.len();
    let count = tiles.iter().map(Vec::len).max().unwrap_or(0);
    let whole = deflicker::corrections(global, params);
    let mut out = vec![vec![0.0; count]; frames];
    for k in 0..count {
        let series: Vec<Option<f64>> = tiles.iter().map(|t| t.get(k).copied().flatten()).collect();
        let own = deflicker::corrections(&series, params);
        for (t, frame) in out.iter_mut().enumerate() {
            let measured =
                series.get(t).copied().flatten().is_some() && global.get(t).copied().flatten().is_some();
            if let (true, Some(slot), Some(c), Some(g)) =
                (measured, frame.get_mut(k), own.get(t), whole.get(t))
            {
                *slot = (c - g).clamp(-MAX_REGIONAL_STOPS, MAX_REGIONAL_STOPS);
            }
        }
    }
    // Centred per frame: the regional part redistributes brightness within the frame and leaves the
    // whole frame's brightness to the global deflicker (which it would otherwise disturb).
    for (t, frame) in out.iter_mut().enumerate() {
        let measured: Vec<usize> = (0..frame.len())
            .filter(|&k| tiles.get(t).and_then(|f| f.get(k)).copied().flatten().is_some())
            .collect();
        if measured.is_empty() {
            continue;
        }
        let mean = measured.iter().filter_map(|&k| frame.get(k)).sum::<f64>() / measured.len() as f64;
        for &k in &measured {
            if let Some(v) = frame.get_mut(k) {
                *v -= mean;
            }
        }
    }
    out
}

/// Gain (linear factor) at normalized position (`u`, `v`) of the frame: the tiles' corrections
/// interpolated bilinearly between tile centres (held constant beyond the outer centres).
#[must_use]
pub fn gain_at(grid: Grid, stops: &[f64], u: f64, v: f64) -> f64 {
    let fx = (u * grid.cols as f64 - 0.5).clamp(0.0, (grid.cols - 1) as f64);
    let fy = (v * grid.rows as f64 - 0.5).clamp(0.0, (grid.rows - 1) as f64);
    let (c0, r0) = (fx.floor() as usize, fy.floor() as usize);
    let (c1, r1) = ((c0 + 1).min(grid.cols - 1), (r0 + 1).min(grid.rows - 1));
    let (tx, ty) = (fx - c0 as f64, fy - r0 as f64);
    let s = |c: usize, r: usize| stops.get(r * grid.cols + c).copied().unwrap_or(0.0);
    let top = s(c0, r0) * (1.0 - tx) + s(c1, r0) * tx;
    let bottom = s(c0, r1) * (1.0 - tx) + s(c1, r1) * tx;
    (top * (1.0 - ty) + bottom * ty).exp2()
}

/// Gains for every pixel of a `width × height` frame, computed on a coarse lattice (every 8 px) and
/// interpolated: the field is smooth, so this is exact to well below a code value.
fn gain_field(grid: Grid, stops: &[f64], width: usize, height: usize) -> impl Fn(usize, usize) -> f32 + Sync {
    const STEP: usize = 8;
    let (lw, lh) = (width.div_ceil(STEP) + 1, height.div_ceil(STEP) + 1);
    let mut lattice = Vec::with_capacity(lw * lh);
    for ly in 0..lh {
        for lx in 0..lw {
            let u = ((lx * STEP) as f64 + 0.5) / width.max(1) as f64;
            let v = ((ly * STEP) as f64 + 0.5) / height.max(1) as f64;
            lattice.push(gain_at(grid, stops, u, v) as f32);
        }
    }
    move |x: usize, y: usize| {
        let (lx, ly) = (x / STEP, y / STEP);
        let (tx, ty) = ((x % STEP) as f32 / STEP as f32, (y % STEP) as f32 / STEP as f32);
        let g = |a: usize, b: usize| lattice.get(b * lw + a).copied().unwrap_or(1.0);
        let top = g(lx, ly) * (1.0 - tx) + g(lx + 1, ly) * tx;
        let bottom = g(lx, ly + 1) * (1.0 - tx) + g(lx + 1, ly + 1) * tx;
        top * (1.0 - ty) + bottom * ty
    }
}

/// Applies `stops` (one per tile) to a linear float RGB image (3 channels).
pub fn apply_linear(data: &mut [f32], width: usize, height: usize, grid: Grid, stops: &[f64]) {
    let gain = gain_field(grid, stops, width, height);
    for (i, px) in data.as_chunks_mut::<3>().0.iter_mut().enumerate() {
        let g = gain(i % width.max(1), i / width.max(1));
        for v in px.iter_mut() {
            *v *= g;
        }
    }
}

/// Applies `stops` (one per tile) to a 16-bit sRGB-encoded RGB image: decoded to linear light,
/// scaled, encoded again.
pub fn apply_srgb16(data: &mut [u16], width: usize, height: usize, grid: Grid, stops: &[f64]) {
    if stops.iter().all(|s| s.abs() < 1e-6) || width == 0 {
        return;
    }
    let decode: Vec<f32> = (0..=u16::MAX)
        .map(|v| srgb_to_linear(f32::from(v) / 65535.0))
        .collect();
    let gain = gain_field(grid, stops, width, height);
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let rows_per = height.div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (k, chunk) in data.chunks_mut(rows_per * width * 3).enumerate() {
            let (decode, gain) = (&decode, &gain);
            s.spawn(move || {
                for (i, px) in chunk.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                    let (x, y) = (i % width, k * rows_per + i / width);
                    let g = gain(x, y);
                    for v in px.iter_mut() {
                        let lin = decode.get(usize::from(*v)).copied().unwrap_or(0.0) * g;
                        *v = (linear_to_srgb(lin) * 65535.0).round().clamp(0.0, 65535.0) as u16;
                    }
                }
            });
        }
    });
}

/// IEC 61966-2-1 decoding.
#[must_use]
pub fn srgb_to_linear(e: f32) -> f32 {
    if e <= 0.040_45 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    }
}

/// IEC 61966-2-1 encoding (input clamped to 0…1).
#[must_use]
pub fn linear_to_srgb(l: f32) -> f32 {
    let l = l.clamp(0.0, 1.0);
    if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// Mean over tiles of each tile's frame-to-frame jitter (RMS of second differences, stops).
#[must_use]
pub fn tile_jitter(tiles: &[Vec<Option<f64>>]) -> f64 {
    let count = tiles.iter().map(Vec::len).max().unwrap_or(0);
    let mut sum = 0.0;
    let mut n = 0usize;
    for k in 0..count {
        let d2: Vec<f64> = tiles
            .windows(3)
            .filter_map(|w| match w {
                [a, b, c] => match (a.get(k)?, b.get(k)?, c.get(k)?) {
                    (Some(a), Some(b), Some(c)) => Some(a - 2.0 * b + c),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        if !d2.is_empty() {
            sum += (d2.iter().map(|v| v * v).sum::<f64>() / d2.len() as f64).sqrt();
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_bounds() {
        assert!(Grid::new(0, 4).is_none());
        assert!(Grid::new(33, 4).is_none());
        assert_eq!(Grid::new(6, 4).unwrap().tiles(), 24);
    }

    #[test]
    fn corrects_only_the_flickering_region() {
        // Two tiles (left, right). The right one flickers ±0.1 stops, the left one is steady; the
        // whole frame sees half of it.
        let n = 60;
        let flick = |t: usize| if t.is_multiple_of(2) { 0.1 } else { -0.1 };
        let tiles: Vec<Vec<Option<f64>>> = (0..n).map(|t| vec![Some(-3.0), Some(-2.0 + flick(t))]).collect();
        let global: Vec<Option<f64>> = (0..n).map(|t| Some(-2.5 + 0.5 * flick(t))).collect();
        let c = corrections(&tiles, &global, &DeflickerParams::default());
        // Left tile: whole-frame correction (±0.05) is undone regionally; right tile gets the rest.
        let left = c.iter().map(|f| f[0]).collect::<Vec<_>>();
        let right = c.iter().map(|f| f[1]).collect::<Vec<_>>();
        for t in 10..n - 10 {
            assert!((left[t] - 0.5 * flick(t)).abs() < 0.01, "left {t}: {}", left[t]);
            assert!(
                (right[t] + 0.5 * flick(t)).abs() < 0.01,
                "right {t}: {}",
                right[t]
            );
        }
        // After the whole-frame correction (exposure) and the regional one, both tiles are steady.
        let whole = deflicker::corrections(&global, &DeflickerParams::default());
        let after: Vec<Vec<Option<f64>>> = tiles
            .iter()
            .zip(&c)
            .zip(&whole)
            .map(|((t, c), g)| t.iter().zip(c).map(|(v, c)| v.map(|v| v + g + c)).collect())
            .collect();
        assert!(tile_jitter(&tiles) > 0.15);
        assert!(
            tile_jitter(&after[10..n - 10]) < 0.01,
            "{}",
            tile_jitter(&after[10..n - 10])
        );
    }

    #[test]
    fn gains_interpolate_between_tile_centres() {
        let grid = Grid::new(2, 1).unwrap();
        let stops = [0.0, 1.0];
        assert!((gain_at(grid, &stops, 0.0, 0.5) - 1.0).abs() < 1e-12);
        assert!((gain_at(grid, &stops, 0.25, 0.5) - 1.0).abs() < 1e-12);
        assert!((gain_at(grid, &stops, 0.5, 0.5) - 2f64.sqrt()).abs() < 1e-12);
        assert!((gain_at(grid, &stops, 1.0, 0.5) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn srgb_round_trip_and_gain() {
        for v in [0u16, 1, 100, 3000, 32768, 65535] {
            let back = (linear_to_srgb(srgb_to_linear(f32::from(v) / 65535.0)) * 65535.0).round() as u16;
            assert!(back.abs_diff(v) <= 1, "{v} -> {back}");
        }
        // A zero correction leaves the image untouched.
        let mut img: Vec<u16> = (0..4 * 3 * 3).map(|i| (i * 1500) as u16).collect();
        let orig = img.clone();
        apply_srgb16(&mut img, 4, 3, Grid::new(2, 2).unwrap(), &[0.0; 4]);
        assert_eq!(img, orig);
        // One stop doubles linear light.
        let mut img = vec![30000u16; 4 * 4 * 3];
        apply_srgb16(&mut img, 4, 4, Grid::new(1, 1).unwrap(), &[1.0]);
        let lin = srgb_to_linear(f32::from(img[0]) / 65535.0);
        let expect = 2.0 * srgb_to_linear(30000.0 / 65535.0);
        assert!((lin - expect).abs() < 1e-4, "{lin} vs {expect}");
        // Linear images: a plain multiply.
        let mut f = vec![0.25f32; 2 * 2 * 3];
        apply_linear(&mut f, 2, 2, Grid::new(1, 1).unwrap(), &[-1.0]);
        assert!(f.iter().all(|v| (v - 0.125).abs() < 1e-6));
    }
}
