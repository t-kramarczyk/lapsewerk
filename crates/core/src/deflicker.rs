//! Deflicker: measuring developed frames and correcting their brightness.
//!
//! Flicker is frame-to-frame brightness noise: aperture blades that do not close to exactly the same
//! size, shutter timing, auto exposure, mains-powered lights. The intended change of light (sunset,
//! a ramp) is slow. So:
//!
//! 1. [`measure`] each developed frame: a robust mean brightness in stops.
//! 2. [`corrections`] fits a smooth, outlier-resistant curve through those measurements and returns,
//!    per frame, the exposure offset that moves the frame onto the curve.
//! 3. Apply the offsets as exposure, develop again, and repeat if needed: a second pass removes what
//!    tone curves and clipping made non-linear. [`MeasureOptions::roi`] restricts measuring to a
//!    reference area (e.g. a sky without moving clouds or a foreground without passing cars).
//!
//! Development is not linear, so one pass rarely gets every frame within a hundredth of a stop;
//! two passes usually do.

use crate::smooth;

/// A rectangle in normalized image coordinates (0..1, origin top left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// Smallest side a measuring area may have (5 % of the frame): measuring renders are small
    /// (768 px), and a smaller area holds too few pixels for a steady measurement.
    pub const MIN_SIDE: f32 = 0.05;

    /// A measuring area inside the frame: all values finite, at least [`Self::MIN_SIDE`] wide and
    /// high, not reaching past the frame's right or bottom edge.
    #[must_use]
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Option<Self> {
        let inside = |start: f32, size: f32| {
            start.is_finite()
                && size.is_finite()
                && start >= 0.0
                && size >= Self::MIN_SIDE
                && start + size <= 1.0 + 1e-6
        };
        (inside(x, width) && inside(y, height)).then_some(Self { x, y, width, height })
    }
}

/// A borrowed, interleaved, linear-light float image (3 or 4 channels, RGB first).
#[derive(Debug, Clone, Copy)]
pub struct ImageView<'a> {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub data: &'a [f32],
}

/// How frames are measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeasureOptions {
    /// Luminance weights of R, G, B for the image's working space (Rec. 709 by default).
    pub luminance: [f32; 3],
    /// Pixels with any channel at or above this are clipped and ignored (1.0 = white).
    pub clip: f32,
    /// Pixels with luminance at or below this are noise and ignored.
    pub floor: f32,
    /// Fractions of the darkest and brightest valid samples left out of the mean.
    pub trim: f32,
    /// Measure only inside this area; `None` measures the whole frame.
    pub roi: Option<Rect>,
    /// Upper bound on sampled pixels; larger images are sampled on a regular grid.
    pub max_samples: usize,
}

impl Default for MeasureOptions {
    fn default() -> Self {
        Self {
            luminance: [0.2126, 0.7152, 0.0722],
            clip: 0.98,
            floor: 1e-4,
            trim: 0.02,
            roi: None,
            max_samples: 1 << 18,
        }
    }
}

/// The brightness of one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measurement {
    /// Trimmed mean of log2 luminance.
    pub stops: f64,
    /// Fraction of the sampled pixels that were neither clipped nor below the floor.
    pub valid_fraction: f64,
}

