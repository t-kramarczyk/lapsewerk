//! Holding the blacks when a frame is brightened: day to night shows a night several stops
//! brighter than it was shot, and an exposure gain lifts everything, the darkest parts too: black
//! turns grey and the picture looks flat. Here the part of the gain that lands on black is taken
//! back, in linear light: a pedestal of `(gain − 1) × floor` is subtracted and the rest stretched to
//! keep white, so what was black stays black while mid-tones and highlights keep (almost all) the
//! brightening.

/// Linear light (SDR white 1) of the brightest black as shot that is held: sRGB code 25 of 255. A
/// render lifts a photo's black a little before any gain (a night's foreground at code 4 renders at
/// about 15), so a lower bound would let the black climb again late in the night once the gain is
/// large.
pub const BLACK_FLOOR: f64 = 0.01;

/// Side of the square blocks averaged before the darkest are picked: sensor noise around the black
/// point averages out instead of reading as black.
const BLOCK: usize = 4;

/// The pedestal (linear, SDR white 1) a gain of `stops` put on the black of a frame whose darkest
/// parts (half a percent) are at `darkest` now (encoded 16-bit, as rendered with the gain); 0 for
/// no brightening. What was black as shot (at most [`BLACK_FLOOR`]) is taken back to it.
#[must_use]
pub fn pedestal(stops: f64, darkest: u16) -> f64 {
    if !stops.is_finite() || stops <= 0.0 {
        return 0.0;
    }
    let gain = stops.exp2();
    let as_shot = (decode(f64::from(darkest) / 65535.0) / gain).min(BLACK_FLOOR);
    ((gain - 1.0) * as_shot).min(0.2)
}

/// The darkest half percent of a 16-bit sRGB frame's luminance (encoded), `width` pixels a row,
/// from the means of 4 × 4 pixel blocks (noise around black does not count as blacker than it is).
/// 0 for an empty or malformed frame.
#[must_use]
pub fn darkest(rgb: &[u16], width: usize) -> u16 {
    let (px, _) = rgb.as_chunks::<3>();
    if width == 0 || px.is_empty() {
        return 0;
    }
    let luma = |&[r, g, b]: &[u16; 3]| u64::from(r) * 2126 + u64::from(g) * 7152 + u64::from(b) * 722;
    let cols = width.div_ceil(BLOCK);
    let mut means: Vec<u16> = Vec::with_capacity(px.len() / (BLOCK * BLOCK) + cols);
    let mut sums = vec![(0u64, 0u64); cols];
    for (y, row) in px.chunks(width).enumerate() {
        for (x, p) in row.iter().enumerate() {
            if let Some(s) = sums.get_mut(x / BLOCK) {
                s.0 += luma(p);
                s.1 += 1;
            }
        }
        if y % BLOCK == BLOCK - 1 || row.len() < width {
            flush(&mut sums, &mut means);
        }
    }
    flush(&mut sums, &mut means);
    if means.is_empty() {
        return 0;
    }
    let k = means.len() / 100;
    means.select_nth_unstable(k);
    means.get(k).copied().unwrap_or(0)
}

/// Moves the block sums of a band of rows into `means` (as encoded luminance) and clears them.
fn flush(sums: &mut [(u64, u64)], means: &mut Vec<u16>) {
    for s in sums.iter_mut() {
        if let Some(mean) = s.0.checked_div(s.1) {
            means.push(u16::try_from(mean / 10_000).unwrap_or(u16::MAX));
        }
        *s = (0, 0);
    }
}

/// sRGB curve, encoded 0..1 → linear 0..1.
fn decode(e: f64) -> f64 {
    if e <= 0.040_45 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear 0..1 → sRGB curve, encoded 0..1.
fn encode(l: f64) -> f64 {
    let l = l.clamp(0.0, 1.0);
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// Takes `pedestal` (linear, SDR white 1) off a 16-bit sRGB-encoded frame (3 samples a pixel; HDR
/// frames carry `value / peak` encoded, so `peak` scales the pedestal to them). White stays white.
pub fn hold(rgb: &mut [u16], pedestal: f64, peak: f64) {
    let peak = if peak.is_finite() && peak >= 1.0 {
        peak
    } else {
        1.0
    };
    let d = (pedestal / peak).clamp(0.0, 0.5);
    if d <= 0.0 {
        return;
    }
    let table: Vec<u16> = (0..=u16::MAX)
        .map(|v| {
            let x = decode(f64::from(v) / 65535.0);
            let y = ((x - d) / (1.0 - d)).max(0.0);
            (encode(y) * 65535.0).round().clamp(0.0, 65535.0) as u16
        })
        .collect();
    for v in rgb.iter_mut() {
        *v = table.get(usize::from(*v)).copied().unwrap_or(*v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(linear: f64) -> u16 {
        (encode(linear) * 65535.0).round() as u16
    }

    #[test]
    fn black_stays_black_and_the_brightening_stays() {
        // A night brightened 2.5 stops: black (0.001 as shot) became grey, mid-grey brighter.
        let stops = 2.5;
        let gain = 2f64.powf(stops);
        let mut px = Vec::new();
        for _ in 0..300 {
            px.extend_from_slice(&[code(0.001 * gain); 3]);
        }
        for _ in 0..700 {
            px.extend_from_slice(&[code(0.01 * gain); 3]);
        }
        px.extend_from_slice(&[65535; 3]);
        let p = pedestal(stops, darkest(&px, 1));
        hold(&mut px, p, 1.0);
        let lin = |v: u16| decode(f64::from(v) / 65535.0);
        // Black is back near what it was as shot...
        assert!(lin(px[0]) < 0.0015, "{}", lin(px[0]));
        // ...a dark mid-tone keeps nearly all its brightening, white stays white.
        assert!(lin(px[900]) > 0.01 * gain * 0.85, "{}", lin(px[900]));
        assert_eq!(px[px.len() - 1], 65535);
        // No brightening, nothing changes.
        assert_eq!(pedestal(0.0, 1000), 0.0);
        assert_eq!(pedestal(-1.0, 1000), 0.0);
        let mut same = vec![1000u16, 30_000];
        hold(&mut same, 0.0, 1.0);
        assert_eq!(same, [1000, 30_000]);
        assert_eq!(darkest(&[], 4), 0);
        assert_eq!(darkest(&[5, 5, 5], 0), 0);
    }

    #[test]
    fn noise_around_black_is_not_read_as_blacker() {
        // A 64 × 64 frame of black at code 3000 with ± 2500 of noise in a checker pattern.
        let mut px = Vec::new();
        for y in 0..64 {
            for x in 0..64 {
                let v = if (x + y) % 2 == 0 { 500 } else { 5500 };
                px.extend_from_slice(&[v; 3]);
            }
        }
        let d = darkest(&px, 64);
        assert!((2900..=3100).contains(&d), "{d}");
    }
}
