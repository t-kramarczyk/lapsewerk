//! Day-to-night (and night-to-day) sequences: how bright the video should get, and which frames to
//! edit as keyframes.
//!
//! A camera that follows the light (auto exposure, or a photographer ramping by hand) records the
//! night about as bright as the day; undoing its steps exactly would leave the night many stops
//! darker, near black. Neither is what a viewer expects: the night should look like night and
//! still show the scene. So the video keeps a chosen fraction of the scene's real change of light
//! ([`brightness_offsets`]): the scene's brightness at constant exposure is the measured brightness
//! plus the camera's exposure changes (EXIF), smoothed so that rounded or phantom steps do not count.
//!
//! Keyframes go where the light changes ([`suggest_keyframes`]): spread evenly over the change in
//! brightness, not in time, so a sunset gets more of them than the hour of night after it.

use crate::smooth;

/// Smoothing of the scene's brightness, in frames (Gaussian sigma): the change of light is slow.
pub const SCENE_SMOOTHING: f64 = 15.0;

/// The scene's brightness at constant exposure, smoothed: `measured` (developed brightness as shot)
/// plus `exposure_compensation` (stops undoing the camera's exposure changes, from EXIF). Frames
/// without a measurement are filled from the smooth curve.
#[must_use]
pub fn scene_brightness(measured: &[Option<f64>], exposure_compensation: &[f64]) -> Vec<f64> {
    let raw: Vec<f64> = measured
        .iter()
        .enumerate()
        .map(|(i, m)| match m {
            Some(m) => m + exposure_compensation.get(i).copied().unwrap_or(0.0),
            None => f64::NAN,
        })
        .collect();
    smooth::robust_local_linear(&raw, SCENE_SMOOTHING, 2)
}

/// The most the day-to-night brightness moves a frame (stops).
const MOST: f64 = 20.0;

/// Exposure offsets (stops, per frame) that make the video's brightness follow `keep` (0…1) of the
/// scene's change of light, unchanged at the brightest point of the scene (the day, as edited).
///
/// `video` is the brightness the video has without them (smooth trend of the frames after ramp
/// compensation), `scene` the scene's ([`scene_brightness`]). Returns the offsets and the anchor
/// frame. Offsets are limited to ±20 stops, beyond any real holy grail (a sunset to the Milky
/// Way is some 15 to 18 stops of scene light, most of it undone again when brightening).
#[must_use]
pub fn brightness_offsets(video: &[f64], scene: &[f64], keep: f64) -> (Vec<f64>, usize) {
    let keep = if keep.is_finite() {
        keep.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let anchor = scene
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_finite())
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map_or(0, |(i, _)| i);
    let (Some(&va), Some(&sa)) = (video.get(anchor), scene.get(anchor)) else {
        return (vec![0.0; video.len()], anchor);
    };
    let offsets = video
        .iter()
        .zip(scene)
        .map(|(v, s)| {
            let target = va + keep * (s - sa);
            let o = target - v;
            if o.is_finite() { o.clamp(-MOST, MOST) } else { 0.0 }
        })
        .collect();
    (offsets, anchor)
}

/// Up to `count` keyframes (at least the first and last frame), spread evenly over the cumulative
/// change of `scene` brightness. A scene whose light hardly changes (under ⅓ stop) gets the first
/// and last frame only.
#[must_use]
pub fn suggest_keyframes(scene: &[f64], count: usize) -> Vec<usize> {
    let n = scene.len();
    if n == 0 {
        return Vec::new();
    }
    let last = n - 1;
    let mut cumulative = Vec::with_capacity(n);
    let mut total = 0.0;
    cumulative.push(0.0);
    for w in scene.windows(2) {
        if let [a, b] = w {
            let d = (b - a).abs();
            if d.is_finite() {
                total += d;
            }
        }
        cumulative.push(total);
    }
    if total < 1.0 / 3.0 || count <= 2 {
        return if last == 0 { vec![0] } else { vec![0, last] };
    }
    let mut out = vec![0];
    for k in 1..count - 1 {
        let goal = total * k as f64 / (count - 1) as f64;
        let i = cumulative.iter().position(|c| *c >= goal).unwrap_or(last);
        if out.last().is_none_or(|&p| p != i) && i != last {
            out.push(i);
        }
    }
    out.push(last);
    out
}

