//! Keyframes: develop settings set on a few frames, interpolated over every frame.
//!
//! Settings are numeric controls addressed by id (for LightCraft: its develop control ids, such as
//! `exposure` or `temperature`). A control only has to appear on the keyframes where it matters;
//! it is interpolated across the keyframes that set it and held constant before the first and after
//! the last of them. Non-numeric settings (profiles, masks, crops) are the adapter's job: it takes
//! them from the nearest keyframe at or before the frame.

use std::collections::BTreeMap;
use std::fmt;

/// Settings on one frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Keyframe {
    /// Frame index in the sequence.
    pub frame: usize,
    /// Control id → value.
    pub values: BTreeMap<String, f64>,
}

/// How values move between keyframes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interpolation {
    /// Straight lines: speed changes abruptly at each keyframe.
    Linear,
    /// Monotone cubic (Fritsch–Carlson): smooth, never overshoots past the keyframe values, so a
    /// slider never swings beyond what you set.
    #[default]
    Smooth,
}

/// Why keyframes could not be interpolated.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyframeError {
    /// Two keyframes on the same frame.
    Duplicate { frame: usize },
    /// A keyframe beyond the end of the sequence.
    OutOfRange { frame: usize, frame_count: usize },
    /// A value that is NaN or infinite.
    NotFinite { frame: usize, control: String },
}

impl fmt::Display for KeyframeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate { frame } => write!(f, "frame {frame} has more than one keyframe"),
            Self::OutOfRange { frame, frame_count } => {
                write!(
                    f,
                    "keyframe on frame {frame}, but the sequence has {frame_count} frames"
                )
            }
            Self::NotFinite { frame, control } => {
                write!(f, "keyframe on frame {frame}: {control} is not a finite number")
            }
        }
    }
}

impl std::error::Error for KeyframeError {}

/// Values of every control on every frame: control id → one value per frame.
pub type Curves = BTreeMap<String, Vec<f64>>;

/// Interpolates `keyframes` over `frame_count` frames.
///
/// # Errors
/// On duplicate frames, frames out of range and non-finite values.
pub fn interpolate(
    keyframes: &[Keyframe],
    frame_count: usize,
    mode: Interpolation,
) -> Result<Curves, KeyframeError> {
    let mut sorted: Vec<&Keyframe> = keyframes.iter().collect();
    sorted.sort_by_key(|k| k.frame);
    for pair in sorted.windows(2) {
        if let [a, b] = pair
            && a.frame == b.frame
        {
            return Err(KeyframeError::Duplicate { frame: a.frame });
        }
    }
    let mut points: BTreeMap<&str, Vec<(f64, f64)>> = BTreeMap::new();
    for k in &sorted {
        if k.frame >= frame_count {
            return Err(KeyframeError::OutOfRange {
                frame: k.frame,
                frame_count,
            });
        }
        for (control, &value) in &k.values {
            if !value.is_finite() {
                return Err(KeyframeError::NotFinite {
                    frame: k.frame,
                    control: control.clone(),
                });
            }
            points.entry(control).or_default().push((k.frame as f64, value));
        }
    }
    Ok(points
        .into_iter()
        .map(|(control, pts)| (control.to_owned(), sample(&pts, frame_count, mode)))
        .collect())
}

/// Samples the curve through `pts` (sorted by x, distinct x) at 0, 1, …, `count − 1`.
fn sample(pts: &[(f64, f64)], count: usize, mode: Interpolation) -> Vec<f64> {
    let slopes = match mode {
        Interpolation::Linear => None,
        Interpolation::Smooth => Some(monotone_slopes(pts)),
    };
    let mut out = Vec::with_capacity(count);
    let mut seg = 0;
    for i in 0..count {
        let x = i as f64;
        while seg + 2 < pts.len() && pts.get(seg + 1).is_some_and(|p| x > p.0) {
            seg += 1;
        }
        out.push(eval(pts, slopes.as_deref(), seg, x));
    }
    out
}

fn eval(pts: &[(f64, f64)], slopes: Option<&[f64]>, seg: usize, x: f64) -> f64 {
    let (Some(&(x0, y0)), Some(&(x1, y1))) = (pts.get(seg), pts.get(seg + 1)) else {
        // One point: constant.
        return pts.first().map_or(0.0, |p| p.1);
    };
    if x <= x0 {
        return y0;
    }
    if x >= x1 {
        return y1;
    }
    let h = x1 - x0;
    let t = (x - x0) / h;
    match slopes {
        None => y0 + t * (y1 - y0),
        Some(m) => {
            let m0 = m.get(seg).copied().unwrap_or(0.0);
            let m1 = m.get(seg + 1).copied().unwrap_or(0.0);
            let t2 = t * t;
            let t3 = t2 * t;
            let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
            let h10 = t3 - 2.0 * t2 + t;
            let h01 = -2.0 * t3 + 3.0 * t2;
            let h11 = t3 - t2;
            h00 * y0 + h10 * h * m0 + h01 * y1 + h11 * h * m1
        }
    }
}

