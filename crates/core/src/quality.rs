//! What can spoil a sequence, found before rendering: frames that stand out (a flash, headlights,
//! someone in front of the lens), highlights burnt out, frames missing.
//!
//! - [`outliers`]: frames much brighter or darker than their neighbours while the neighbours agree
//!   with each other (a steady scene, one odd frame) — at most two in a row, longer runs are a real
//!   change of light.
//! - [`clipped_fraction`]: the share of a frame at white (any channel), from a measuring render.
//! - [`missing_frames`]: where the capture pauses for longer than the interval, and how many frames
//!   would fill the pause (so the video does not jump there).

use crate::deflicker::ImageView;

/// Frames whose brightness (`values`, stops) differs by more than `threshold` stops from what the
/// three frames on each side predict (a robust line through them, so a steady change of light such
/// as a sunset does not count), while those neighbours lie close to that line (less than half of
/// it: the scene is steady there).
#[must_use]
pub fn outliers(values: &[Option<f64>], threshold: f64) -> Vec<usize> {
    // Twice: odd frames found the first time no longer count as neighbours (two headlight frames
    // would otherwise tilt the line of the frames after them).
    let first = odd_frames(values, threshold, &[]);
    without_runs(&odd_frames(values, threshold, &first))
}

/// One pass of [`outliers`], leaving the frames in `skip` out as neighbours.
fn odd_frames(values: &[Option<f64>], threshold: f64, skip: &[usize]) -> Vec<usize> {
    let n = values.len();
    let mut out = Vec::new();
    for i in 0..n {
        let Some(v) = values.get(i).copied().flatten().filter(|v| v.is_finite()) else {
            continue;
        };
        let around: Vec<(f64, f64)> = (i.saturating_sub(3)..(i + 4).min(n))
            .filter(|&j| j != i && !skip.contains(&j))
            .filter_map(|j| {
                Some((
                    j as f64 - i as f64,
                    values.get(j).copied().flatten().filter(|v| v.is_finite())?,
                ))
            })
            .collect();
        if around.len() < 3 {
            continue;
        }
        let Some((at_i, spread)) = robust_line(&around) else {
            continue;
        };
        // An odd frame stands out from both sides; the first frame after a lasting change of
        // light matches the side after it.
        let side = |range: std::ops::Range<usize>| -> Option<f64> {
            let mut v: Vec<f64> = range
                .filter(|j| !skip.contains(j))
                .filter_map(|j| values.get(j).copied().flatten())
                .filter(|v| v.is_finite())
                .collect();
            v.sort_by(f64::total_cmp);
            v.get(v.len() / 2).copied()
        };
        let apart = |m: Option<f64>| m.is_none_or(|m| (v - m).abs() > threshold);
        // Frames next to it that look like it: three alike in a row are a change of light.
        let alike = |j: usize| {
            values
                .get(j)
                .copied()
                .flatten()
                .is_some_and(|w| (w - v).abs() < threshold / 2.0)
        };
        let before = (1..=2).take_while(|d| i >= *d && alike(i - d)).count();
        let after = (1..=2).take_while(|d| alike(i + d)).count();
        if before + after >= 2 {
            continue;
        }
        // The first or last frame has neighbours on one side only: a lasting change of light
        // that begins there cannot be told from an odd frame, so it must stand out twice as far
        // (a flash does; the light turning at the end of a series does not).
        let (before_m, after_m) = (side(i.saturating_sub(3)..i), side(i + 1..(i + 4).min(n)));
        let needed = if before_m.is_none() || after_m.is_none() {
            2.0 * threshold
        } else {
            threshold
        };
        if (v - at_i).abs() > needed && spread < threshold / 2.0 && apart(before_m) && apart(after_m) {
            out.push(i);
        }
    }
    out
}

/// `found` without runs of three or more frames: a run is a change of light, not an odd frame.
fn without_runs(found: &[usize]) -> Vec<usize> {
    let mut kept = Vec::new();
    let mut k = 0;
    while k < found.len() {
        let start = k;
        while k + 1 < found.len() && found.get(k + 1).copied() == found.get(k).map(|v| v + 1) {
            k += 1;
        }
        if k - start < 2 {
            kept.extend(found.get(start..=k).unwrap_or_default());
        }
        k += 1;
    }
    kept
}

/// A Theil–Sen line through `points` (x, y): its value at x = 0, and the median distance of the
/// points from it.
fn robust_line(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    let median = |mut v: Vec<f64>| -> Option<f64> {
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied()
    };
    let mut slopes = Vec::new();
    for (k, a) in points.iter().enumerate() {
        for b in points.iter().skip(k + 1) {
            if (b.0 - a.0).abs() > 0.0 {
                slopes.push((b.1 - a.1) / (b.0 - a.0));
            }
        }
    }
    let slope = median(slopes)?;
    let intercept = median(points.iter().map(|(x, y)| y - slope * x).collect())?;
    let spread = median(
        points
            .iter()
            .map(|(x, y)| (y - slope * x - intercept).abs())
            .collect(),
    )?;
    Some((intercept, spread))
}