/// A brightness shape set by hand (the "holy grail" curve drawn freely): `points` (frame, stops)
/// as an offset per frame for `n` frames, smooth between points (no kinks), held before the first
/// and after the last. A point past the last frame still shapes the curve towards it; points not
/// finite are left out, and of two at one frame the first counts.
#[must_use]
pub fn shaped(points: &[(usize, f64)], n: usize) -> Vec<f64> {
    let mut p: Vec<(usize, f64)> = points.iter().copied().filter(|&(_, s)| s.is_finite()).collect();
    p.sort_by_key(|&(f, _)| f);
    p.dedup_by_key(|&mut (f, _)| f);
    (0..n)
        .map(|i| {
            let after = p.iter().position(|&(f, _)| f >= i);
            match after {
                None => p.last().map_or(0.0, |q| q.1),
                Some(0) => p.first().map_or(0.0, |q| q.1),
                Some(k) => {
                    let (Some(&(f0, a)), Some(&(f1, b))) = (p.get(k - 1), p.get(k)) else {
                        return 0.0;
                    };
                    let t = (i - f0) as f64 / (f1 - f0).max(1) as f64;
                    let t = t * t * (3.0 - 2.0 * t);
                    a + (b - a) * t
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deep_holy_grail_keeps_its_share_of_the_night() {
        // Sunset to the Milky Way: 18 stops of scene light, the camera's steps undone.
        let scene: Vec<f64> = (0..100).map(|i| -0.18 * f64::from(i)).collect();
        let (o, _) = brightness_offsets(&scene, &scene, 0.3);
        // The night keeps 30 %: 5.4 stops down, so it is brightened by 12.6 (once capped at 8).
        assert!((o[99] - 0.7 * 17.82).abs() < 1e-9, "{}", o[99]);
    }

    #[test]
    fn a_hand_drawn_shape_is_smooth_and_held_at_the_ends() {
        let v = shaped(&[(10, 0.0), (20, 1.0), (99, 5.0)], 31);
        assert_eq!(v.len(), 31);
        assert_eq!(v[0], 0.0);
        assert_eq!(v[10], 0.0);
        assert!((v[15] - 0.5).abs() < 1e-12);
        assert!(v[12] < 0.2 * 1.0, "eases in: {}", v[12]);
        assert_eq!(v[20], 1.0);
        // Towards the point at frame 99 (past the end): rising, not yet there.
        assert!(v[30] > 1.0 && v[30] < 5.0, "{}", v[30]);
        assert_eq!(shaped(&[], 3), vec![0.0; 3]);
        assert_eq!(shaped(&[(1, f64::NAN), (0, 0.5)], 3), vec![0.5; 3]);
        // Past the last frame: held from it (one point), or the curve heading there.
        assert_eq!(shaped(&[(70, 1.0)], 3), vec![1.0; 3]);
        let towards = shaped(&[(0, 0.0), (20, 2.0)], 11);
        assert!((towards[10] - 1.0).abs() < 1e-12, "{}", towards[10]);
    }

    #[test]
    fn auto_exposure_hides_a_sunrise_that_the_offsets_bring_back() {
        // Scene: 10 stops darker at night (frames 0-49), linear rise to day (50-149), day (150-199).
        let n = 200;
        let scene_true: Vec<f64> = (0..n)
            .map(|i| match i {
                0..50 => -10.0,
                50..150 => -10.0 + 10.0 * (i - 50) as f64 / 100.0,
                _ => 0.0,
            })
            .collect();
        // The camera kept the picture at -2.5 by exposing more at night; undoing its exposure
        // changes (the compensation) gives the scene's own change.
        let measured: Vec<Option<f64>> = (0..n).map(|_| Some(-2.5)).collect();
        let comp = scene_true.clone();
        let scene = scene_brightness(&measured, &comp);
        assert!(
            (scene[0] - (-12.5)).abs() < 0.2 && (scene[199] - (-2.5)).abs() < 0.2,
            "{} {}",
            scene[0],
            scene[199]
        );
        let video = vec![-2.5; n];
        let (off, anchor) = brightness_offsets(&video, &scene, 0.3);
        assert!(anchor >= 150, "the day is the anchor: {anchor}");
        assert!(off[199].abs() < 0.05);
        assert!(
            (off[0] - (-3.0)).abs() < 0.2,
            "night 30 % of 10 stops darker: {}",
            off[0]
        );
        let (none, _) = brightness_offsets(&video, &scene, 0.0);
        assert!(none.iter().all(|o| o.abs() < 1e-9));
        // Keyframes on the rise, none in the flat night or day beyond the ends.
        let k = suggest_keyframes(&scene, 5);
        assert_eq!(k.first(), Some(&0));
        assert_eq!(k.last(), Some(&199));
        assert!(
            k[1..k.len() - 1].iter().all(|&i| (40..=160).contains(&i)),
            "{k:?}"
        );
    }

    #[test]
    fn steady_light_gets_the_ends_only() {
        assert_eq!(suggest_keyframes(&[1.0; 50], 6), vec![0, 49]);
        assert_eq!(suggest_keyframes(&[1.0], 6), vec![0]);
        assert!(suggest_keyframes(&[], 6).is_empty());
    }
}
