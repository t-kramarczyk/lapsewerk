//! Stabilization: how far each frame moved against the one before, the camera's path over the
//! sequence, and the correction that keeps intentional motion (a slider, a pan) but removes shake
//! (wind on the tripod, a bump, a settling head).
//!
//! Motion between two frames is found by phase correlation on small luminance images: the
//! normalized cross-power spectrum of two shifted images is a pure phase ramp, whose inverse
//! transform is a sharp peak at the shift. It ignores overall brightness changes (flicker, a ramp)
//! and, with a window against the image edges, finds the shift of what most of the picture does.

use crate::smooth;

/// Side of the square luminance images shifts are measured on (a power of two).
pub const SIZE: usize = 512;

/// A luminance image for measuring shifts: `SIZE × SIZE` values, the frame squeezed to a square.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumb {
    pub data: Vec<f32>,
}

impl Thumb {
    /// From an interleaved linear RGB image (3 channels): log luminance, resampled to `SIZE × SIZE`
    /// by box averaging. `None` for an empty or malformed image.
    #[must_use]
    pub fn from_linear_rgb(data: &[f32], width: usize, height: usize, luminance: [f32; 3]) -> Option<Self> {
        if width == 0 || height == 0 || data.len() < width * height * 3 {
            return None;
        }
        let px = data.as_chunks::<3>().0;
        let mut out = vec![0.0f32; SIZE * SIZE];
        for (ty, row) in out.chunks_mut(SIZE).enumerate() {
            let (y0, y1) = (
                ty * height / SIZE,
                ((ty + 1) * height / SIZE).max(ty * height / SIZE + 1),
            );
            for (tx, o) in row.iter_mut().enumerate() {
                let (x0, x1) = (
                    tx * width / SIZE,
                    ((tx + 1) * width / SIZE).max(tx * width / SIZE + 1),
                );
                let (mut sum, mut n) = (0.0f32, 0u32);
                for y in y0..y1.min(height) {
                    for x in x0..x1.min(width) {
                        if let Some(p) = px.get(y * width + x) {
                            sum += luminance[0] * p[0] + luminance[1] * p[1] + luminance[2] * p[2];
                            n += 1;
                        }
                    }
                }
                *o = (sum / n.max(1) as f32).max(1e-5).log2();
            }
        }
        Some(Self { data: out })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct C {
    re: f64,
    im: f64,
}

impl C {
    fn mul(self, o: C) -> C {
        C {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
    fn conj(self) -> C {
        C {
            re: self.re,
            im: -self.im,
        }
    }
}

/// In-place radix-2 FFT of `a` (length a power of two); `inverse` for the inverse (unscaled).
fn fft(a: &mut [C], inverse: bool) {
    let n = a.len();
    if n < 2 {
        return;
    }
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            a.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = 2.0 * std::f64::consts::PI / len as f64 * if inverse { 1.0 } else { -1.0 };
        let w = C {
            re: ang.cos(),
            im: ang.sin(),
        };
        for chunk in a.chunks_mut(len) {
            let mut wk = C { re: 1.0, im: 0.0 };
            let (lo, hi) = chunk.split_at_mut(len / 2);
            for (u, v) in lo.iter_mut().zip(hi.iter_mut()) {
                let t = v.mul(wk);
                *v = C {
                    re: u.re - t.re,
                    im: u.im - t.im,
                };
                *u = C {
                    re: u.re + t.re,
                    im: u.im + t.im,
                };
                wk = wk.mul(w);
            }
        }
        len <<= 1;
    }
}

/// 2-D FFT of a `SIZE × SIZE` image (rows, then columns).
fn fft2(a: &mut [C], inverse: bool) {
    for row in a.chunks_mut(SIZE) {
        fft(row, inverse);
    }
    let mut col = vec![C::default(); SIZE];
    for x in 0..SIZE {
        for (y, c) in col.iter_mut().enumerate() {
            *c = a.get(y * SIZE + x).copied().unwrap_or_default();
        }
        fft(&mut col, inverse);
        for (y, c) in col.iter().enumerate() {
            if let Some(v) = a.get_mut(y * SIZE + x) {
                *v = *c;
            }
        }
    }
}

/// Spectrum of a thumb, mean removed and Hann-windowed (so the image edges do not correlate).
fn spectrum(t: &Thumb) -> Vec<C> {
    let mean = t.data.iter().map(|v| f64::from(*v)).sum::<f64>() / t.data.len().max(1) as f64;
    let hann: Vec<f64> = (0..SIZE)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (SIZE - 1) as f64).cos())
        .collect();
    let mut a: Vec<C> = t
        .data
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let w = hann.get(i % SIZE).copied().unwrap_or(0.0) * hann.get(i / SIZE).copied().unwrap_or(0.0);
            C {
                re: (f64::from(*v) - mean) * w,
                im: 0.0,
            }
        })
        .collect();
    fft2(&mut a, false);
    a
}

