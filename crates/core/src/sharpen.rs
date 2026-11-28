//! Output sharpening: an unsharp mask on the finished frame at the video's size.
//!
//! Sharpening while developing hardly shows in a video: the photo is sharpened at its full size
//! and then made ten times smaller (a city series, 540 × 960: detail by the variance of the Laplacian
//! 136 unsharpened, 166 with sharpening 70, 255 in the earlier workflow's film). Sharpened after
//! the scaling, at one pixel, the frame gets the crispness people know from that workflow
//! (amount 1, held to the neighbours: 235).
//!
//! It works on the brightness only (each pixel's channels moved together), so edges get no
//! coloured fringes, and leaves out differences below a threshold, so the noise of a night sky
//! and flat areas stay as they are rather than turning into grain that changes frame to frame
//! (star field, 480 × 270: frame-to-frame difference of the sky 0.99 → 1.15 of 255, from the
//! crisper stars; a threshold of 1000 left 1.14 and cost the day detail; flicker unchanged).

/// Radius of the Gaussian the frame is compared with, in output pixels.
pub const RADIUS: f64 = 1.0;
/// Differences of brightness (of 65535) below this are left alone: noise, smooth gradients.
pub const THRESHOLD: f64 = 600.0;
/// Over this many more steps the sharpening fades in above the threshold (no hard edge between
/// sharpened and untouched).
const FADE: f64 = 1200.0;
/// How far past its neighbours' range a pixel may go, as a share of the contrast there…
const OVERSHOOT: f64 = 0.05;
/// …and at most this share of its own brightness (a star's dark ring stays under a tenth).
const OVERSHOOT_MOST: f64 = 0.1;

/// Sharpens the 16-bit RGB frame `rgb` (`width` pixels wide, rows packed) in place by `amount`
/// (0: unchanged, 1: the difference to the blurred frame added once).
pub fn unsharp(rgb: &mut [u16], width: usize, amount: f64) {
    if !(amount.is_finite() && amount > 0.0) || width == 0 {
        return;
    }
    let px = rgb.len() / 3;
    let height = px / width;
    if height == 0 {
        return;
    }
    let luma: Vec<f64> = rgb
        .as_chunks::<3>()
        .0
        .iter()
        .take(width * height)
        .map(|&[r, g, b]| 0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b))
        .collect();
    let blurred = blur(&luma, width, height, RADIUS);
    for (i, ((p, &y), &bl)) in rgb
        .as_chunks_mut::<3>()
        .0
        .iter_mut()
        .zip(&luma)
        .zip(&blurred)
        .enumerate()
    {
        let d = y - bl;
        let weight = ((d.abs() - THRESHOLD) / FADE).clamp(0.0, 1.0);
        // Never past the darkest or brightest of its neighbours as they were: next to a star or a
        // lamp the mask would dig a dark ring into the sky (Testers: 16 % of bright stars with a
        // ring pixel 8 below the sky, the Pleiades' brightest 38 → 16); an edge stays crisp.
        // A little past them (a twentieth of the contrast there, at most a tenth of the pixel's own
        // brightness) keeps a hard edge crisp.
        let (lo, hi) = neighbours(&luma, width, height, i);
        let room = (OVERSHOOT * (hi - lo)).min(OVERSHOOT_MOST * y);
        let shift = (amount * d * weight).clamp(lo - room - y, hi + room - y);
        if shift == 0.0 {
            continue;
        }
        for c in p.iter_mut() {
            *c = (f64::from(*c) + shift).round().clamp(0.0, 65535.0) as u16;
        }
    }
}

/// The darkest and brightest of `v` in the 3 × 3 neighbourhood of pixel `i` (`width × height`).
fn neighbours(v: &[f64], width: usize, height: usize, i: usize) -> (f64, f64) {
    let (x, y) = (i % width, i / width);
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for yy in y.saturating_sub(1)..=(y + 1).min(height - 1) {
        for xx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
            if let Some(&n) = v.get(yy * width + xx) {
                lo = lo.min(n);
                hi = hi.max(n);
            }
        }
    }
    (lo, hi)
}