/// Fritsch–Carlson tangents: a cubic Hermite spline with these is monotone on every segment where the
/// data is.
fn monotone_slopes(pts: &[(f64, f64)]) -> Vec<f64> {
    let n = pts.len();
    if n < 2 {
        return vec![0.0; n];
    }
    let secants: Vec<f64> = pts
        .windows(2)
        .map(|w| match w {
            [(x0, y0), (x1, y1)] => (y1 - y0) / (x1 - x0),
            _ => 0.0,
        })
        .collect();
    let mut m = Vec::with_capacity(n);
    m.push(secants.first().copied().unwrap_or(0.0));
    for w in secants.windows(2) {
        let [a, b] = w else { continue };
        // Turning point or flat segment: flat tangent. Otherwise the harmonic mean, which stays
        // inside the Fritsch–Carlson monotonicity region.
        m.push(if a * b <= 0.0 { 0.0 } else { 2.0 * a * b / (a + b) });
    }
    m.push(secants.last().copied().unwrap_or(0.0));
    // Shrink tangents where they would overshoot (|α|²+|β|² > 9).
    for (k, &d) in secants.iter().enumerate() {
        if d == 0.0 {
            if let Some(v) = m.get_mut(k) {
                *v = 0.0;
            }
            if let Some(v) = m.get_mut(k + 1) {
                *v = 0.0;
            }
            continue;
        }
        let (Some(&mk), Some(&mk1)) = (m.get(k), m.get(k + 1)) else {
            continue;
        };
        let a = mk / d;
        let b = mk1 / d;
        let s = a * a + b * b;
        if s > 9.0 {
            let tau = 3.0 / s.sqrt();
            if let Some(v) = m.get_mut(k) {
                *v = tau * a * d;
            }
            if let Some(v) = m.get_mut(k + 1) {
                *v = tau * b * d;
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kf(frame: usize, values: &[(&str, f64)]) -> Keyframe {
        Keyframe {
            frame,
            values: values.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect(),
        }
    }

    #[test]
    fn hits_keyframes_and_holds_outside() {
        let keys = [
            kf(10, &[("exposure", 0.0)]),
            kf(20, &[("exposure", 2.0)]),
            kf(40, &[("exposure", 1.0)]),
        ];
        for mode in [Interpolation::Linear, Interpolation::Smooth] {
            let c = interpolate(&keys, 50, mode).unwrap();
            let e = &c["exposure"];
            assert_eq!(e.len(), 50);
            assert_eq!(e[0], 0.0);
            assert_eq!(e[10], 0.0);
            assert_eq!(e[20], 2.0);
            assert_eq!(e[40], 1.0);
            assert_eq!(e[49], 1.0);
        }
    }

    #[test]
    fn linear_midpoint() {
        let keys = [kf(0, &[("t", 0.0)]), kf(10, &[("t", 10.0)])];
        let c = interpolate(&keys, 11, Interpolation::Linear).unwrap();
        assert!((c["t"][5] - 5.0).abs() < 1e-12);
    }

    #[test]
    fn smooth_never_overshoots() {
        // A peak then a plateau: a natural spline would overshoot both.
        let keys = [
            kf(0, &[("v", 0.0)]),
            kf(10, &[("v", 1.0)]),
            kf(12, &[("v", 1.0)]),
            kf(30, &[("v", -1.0)]),
            kf(31, &[("v", 3.0)]),
        ];
        let c = interpolate(&keys, 40, Interpolation::Smooth).unwrap();
        let v = &c["v"];
        for i in 0..10 {
            assert!(v[i] <= v[i + 1] + 1e-12, "not monotone at {i}: {v:?}");
        }
        for x in &v[10..=12] {
            assert!((x - 1.0).abs() < 1e-12, "plateau broke: {v:?}");
        }
        assert!(v.iter().all(|x| (-1.0 - 1e-12..=3.0 + 1e-12).contains(x)));
    }

    #[test]
    fn controls_only_on_some_keyframes() {
        let keys = [kf(0, &[("a", 1.0)]), kf(5, &[("b", 2.0)]), kf(9, &[("a", 3.0)])];
        let c = interpolate(&keys, 10, Interpolation::Linear).unwrap();
        assert_eq!(c["b"], vec![2.0; 10]);
        assert!((c["a"][9] - 3.0).abs() < 1e-12);
        assert!((c["a"][0] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn unsorted_input_is_fine() {
        let keys = [kf(9, &[("a", 9.0)]), kf(0, &[("a", 0.0)])];
        let c = interpolate(&keys, 10, Interpolation::Linear).unwrap();
        assert!((c["a"][3] - 3.0).abs() < 1e-12);
    }

    #[test]
    fn errors() {
        assert_eq!(
            interpolate(&[kf(1, &[]), kf(1, &[])], 5, Interpolation::Smooth),
            Err(KeyframeError::Duplicate { frame: 1 })
        );
        assert_eq!(
            interpolate(&[kf(5, &[])], 5, Interpolation::Smooth),
            Err(KeyframeError::OutOfRange {
                frame: 5,
                frame_count: 5
            })
        );
        assert!(matches!(
            interpolate(&[kf(0, &[("x", f64::INFINITY)])], 5, Interpolation::Smooth),
            Err(KeyframeError::NotFinite { .. })
        ));
        assert!(interpolate(&[], 0, Interpolation::Smooth).unwrap().is_empty());
    }
}