/// The shift of `b` against `a`, in thumb pixels (positive: `b`'s content lies right / below
/// `a`'s), and the peak's strength (0…1: how much of the spectrum agrees on it). `None` for thumbs
/// of the wrong size.
///
/// The cross-power spectrum is normalized with a floor (its mean magnitude): fully normalized, the
/// frequencies the window and the picture leave nearly empty count as much as the picture's own
/// and pull fractional shifts towards whole pixels (0.4 px read as 0.02 px; checked against an
/// independent NumPy implementation, where the floored version reads 0.40).
#[must_use]
pub fn shift(a: &Thumb, b: &Thumb) -> Option<(f64, f64, f64)> {
    if a.data.len() != SIZE * SIZE || b.data.len() != SIZE * SIZE {
        return None;
    }
    let (fa, fb) = (spectrum(a), spectrum(b));
    let cross: Vec<C> = fa.iter().zip(&fb).map(|(x, y)| y.mul(x.conj())).collect();
    // Normalized cross-power spectrum, softened: frequencies with little energy (noise, or nothing
    // at all in a smooth picture) would otherwise count as much as the picture's own and pull the
    // peak towards zero shift. `eps` is the mean magnitude.
    let eps = cross
        .iter()
        .map(|p| (p.re * p.re + p.im * p.im).sqrt())
        .sum::<f64>()
        / cross.len().max(1) as f64;
    let mut r: Vec<C> = cross
        .iter()
        .map(|p| {
            let m = (p.re * p.re + p.im * p.im).sqrt() + eps;
            if m > 1e-300 {
                C {
                    re: p.re / m,
                    im: p.im / m,
                }
            } else {
                C::default()
            }
        })
        .collect();
    let spectrum = r.clone();
    // The inverse transform at the peak is at most the sum of the magnitudes (every frequency agreeing).
    let scale = 1.0
        / r.iter()
            .map(|c| (c.re * c.re + c.im * c.im).sqrt())
            .sum::<f64>()
            .max(1e-300);
    fft2(&mut r, true);
    let (mut best, mut at) = (f64::MIN, 0usize);
    for (i, v) in r.iter().enumerate() {
        if v.re > best {
            best = v.re;
            at = i;
        }
    }
    let wrap = |p: usize| {
        if p > SIZE / 2 {
            p as f64 - SIZE as f64
        } else {
            p as f64
        }
    };
    let (mut x, mut y) = (wrap(at % SIZE), wrap(at / SIZE));
    // Sub-pixel: the correlation evaluated on a fine grid around the peak straight from the spectrum
    // (Guizar-Sicairos, Thurman and Fienup 2008): ±1 px in tenths, then ±0.1 px in hundredths.
    for (half, step) in [(1.0, 0.1), (0.1, 0.01)] {
        let (bx, by) = refine(&spectrum, x, y, half, step);
        x = bx;
        y = by;
    }
    Some((x, y, (best * scale).clamp(0.0, 1.0)))
}

/// The point of the finest grid (`x ± half`, `y ± half` in steps of `step`) where the inverse
/// transform of `spectrum` is largest. Separable: the transform along x for every row, then along y.
fn refine(spectrum: &[C], x: f64, y: f64, half: f64, step: f64) -> (f64, f64) {
    let k = (2.0 * half / step).round() as usize + 1;
    let us: Vec<f64> = (0..k).map(|i| x - half + i as f64 * step).collect();
    let vs: Vec<f64> = (0..k).map(|i| y - half + i as f64 * step).collect();
    let freq = |i: usize| {
        if i >= SIZE / 2 {
            i as f64 - SIZE as f64
        } else {
            i as f64
        }
    };
    let tau = 2.0 * std::f64::consts::PI / SIZE as f64;
    let ex: Vec<C> = us
        .iter()
        .flat_map(|u| {
            (0..SIZE).map(move |i| {
                let a = tau * freq(i) * u;
                C {
                    re: a.cos(),
                    im: a.sin(),
                }
            })
        })
        .collect();
    // rows[y][iu] = Σx spectrum[y][x] · e^{i·2π·fx·u}
    let mut rows = vec![C::default(); SIZE * k];
    for (yy, row) in spectrum.chunks(SIZE).enumerate() {
        for iu in 0..k {
            let mut acc = C::default();
            for (xx, v) in row.iter().enumerate() {
                let e = ex.get(iu * SIZE + xx).copied().unwrap_or_default();
                let t = v.mul(e);
                acc.re += t.re;
                acc.im += t.im;
            }
            if let Some(slot) = rows.get_mut(yy * k + iu) {
                *slot = acc;
            }
        }
    }
    let (mut best, mut at) = (f64::MIN, (x, y));
    for v in &vs {
        for (iu, u) in us.iter().enumerate() {
            let mut re = 0.0;
            for yy in 0..SIZE {
                let a = tau * freq(yy) * v;
                let e = C {
                    re: a.cos(),
                    im: a.sin(),
                };
                re += rows.get(yy * k + iu).copied().unwrap_or_default().mul(e).re;
            }
            if re > best {
                best = re;
                at = (*u, *v);
            }
        }
    }
    at
}