/// The share of pixels of a (linear, 0..1) image with a channel at or above `clip`, from up to
/// about 65 000 pixels on a regular grid; `None` for an empty or malformed image.
#[must_use]
pub fn clipped_fraction(image: ImageView<'_>, clip: f32) -> Option<f32> {
    let ImageView {
        width,
        height,
        channels,
        data,
    } = image;
    if width == 0 || height == 0 || !(3..=4).contains(&channels) || data.len() < width * height * channels {
        return None;
    }
    let step = ((width * height) as f64 / 65_536.0).sqrt().ceil().max(1.0) as usize;
    let (mut clipped, mut seen) = (0usize, 0usize);
    for y in (0..height).step_by(step) {
        for x in (0..width).step_by(step) {
            let i = (y * width + x) * channels;
            let Some(px) = data.get(i..i + 3) else { continue };
            seen += 1;
            if px.iter().any(|v| *v >= clip) {
                clipped += 1;
            }
        }
    }
    (seen > 0).then(|| clipped as f32 / seen as f32)
}

/// Frames to put in where the capture paused: (before which frame, how many), for pauses longer
/// than 1.5 intervals, at most `max` frames each (a longer pause is a cut, not a gap to fill).
#[must_use]
pub fn missing_frames(times_ms: &[Option<i64>], interval_ms: i64, max: usize) -> Vec<(usize, usize)> {
    if interval_ms <= 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 1..times_ms.len() {
        let (Some(Some(a)), Some(Some(b))) = (times_ms.get(i - 1), times_ms.get(i)) else {
            continue;
        };
        let pause = b - a;
        if pause * 2 > interval_ms * 3 {
            let missing = ((pause as f64 / interval_ms as f64).round() as usize).saturating_sub(1);
            if (1..=max).contains(&missing) {
                out.push((i, missing));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_flash_but_not_a_change_of_light() {
        // A slow sunset with a flash at frame 10 and headlights over frames 20-21.
        let mut v: Vec<Option<f64>> = (0..40).map(|i| Some(-0.05 * f64::from(i))).collect();
        v[10] = v[10].map(|x| x + 1.2);
        v[20] = v[20].map(|x| x + 0.8);
        v[21] = v[21].map(|x| x + 0.8);
        // A real change: from frame 30 on, a stop darker for good.
        for x in v.iter_mut().skip(30) {
            *x = x.map(|x| x - 1.0);
        }
        v[35] = None;
        assert_eq!(outliers(&v, 0.4), vec![10, 20, 21]);
        // Three in a row: a change of light, not an odd frame.
        let mut w: Vec<Option<f64>> = vec![Some(0.0); 20];
        for x in w.iter_mut().skip(8).take(3) {
            *x = Some(1.0);
        }
        assert!(outliers(&w, 0.4).is_empty());
    }

    #[test]
    fn the_light_turning_at_the_end_of_a_series_is_no_odd_frame() {
        // Karer See cut after frame 11: the last frame half a stop darker, as the frame after it
        // (left out) was too. A flash at the end (1.2 stops) still counts.
        let v: Vec<Option<f64>> = [
            -3.46, -3.41, -3.38, -3.38, -3.45, -3.51, -3.37, -3.2, -3.21, -3.17, -3.73,
        ]
        .into_iter()
        .map(Some)
        .collect();
        assert!(outliers(&v, 0.4).is_empty(), "{:?}", outliers(&v, 0.4));
        let mut f = v.clone();
        f[10] = Some(-3.17 + 1.2);
        assert_eq!(outliers(&f, 0.4), vec![10]);
        let mut s = v;
        s[0] = Some(-3.46 + 1.2);
        s[10] = Some(-3.17);
        assert_eq!(outliers(&s, 0.4), vec![0]);
    }

    #[test]
    fn flicker_is_not_an_odd_frame() {
        // Strong flicker (±0.15 stops at random) over a sunset: no frame stands out at 0.4.
        let mut seed = 7u64;
        let v: Vec<Option<f64>> = (0..200)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let r = (seed >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
                Some(-0.04 * f64::from(i) + 0.15 * r)
            })
            .collect();
        assert!(outliers(&v, 0.4).is_empty(), "{:?}", outliers(&v, 0.4));
    }

    #[test]
    fn counts_burnt_out_highlights() {
        let mut data = vec![0.5f32; 100 * 100 * 3];
        for px in data.chunks_mut(3).take(2500) {
            px[1] = 1.0;
        }
        let view = ImageView {
            width: 100,
            height: 100,
            channels: 3,
            data: &data,
        };
        let f = clipped_fraction(view, 0.995).unwrap();
        assert!((f - 0.25).abs() < 0.01, "{f}");
        let empty = ImageView {
            width: 0,
            height: 0,
            channels: 3,
            data: &[],
        };
        assert_eq!(clipped_fraction(empty, 0.995), None);
    }

    #[test]
    fn fills_pauses_but_not_cuts() {
        let t = |ms: &[i64]| ms.iter().map(|m| Some(*m)).collect::<Vec<_>>();
        // 5 s interval; a 15 s pause before frame 3 (two frames missing), an hour before frame 5.
        let times = t(&[0, 5000, 10_000, 25_000, 30_000, 3_630_000]);
        assert_eq!(missing_frames(&times, 5000, 10), vec![(3, 2)]);
        assert!(missing_frames(&times, 0, 10).is_empty());
        assert!(missing_frames(&[Some(0), None, Some(20_000)], 5000, 10).is_empty());
    }
}