/// Measures `image`. `None` when the image is malformed or too few pixels (under 1 %) are usable.
#[must_use]
pub fn measure(image: ImageView<'_>, opts: &MeasureOptions) -> Option<Measurement> {
    let ImageView {
        width,
        height,
        channels,
        data,
    } = image;
    if width == 0 || height == 0 || !(3..=4).contains(&channels) {
        return None;
    }
    if data.len() < width.checked_mul(height)?.checked_mul(channels)? {
        return None;
    }
    let (x0, y0, x1, y1) = roi_pixels(opts.roi, width, height)?;
    let area = (x1 - x0) * (y1 - y0);
    let step = step_for(area, opts.max_samples.max(1));
    let [wr, wg, wb] = opts.luminance;

    let mut samples: Vec<f32> = Vec::with_capacity(area / (step * step) + 1);
    let mut total = 0usize;
    for y in (y0..y1).step_by(step) {
        let row = y * width;
        for x in (x0..x1).step_by(step) {
            total += 1;
            let i = (row + x) * channels;
            let Some(&[r, g, b]) = data.get(i..i + 3).and_then(|s| <&[f32; 3]>::try_from(s).ok()) else {
                continue;
            };
            if !(r.is_finite() && g.is_finite() && b.is_finite()) {
                continue;
            }
            if r >= opts.clip || g >= opts.clip || b >= opts.clip {
                continue;
            }
            let lum = wr * r + wg * g + wb * b;
            if lum > opts.floor {
                samples.push(lum.log2());
            }
        }
    }
    if total == 0 || samples.len() * 100 < total {
        return None;
    }
    samples.sort_unstable_by(f32::total_cmp);
    let trim = opts.trim.clamp(0.0, 0.45);
    let cut = (samples.len() as f32 * trim) as usize;
    let kept = samples.get(cut..samples.len() - cut)?;
    if kept.is_empty() {
        return None;
    }
    let sum: f64 = kept.iter().map(|&v| f64::from(v)).sum();
    Some(Measurement {
        stops: sum / kept.len() as f64,
        valid_fraction: samples.len() as f64 / total as f64,
    })
}

/// Perceived brightness of `image` in stops: log2 of the mean sRGB-encoded luminance (encoded values
/// are close to how bright things look). Unlike [`measure`], dark pixels count as little as they are
/// seen: in a frame darkened by several stops (a night), the log mean is ruled by near-black pixels
/// whose tiny, invisible changes it magnifies. For reporting flicker as seen; corrections still come
/// from [`measure`]. `None` for a malformed or black image.
#[must_use]
pub fn perceived(image: ImageView<'_>, opts: &MeasureOptions) -> Option<f64> {
    let ImageView {
        width,
        height,
        channels,
        data,
    } = image;
    if width == 0 || height == 0 || !(3..=4).contains(&channels) {
        return None;
    }
    if data.len() < width.checked_mul(height)?.checked_mul(channels)? {
        return None;
    }
    let (x0, y0, x1, y1) = roi_pixels(opts.roi, width, height)?;
    let step = step_for((x1 - x0) * (y1 - y0), opts.max_samples.max(1));
    let [wr, wg, wb] = opts.luminance;
    let (mut sum, mut n) = (0.0f64, 0usize);
    for y in (y0..y1).step_by(step) {
        for x in (x0..x1).step_by(step) {
            let i = (y * width + x) * channels;
            let Some(&[r, g, b]) = data.get(i..i + 3).and_then(|s| <&[f32; 3]>::try_from(s).ok()) else {
                continue;
            };
            let lum = wr * r + wg * g + wb * b;
            if lum.is_finite() {
                sum += f64::from(crate::regional::linear_to_srgb(lum));
                n += 1;
            }
        }
    }
    let mean = sum / n.max(1) as f64;
    (n > 0 && mean > 0.0).then(|| mean.log2())
}

fn roi_pixels(roi: Option<Rect>, width: usize, height: usize) -> Option<(usize, usize, usize, usize)> {
    let Some(r) = roi else {
        return Some((0, 0, width, height));
    };
    let finite = [r.x, r.y, r.width, r.height].iter().all(|v| v.is_finite());
    if !finite {
        return None;
    }
    let to_px = |v: f32, size: usize| ((v.clamp(0.0, 1.0) * size as f32).round() as usize).min(size);
    let (x0, x1) = (to_px(r.x, width), to_px(r.x + r.width, width));
    let (y0, y1) = (to_px(r.y, height), to_px(r.y + r.height, height));
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

/// Grid step so that about `max` pixels of `area` are sampled.
fn step_for(area: usize, max: usize) -> usize {
    if area <= max {
        return 1;
    }
    ((area as f64 / max as f64).sqrt().ceil() as usize).max(1)
}

/// How strongly brightness is smoothed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeflickerParams {
    /// Width of the smoothing in frames (Gaussian sigma). Larger removes slower flicker but follows
    /// fast real changes (a cloud shadow) less closely. 5–15 suits most sequences.
    pub smoothing_frames: f64,
    /// Outlier-rejection rounds; frames with a flash or headlights are then corrected towards their
    /// neighbours instead of pulling the neighbours.
    pub robust_iterations: u32,
    /// Largest correction applied to a frame, in stops.
    pub max_correction_stops: f64,
}