/// Per-frame corrections (dx, dy, in thumb pixels) from the shifts between consecutive frames
/// (`steps[i]`: frame i + 1 against frame i; `None` where it could not be measured, counted as no
/// motion): the camera's path smoothed over `smoothing_frames` (Gaussian sigma, outliers resisted),
/// minus the path. Moving each frame by its correction leaves the smooth path.
#[must_use]
pub fn corrections(steps: &[Option<(f64, f64)>], smoothing_frames: f64) -> Vec<(f64, f64)> {
    let n = steps.len() + 1;
    let mut path = Vec::with_capacity(n);
    let (mut x, mut y) = (0.0, 0.0);
    path.push((x, y));
    for s in steps {
        let (dx, dy) = s.unwrap_or((0.0, 0.0));
        x += dx;
        y += dy;
        path.push((x, y));
    }
    let xs: Vec<f64> = path.iter().map(|p| p.0).collect();
    let ys: Vec<f64> = path.iter().map(|p| p.1).collect();
    let sx = smooth::robust_local_linear(&xs, smoothing_frames, 2);
    let sy = smooth::robust_local_linear(&ys, smoothing_frames, 2);
    (0..n)
        .map(|i| {
            let g = |v: &[f64]| v.get(i).copied().unwrap_or(0.0);
            (g(&sx) - g(&xs), g(&sy) - g(&ys))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A textured test scene sampled at an offset: 60 waves of random direction, phase and
    /// wavelength (6 to 120 px), like the mix of fine and coarse detail of a photo.
    fn scene(ox: f64, oy: f64) -> Thumb {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let waves: Vec<(f64, f64, f64, f64)> = (0..60)
            .map(|_| {
                let angle = rnd() * std::f64::consts::TAU;
                let k = std::f64::consts::TAU / (6.0 + 114.0 * rnd());
                (
                    k * angle.cos(),
                    k * angle.sin(),
                    rnd() * std::f64::consts::TAU,
                    0.3 + rnd(),
                )
            })
            .collect();
        let mut data = Vec::with_capacity(SIZE * SIZE);
        for y in 0..SIZE {
            for x in 0..SIZE {
                let (u, v) = (x as f64 - ox, y as f64 - oy);
                let val: f64 = waves
                    .iter()
                    .map(|(kx, ky, ph, a)| a * (kx * u + ky * v + ph).sin())
                    .sum();
                data.push(val as f32);
            }
        }
        Thumb { data }
    }

    #[test]
    fn finds_whole_and_fractional_shifts() {
        let a = scene(0.0, 0.0);
        for (dx, dy) in [
            (3.0, -2.0),
            (0.4, 0.0),
            (-1.3, 2.6),
            (0.0, 0.0),
            (0.25, -0.75),
            (12.0, 7.5),
        ] {
            let b = scene(dx, dy);
            let (x, y, peak) = shift(&a, &b).unwrap();
            eprintln!("({dx}, {dy}) -> ({x:.3}, {y:.3}), peak {peak:.2}");
            assert!(
                (x - dx).abs() < 0.1 && (y - dy).abs() < 0.1,
                "({dx}, {dy}) -> ({x:.3}, {y:.3})"
            );
            assert!(peak > 0.1, "peak {peak}");
        }
    }

    #[test]
    fn brightness_changes_do_not_move_it() {
        let a = scene(0.0, 0.0);
        let mut b = scene(1.0, 1.0);
        for v in &mut b.data {
            *v = *v * 1.3 + 0.8; // brighter and more contrast (log luminance: an exposure change)
        }
        let (x, y, _) = shift(&a, &b).unwrap();
        assert!(
            (x - 1.0).abs() < 0.15 && (y - 1.0).abs() < 0.15,
            "({x:.3}, {y:.3})"
        );
    }

    #[test]
    fn shake_is_removed_and_a_pan_is_kept() {
        // A steady pan of 0.5 px a frame with ±1 px shake on top.
        let n = 80;
        let shake = |i: usize| if i.is_multiple_of(2) { 1.0 } else { -1.0 };
        let positions: Vec<f64> = (0..n).map(|i| 0.5 * i as f64 + shake(i)).collect();
        let steps: Vec<Option<(f64, f64)>> = positions.windows(2).map(|w| Some((w[1] - w[0], 0.0))).collect();
        let c = corrections(&steps, 8.0);
        assert_eq!(c.len(), n);
        let stabilized: Vec<f64> = positions.iter().zip(&c).map(|(p, c)| p + c.0).collect();
        // Interior: the pan remains, the shake is gone.
        for i in 10..n - 10 {
            let expect = 0.5 * i as f64 + stabilized[10] - 5.0;
            assert!(
                (stabilized[i] - expect).abs() < 0.1,
                "{i}: {} vs {expect}",
                stabilized[i]
            );
        }
    }

    #[test]
    fn makes_thumbs_from_rgb() {
        let rgb = vec![0.5f32; 30 * 20 * 3];
        let t = Thumb::from_linear_rgb(&rgb, 30, 20, [0.2627, 0.678, 0.0593]).unwrap();
        assert_eq!(t.data.len(), SIZE * SIZE);
        assert!(t.data.iter().all(|v| (v - (-1.0)).abs() < 1e-4));
        assert!(Thumb::from_linear_rgb(&rgb, 0, 20, [0.3, 0.6, 0.1]).is_none());
    }
}