/// A separable Gaussian blur of `v` (`width × height`) with `sigma`, edges held.
fn blur(v: &[f64], width: usize, height: usize, sigma: f64) -> Vec<f64> {
    let reach = (3.0 * sigma).ceil() as usize;
    let kernel: Vec<f64> = (0..=reach)
        .map(|d| (-((d * d) as f64) / (2.0 * sigma * sigma)).exp())
        .collect();
    let pass = |src: &[f64], len: usize, count: usize, at: &dyn Fn(usize, usize) -> usize| {
        let mut out = vec![0.0; src.len()];
        for line in 0..count {
            for i in 0..len {
                let (mut sum, mut weights) = (0.0, 0.0);
                for (d, &k) in kernel.iter().enumerate() {
                    for j in [i.saturating_sub(d), (i + d).min(len - 1)]
                        .into_iter()
                        .take(if d == 0 { 1 } else { 2 })
                    {
                        if let Some(x) = src.get(at(line, j)) {
                            sum += k * x;
                            weights += k;
                        }
                    }
                }
                if let Some(o) = out.get_mut(at(line, i)) {
                    *o = sum / weights;
                }
            }
        }
        out
    };
    let rows = pass(v, width, height, &|y, x| y * width + x);
    pass(&rows, height, width, &|x, y| y * width + x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize, f: impl Fn(usize, usize) -> u16) -> Vec<u16> {
        let mut out = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                out.extend([f(x, y); 3]);
            }
        }
        out
    }

    #[test]
    fn an_edge_gets_crisper_and_flat_areas_stay() {
        let mut f = frame(20, 4, |x, _| if x < 10 { 20_000 } else { 40_000 });
        let before = f.clone();
        unsharp(&mut f, 20, 0.6);
        // The dark side darker and the bright side brighter next to the edge…
        let at = |v: &[u16], x: usize| v.get((20 + x) * 3).copied().unwrap_or(0);
        assert!(at(&f, 9) < at(&before, 9) && at(&f, 10) > at(&before, 10));
        // …far from it unchanged.
        assert_eq!(at(&f, 2), at(&before, 2));
        assert_eq!(at(&f, 17), at(&before, 17));
    }

    #[test]
    fn a_bright_star_gets_no_dark_ring() {
        let mut f = frame(9, 9, |x, y| if x == 4 && y == 4 { 60_000 } else { 12_000 });
        unsharp(&mut f, 9, 1.0);
        let darkest = f.iter().copied().min().unwrap_or(0);
        assert!(darkest >= 10_800, "{darkest}");
    }

    #[test]
    fn noise_below_the_threshold_is_left_alone() {
        let mut f = frame(16, 16, |x, y| 30_000 + if (x + y) % 2 == 0 { 200 } else { 0 });
        let before = f.clone();
        unsharp(&mut f, 16, 1.0);
        assert_eq!(f, before);
    }

    #[test]
    fn colours_move_together_and_zero_does_nothing() {
        let mut f: Vec<u16> = (0..10)
            .flat_map(|x| {
                if x < 5 {
                    [10_000, 20_000, 30_000]
                } else {
                    [30_000, 40_000, 50_000]
                }
            })
            .collect();
        let before = f.clone();
        unsharp(&mut f, 10, 0.0);
        assert_eq!(f, before);
        unsharp(&mut f, 10, 0.6);
        for (a, b) in f.as_chunks::<3>().0.iter().zip(before.as_chunks::<3>().0) {
            let shift: Vec<i32> = a
                .iter()
                .zip(b)
                .map(|(x, y)| i32::from(*x) - i32::from(*y))
                .collect();
            assert!(shift.iter().all(|s| (s - shift[0]).abs() <= 1), "{shift:?}");
        }
    }
}