impl Default for DeflickerParams {
    fn default() -> Self {
        Self {
            smoothing_frames: 8.0,
            robust_iterations: 3,
            max_correction_stops: 1.5,
        }
    }
}

/// How much wider than the smoothing the local parabola of [`corrections`] is: a parabola follows
/// flicker more closely than a line of the same width; half as wide again, it leaves as little.
const QUADRATIC_WIDTH: f64 = 1.5;

/// Per-frame exposure corrections in stops that move each measured frame onto the smoothed curve.
///
/// Frames without a measurement (`None`) are left out of the fit and get 0.
#[must_use]
pub fn corrections(measured: &[Option<f64>], params: &DeflickerParams) -> Vec<f64> {
    let values: Vec<f64> = measured
        .iter()
        .map(|m| m.filter(|v| v.is_finite()).unwrap_or(f64::NAN))
        .collect();
    // A local parabola, half as wide again as the smoothing (as quiet on flicker): it follows the
    // knee where a ramp ends and a moonrise, which a local line rounded off or pulled forward (TNT's
    // knees pumped 0.30 stops, now 0.14; the moonrise's swell 0.035 is gone; a fast ramp without
    // EXIF 0.0031 -> 0.0007 stops of flicker).
    let target = smooth::robust_local_quadratic(
        &values,
        params.smoothing_frames * QUADRATIC_WIDTH,
        params.robust_iterations,
    );
    let limit = if params.max_correction_stops.is_finite() {
        params.max_correction_stops.abs()
    } else {
        0.0
    };
    values
        .iter()
        .zip(&target)
        .map(|(v, t)| {
            let c = t - v;
            if c.is_finite() {
                c.clamp(-limit, limit)
            } else {
                0.0
            }
        })
        .collect()
}

/// How much of an exposure correction shows in the measured brightness: the least-squares slope of
/// the measured change (`after − before`) on the `applied` corrections, clamped to 0.2…3.
///
/// The tone curve bends exposure changes: in its toe (a dark scene) a correction of x stops moves the
/// rendered result by more than x, in its shoulder by less. A check pass divides what is left by
/// this gain, so the second correction lands instead of overshooting. Frames where either measurement
/// is missing are left out; `None` when the corrections are too small to tell.
#[must_use]
pub fn correction_gain(before: &[Option<f64>], after: &[Option<f64>], applied: &[f64]) -> Option<f64> {
    let (dc, ac) = fast_parts(before, after, applied);
    slope(dc.iter().zip(&ac).map(|(d, c)| (*d, *c, 1.0)))
}

/// [`correction_gain`] per frame, from the frames within about `sigma` frames of it (Gaussian
/// weights): where a sequence moves along the tone curve (night in the toe, day in the middle), a
/// correction acts differently in different parts of it. Frames whose neighbourhood tells too little
/// get the whole sequence's gain, and 1 when even that is unknown.
#[must_use]
pub fn correction_gains(
    before: &[Option<f64>],
    after: &[Option<f64>],
    applied: &[f64],
    sigma: f64,
) -> Vec<f64> {
    let (dc, ac) = fast_parts(before, after, applied);
    let whole = slope(dc.iter().zip(&ac).map(|(d, c)| (*d, *c, 1.0))).unwrap_or(1.0);
    let radius = (3.0 * sigma.max(1.0)).ceil() as usize;
    (0..dc.len())
        .map(|i| {
            let lo = i.saturating_sub(radius);
            let hi = (i + radius).min(dc.len().saturating_sub(1));
            let near = (lo..=hi).filter_map(|j| {
                let (d, c) = (dc.get(j)?, ac.get(j)?);
                let x = j.abs_diff(i) as f64 / sigma.max(1.0);
                Some((*d, *c, (-0.5 * x * x).exp()))
            });
            slope(near).unwrap_or(whole)
        })
        .collect()
}

/// Fast parts of the measured change and of the applied corrections: slow changes (a day-to-night
/// offset added between the passes, the scene's own trend) removed by a local linear fit, since only
/// the frame-to-frame part says how corrections act.
fn fast_parts(before: &[Option<f64>], after: &[Option<f64>], applied: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let n = before.len().min(after.len()).min(applied.len());
    let change: Vec<f64> = (0..n)
        .map(
            |i| match (before.get(i).copied().flatten(), after.get(i).copied().flatten()) {
                (Some(b), Some(a)) if (a - b).is_finite() => a - b,
                _ => f64::NAN,
            },
        )
        .collect();
    let applied: Vec<f64> = applied
        .iter()
        .take(n)
        .map(|c| if c.is_finite() { *c } else { f64::NAN })
        .collect();
    let fast = |v: &[f64]| -> Vec<f64> {
        let trend = smooth::local_linear(v, None, 8.0);
        v.iter().zip(&trend).map(|(x, t)| x - t).collect()
    };
    (fast(&change), fast(&applied))
}

/// Weighted least-squares slope through the origin of (change, correction, weight) triples,
/// clamped to 0.2…3; `None` when the corrections are too small to tell (under about 0.005 stops RMS).
fn slope(points: impl Iterator<Item = (f64, f64, f64)>) -> Option<f64> {
    let (mut sxy, mut sxx, mut sw) = (0.0, 0.0, 0.0);
    let mut count = 0usize;
    for (d, c, w) in points {
        if d.is_finite() && c.is_finite() && w > 0.0 {
            sxy += w * d * c;
            sxx += w * c * c;
            sw += w;
            count += 1;
        }
    }
    if count < 3 || sw <= 0.0 || sxx / sw < 2.5e-5 {
        return None;
    }
    Some((sxy / sxx).clamp(0.2, 3.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measuring_areas_stay_inside_the_frame() {
        assert!(Rect::new(0.0, 0.0, 1.0, 0.4).is_some());
        assert!(Rect::new(0.25, 0.5, 0.75, 0.5).is_some());
        assert!(Rect::new(0.5, 0.0, 0.6, 1.0).is_none(), "past the right edge");
        assert!(Rect::new(0.0, 0.0, 1.0, 0.01).is_none(), "too thin");
        assert!(Rect::new(-0.1, 0.0, 0.5, 0.5).is_none());
        assert!(Rect::new(0.0, f32::NAN, 0.5, 0.5).is_none());
    }

    #[test]
    fn a_reference_area_ignores_what_changes_outside_it() {
        // Sky (top half) steady at 0.25; foreground (bottom half) flickering between frames.
        let (w, h) = (40, 20);
        let frame = |ground: f32| -> Vec<f32> {
            (0..w * h)
                .flat_map(|i| {
                    let v = if i / w < h / 2 { 0.25 } else { ground };
                    [v, v, v]
                })
                .collect()
        };
        let sky = MeasureOptions {
            roi: Rect::new(0.0, 0.0, 1.0, 0.5),
            ..Default::default()
        };
        let whole = MeasureOptions::default();
        let (a, b) = (frame(0.05), frame(0.2));
        let m = |data: &[f32], o: &MeasureOptions| {
            let view = ImageView {
                width: w,
                height: h,
                channels: 3,
                data,
            };
            measure(view, o).unwrap().stops
        };
        assert!((m(&a, &sky) - m(&b, &sky)).abs() < 1e-9);
        assert!((m(&a, &sky) - (-2.0)).abs() < 1e-6);
        assert!((m(&a, &whole) - m(&b, &whole)).abs() > 0.5);
    }

    fn flat(width: usize, height: usize, value: f32) -> Vec<f32> {
        vec![value; width * height * 3]
    }

    #[test]
    fn measures_a_flat_grey() {
        let data = flat(64, 48, 0.25);
        let m = measure(
            ImageView {
                width: 64,
                height: 48,
                channels: 3,
                data: &data,
            },
            &MeasureOptions::default(),
        )
        .unwrap();
        assert!((m.stops - -2.0).abs() < 1e-5, "{m:?}");
        assert!((m.valid_fraction - 1.0).abs() < 1e-12);
    }

    #[test]
    fn one_stop_brighter_measures_one_stop_more() {
        let a = flat(100, 100, 0.1);
        let b = flat(100, 100, 0.2);
        let o = MeasureOptions::default();
        let ma = measure(
            ImageView {
                width: 100,
                height: 100,
                channels: 3,
                data: &a,
            },
            &o,
        )
        .unwrap();
        let mb = measure(
            ImageView {
                width: 100,
                height: 100,
                channels: 3,
                data: &b,
            },
            &o,
        )
        .unwrap();
        assert!((mb.stops - ma.stops - 1.0).abs() < 1e-5);
    }

    #[test]
    fn clipped_and_black_pixels_are_ignored() {
        let mut data = flat(10, 10, 0.5);
        for px in data.chunks_mut(3).take(30) {
            px.copy_from_slice(&[1.0, 1.0, 1.0]);
        }
        for px in data.chunks_mut(3).skip(30).take(20) {
            px.copy_from_slice(&[0.0, 0.0, 0.0]);
        }
        let m = measure(
            ImageView {
                width: 10,
                height: 10,
                channels: 3,
                data: &data,
            },
            &MeasureOptions::default(),
        )
        .unwrap();
        assert!((m.stops - -1.0).abs() < 1e-5);
        assert!((m.valid_fraction - 0.5).abs() < 1e-12);
    }

    #[test]
    fn roi_measures_only_its_area_and_rgba_works() {
        // Left half 0.5, right half 0.125, RGBA.
        let (w, h) = (20, 10);
        let mut data = vec![1.0f32; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.5 } else { 0.125 };
                let i = (y * w + x) * 4;
                data[i..i + 3].copy_from_slice(&[v, v, v]);
            }
        }
        let opts = MeasureOptions {
            roi: Some(Rect {
                x: 0.5,
                y: 0.0,
                width: 0.5,
                height: 1.0,
            }),
            ..Default::default()
        };
        let m = measure(
            ImageView {
                width: w,
                height: h,
                channels: 4,
                data: &data,
            },
            &opts,
        )
        .unwrap();
        assert!((m.stops - -3.0).abs() < 1e-5, "{m:?}");
    }

    #[test]
    fn sampling_keeps_large_images_bounded_and_accurate() {
        let (w, h) = (2000, 1500);
        let data = flat(w, h, 0.3);
        let opts = MeasureOptions {
            max_samples: 10_000,
            ..Default::default()
        };
        let m = measure(
            ImageView {
                width: w,
                height: h,
                channels: 3,
                data: &data,
            },
            &opts,
        )
        .unwrap();
        assert!((m.stops - f64::from(0.3f32.log2())).abs() < 1e-5);
    }

    #[test]
    fn malformed_images_are_rejected() {
        let o = MeasureOptions::default();
        let data = flat(4, 4, 0.5);
        assert!(
            measure(
                ImageView {
                    width: 0,
                    height: 4,
                    channels: 3,
                    data: &data
                },
                &o
            )
            .is_none()
        );
        assert!(
            measure(
                ImageView {
                    width: 4,
                    height: 4,
                    channels: 2,
                    data: &data
                },
                &o
            )
            .is_none()
        );
        assert!(
            measure(
                ImageView {
                    width: 5,
                    height: 4,
                    channels: 3,
                    data: &data
                },
                &o
            )
            .is_none()
        );
        assert!(
            measure(
                ImageView {
                    width: usize::MAX,
                    height: 4,
                    channels: 3,
                    data: &data
                },
                &o
            )
            .is_none()
        );
        let empty_roi = MeasureOptions {
            roi: Some(Rect {
                x: 0.5,
                y: 0.5,
                width: 0.0,
                height: 0.5,
            }),
            ..o
        };
        assert!(
            measure(
                ImageView {
                    width: 4,
                    height: 4,
                    channels: 3,
                    data: &data
                },
                &empty_roi
            )
            .is_none()
        );
        let black = flat(4, 4, 0.0);
        assert!(
            measure(
                ImageView {
                    width: 4,
                    height: 4,
                    channels: 3,
                    data: &black
                },
                &o
            )
            .is_none()
        );
    }

    #[test]
    fn corrections_remove_flicker_and_keep_the_trend() {
        // A sunset: −0.02 stops per frame, with ±0.15 stop flicker and one flash.
        let n = 200;
        let trend: Vec<f64> = (0..n).map(|i| -0.02 * i as f64).collect();
        let flicker = |i: usize| ((i * 7919) % 13) as f64 / 12.0 * 0.3 - 0.15;
        let mut measured: Vec<Option<f64>> = (0..n).map(|i| Some(trend[i] + flicker(i))).collect();
        measured[100] = Some(trend[100] + 2.0);
        measured[50] = None;
        let corr = corrections(&measured, &DeflickerParams::default());
        assert_eq!(corr[50], 0.0);
        let mut worst: f64 = 0.0;
        for i in (0..n).filter(|&i| i != 50 && i != 100) {
            let corrected = measured[i].unwrap() + corr[i];
            worst = worst.max((corrected - trend[i]).abs());
        }
        let at = (0..n)
            .filter(|&i| i != 50 && i != 100)
            .max_by(|a, b| {
                (measured[*a].unwrap() + corr[*a] - trend[*a])
                    .abs()
                    .total_cmp(&(measured[*b].unwrap() + corr[*b] - trend[*b]).abs())
            })
            .unwrap_or(0);
        assert!(worst < 0.06, "residual flicker {worst} at {at}");
        // The flash is pulled back as far as the limit allows.
        assert!((corr[100] + 1.5).abs() < 1e-9, "{}", corr[100]);
    }

    #[test]
    fn corrections_edge_cases() {
        let p = DeflickerParams::default();
        assert!(corrections(&[], &p).is_empty());
        assert_eq!(corrections(&[None, None], &p), vec![0.0, 0.0]);
        assert_eq!(corrections(&[Some(f64::NAN), Some(1.0)], &p)[0], 0.0);
        let bad = DeflickerParams {
            max_correction_stops: f64::NAN,
            ..p
        };
        assert!(
            corrections(&[Some(0.0), Some(1.0), Some(0.0)], &bad)
                .iter()
                .all(|c| *c == 0.0)
        );
    }

    #[test]
    fn gain_of_damped_corrections() {
        let applied: Vec<f64> = (0..40).map(|i| if i % 2 == 0 { 0.1 } else { -0.1 }).collect();
        let before: Vec<Option<f64>> = (0..40).map(|i| Some(i as f64 * 0.01)).collect();
        let after: Vec<Option<f64>> = before
            .iter()
            .zip(&applied)
            .map(|(b, c)| b.map(|b| b + 0.3 + 0.55 * c))
            .collect();
        let g = correction_gain(&before, &after, &applied).unwrap();
        assert!((g - 0.55).abs() < 1e-9, "{g}");
        // Corrections too small to tell, missing frames, clamping.
        assert_eq!(correction_gain(&before, &after, &[0.0; 40]), None);
        assert_eq!(correction_gain(&[None; 40], &after, &applied), None);
        let wild: Vec<Option<f64>> = before
            .iter()
            .zip(&applied)
            .map(|(b, c)| b.map(|b| b + 5.0 * c))
            .collect();
        assert_eq!(correction_gain(&before, &wild, &applied), Some(3.0));
    }

    #[test]
    fn gains_follow_the_tone_curve_along_the_sequence() {
        // First half: corrections act at 150 % (night, toe); second half at 80 % (day).
        let n = 200;
        let applied: Vec<f64> = (0..n).map(|i| if i % 2 == 0 { 0.05 } else { -0.05 }).collect();
        let before: Vec<Option<f64>> = (0..n).map(|i| Some(-6.0 + 0.02 * i as f64)).collect();
        let after: Vec<Option<f64>> = before
            .iter()
            .zip(&applied)
            .enumerate()
            .map(|(i, (b, c))| {
                b.map(|b| b + 1.5 * f64::from(u8::from(i >= 100)) + if i < 100 { 1.5 * c } else { 0.8 * c })
            })
            .collect();
        let g = correction_gains(&before, &after, &applied, 15.0);
        assert!((g[30] - 1.5).abs() < 0.02, "{}", g[30]);
        assert!((g[170] - 0.8).abs() < 0.02, "{}", g[170]);
        // A step offset between the passes (a day-to-night change) does not disturb it.
        let whole = correction_gain(&before, &after, &applied).unwrap();
        assert!((0.8..=1.5).contains(&whole), "{whole}");
    }
}
