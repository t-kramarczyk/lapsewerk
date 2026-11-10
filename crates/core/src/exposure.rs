//! Exposure values from camera settings, and ramp compensation.
//!
//! During a day-to-night ("holy grail") sequence the photographer or the camera changes shutter,
//! aperture or ISO in steps. Each step makes the raw frames jump in brightness. [`ramp_compensation`]
//! computes, per frame, the exposure offset in stops that undoes those steps relative to a reference
//! frame, so the developed sequence only shows the real change of light, which keyframes and deflicker
//! then shape.

use std::fmt;

/// Camera settings of one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Exposure {
    /// Shutter time in seconds (EXIF `ExposureTime`).
    pub shutter_s: f64,
    /// F-number (EXIF `FNumber`).
    pub aperture: f64,
    /// ISO sensitivity (EXIF `ISOSpeedRatings` / `RecommendedExposureIndex`).
    pub iso: f64,
}

/// Why an [`Exposure`] could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExposureError {
    Shutter,
    Aperture,
    Iso,
}

impl fmt::Display for ExposureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self {
            Self::Shutter => "shutter time",
            Self::Aperture => "aperture",
            Self::Iso => "ISO",
        };
        write!(f, "{what} must be a finite number greater than zero")
    }
}

impl std::error::Error for ExposureError {}

/// How camera-reported settings are snapped before comparing frames.
///
/// Cameras report nominal values (1/60 s, f/5.6, ISO 640) that are not exact powers of two
/// (1/64 s, f/5.66, ISO 635). Comparing nominal values would turn every "one third stop" step into
/// 0.29 or 0.36 stops and leave a visible residue. Snapping each setting to the camera's step grid
/// recovers the true step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StepGrid {
    /// Use the reported values as they are.
    Exact,
    /// Third-stop steps (the default on almost every camera).
    #[default]
    Thirds,
    /// Half-stop steps.
    Halves,
}

impl StepGrid {
    fn snap(self, stops: f64) -> f64 {
        let steps = match self {
            Self::Exact => return stops,
            Self::Thirds => 3.0,
            Self::Halves => 2.0,
        };
        (stops * steps).round() / steps
    }
}

impl Exposure {
    /// Validated constructor.
    ///
    /// # Errors
    /// When a value is not finite or not greater than zero.
    pub fn new(shutter_s: f64, aperture: f64, iso: f64) -> Result<Self, ExposureError> {
        let valid = |v: f64| v.is_finite() && v > 0.0;
        if !valid(shutter_s) {
            return Err(ExposureError::Shutter);
        }
        if !valid(aperture) {
            return Err(ExposureError::Aperture);
        }
        if !valid(iso) {
            return Err(ExposureError::Iso);
        }
        Ok(Self {
            shutter_s,
            aperture,
            iso,
        })
    }

    /// Light recorded relative to 1 s, f/1, ISO 100, in stops: larger is brighter.
    ///
    /// Each setting is snapped to `grid` on its own, as cameras step each one separately.
    #[must_use]
    pub fn stops(&self, grid: StepGrid) -> f64 {
        let shutter = grid.snap(self.shutter_s.log2());
        let aperture = grid.snap(2.0 * self.aperture.log2());
        let iso = grid.snap((self.iso / 100.0).log2());
        shutter - aperture + iso
    }
}

/// Per-frame compensation in stops that cancels the camera's exposure changes relative to `reference`.
///
/// Frames without settings (`None`, e.g. a JPEG without EXIF) take the settings of the nearest
/// earlier frame that has them, or the nearest later one at the start. Returns `None` when no frame
/// has settings or `reference` is out of range. A frame recorded one stop brighter than the
/// reference gets −1.0.
#[must_use]
pub fn ramp_compensation(
    exposures: &[Option<Exposure>],
    reference: usize,
    grid: StepGrid,
) -> Option<Vec<f64>> {
    if reference >= exposures.len() {
        return None;
    }
    let first_known = exposures.iter().flatten().next().copied()?;
    let mut last = first_known;
    let filled: Vec<f64> = exposures
        .iter()
        .map(|e| {
            if let Some(e) = e {
                last = *e;
            }
            last.stops(grid)
        })
        .collect();
    let reference_stops = *filled.get(reference)?;
    Some(filled.iter().map(|s| reference_stops - s).collect())
}

/// A frame where the recorded exposure changes from the frame before.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExposureStep {
    /// Index of the first frame with the new settings.
    pub frame: usize,
    /// Change in stops (positive: the camera recorded more light from this frame on).
    pub stops: f64,
}

/// Exposure changes between consecutive frames larger than `threshold` stops.
///
/// Input is the output of [`ramp_compensation`]; the step sign follows the recorded light.
#[must_use]
pub fn exposure_steps(compensation: &[f64], threshold: f64) -> Vec<ExposureStep> {
    compensation
        .windows(2)
        .enumerate()
        .filter_map(|(i, w)| {
            let [before, after] = w else { return None };
            let stops = before - after;
            (stops.abs() > threshold).then_some(ExposureStep { frame: i + 1, stops })
        })
        .collect()
}

/// How a camera exposure step was checked against the frames' measured brightness.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepCheck {
    /// First frame with the new settings.
    pub frame: usize,
    /// The step the EXIF data reports, in stops (positive: more light recorded).
    pub reported: f64,
    /// The jump measured across the step, when both sides had enough frames.
    pub measured: Option<f64>,
    /// The step compensated: `reported` when the measurement confirms it (or there is none),
    /// else `measured`.
    pub used: f64,
}

/// Jump of `values` between frame `at − 1` and `at`: straight lines fitted to the measured frames in
/// `left` and `right` (index ranges ending before and starting at `at`), both evaluated at `at − 0.5`.
/// `None` when a side has fewer than two measured frames.
#[must_use]
pub fn measured_jump(
    values: &[Option<f64>],
    left: std::ops::Range<usize>,
    right: std::ops::Range<usize>,
    at: usize,
) -> Option<f64> {
    let x = at as f64 - 0.5;
    Some(line_at(values, right, x)? - line_at(values, left, x)?)
}

fn line_at(values: &[Option<f64>], range: std::ops::Range<usize>, x: f64) -> Option<f64> {
    let pts: Vec<(f64, f64)> = range
        .filter_map(|i| {
            values
                .get(i)
                .copied()
                .flatten()
                .filter(|v| v.is_finite())
                .map(|v| (i as f64, v))
        })
        .collect();
    if pts.len() < 2 {
        return None;
    }
    let n = pts.len() as f64;
    let mx = pts.iter().map(|p| p.0).sum::<f64>() / n;
    let my = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    Some(my + slope * (x - mx))
}

/// Checks each reported step against `measured` (linear sensor brightness in stops, e.g. the
/// log-mean of the raw samples, which no look or tone curve distorts) and returns the compensation
/// built from the confirmed steps.
///
/// Some cameras' interval modes follow the light continuously and record rounded nominal settings,
/// so the EXIF shows steps the sensor data does not: compensating those would add jumps. A step is
/// confirmed when the measured jump is within `tolerance` stops of the reported one; otherwise the
/// measured jump is used. A step that cannot be measured (too close to its neighbours, frames
/// missing) is taken as reported, unless most measured steps turned out not to exist: the camera
/// then follows the light continuously and its unmeasured steps are treated as not happening either.
/// `window` frames on each side (never across a neighbouring step) are fitted.
#[must_use]
pub fn verified_compensation(
    frame_count: usize,
    steps: &[ExposureStep],
    measured: &[Option<f64>],
    window: usize,
    tolerance: f64,
) -> (Vec<f64>, Vec<StepCheck>) {
    let window = window.max(2);
    let mut checks = Vec::with_capacity(steps.len());
    for (k, step) in steps.iter().enumerate() {
        let prev = k.checked_sub(1).and_then(|j| steps.get(j)).map_or(0, |s| s.frame);
        let next = steps.get(k + 1).map_or(frame_count, |s| s.frame);
        let left = prev.max(step.frame.saturating_sub(window))..step.frame;
        let right = step.frame..next.min(step.frame + window);
        let jump = measured_jump(measured, left, right, step.frame);
        let used = match jump {
            Some(m) if (m - step.stops).abs() > tolerance => m,
            _ => step.stops,
        };
        checks.push(StepCheck {
            frame: step.frame,
            reported: step.stops,
            measured: jump,
            used,
        });
    }
    let measured_count = checks.iter().filter(|c| c.measured.is_some()).count();
    let phantom = checks
        .iter()
        .filter(|c| c.measured.is_some() && c.used != c.reported)
        .count();
    if phantom * 2 > measured_count {
        for c in checks.iter_mut().filter(|c| c.measured.is_none()) {
            c.used = 0.0;
        }
    }
    let mut comp = vec![0.0; frame_count];
    let mut total = 0.0;
    let mut next = checks.iter().peekable();
    for (i, c) in comp.iter_mut().enumerate() {
        while let Some(check) = next.next_if(|c| c.frame <= i) {
            total -= check.used;
        }
        *c = total;
    }
    (comp, checks)
}

/// Steps the camera made that show only in the pictures (no EXIF data, e.g. exported JPEGs): a
/// change of at least `min` stops in `stops` (each frame's measured brightness) from one frame to
/// the next, where the changes between the [`SIDE`] frames on either side stay small (the light's
/// own trend, however steep, changes little from frame to frame). A flash (up, then down again)
/// is no step. The size is the change less the trend around it; positive: brighter from there on.
///
/// With `tiles` (each frame's brightness in the tiles of a grid, stops), a change must move most
/// tiles alike: the camera changes the whole picture by the same factor, whereas street lights
/// coming on, the moon out of a cloud or a floodlight going off change parts of it.
#[must_use]
pub fn visible_steps(stops: &[Option<f64>], tiles: &[Vec<Option<f64>>], min: f64) -> Vec<ExposureStep> {
    if !min.is_finite() || min <= 0.0 {
        return Vec::new();
    }
    // Frame-to-frame changes: d[i] is from frame i - 1 to i.
    let d: Vec<Option<f64>> = (0..stops.len())
        .map(
            |i| match (i.checked_sub(1).and_then(|p| stops.get(p)), stops.get(i)) {
                (Some(Some(a)), Some(Some(b))) if a.is_finite() && b.is_finite() => Some(b - a),
                _ => None,
            },
        )
        .collect();
    (SIDE + 1..d.len().saturating_sub(SIDE))
        .filter_map(|i| {
            let jump = (*d.get(i)?)?;
            let mut around: Vec<f64> = (i - SIDE..=i + SIDE)
                .filter(|&k| k != i)
                .map(|k| d.get(k).copied().flatten())
                .collect::<Option<Vec<f64>>>()?;
            around.sort_by(f64::total_cmp);
            let trend = *around.get(around.len() / 2)?;
            // Calm enough: a third-stop step of the camera nearby (which the deflicker takes
            // out) still is; a flash or another large step is not.
            let calm = around.iter().all(|x| (x - trend).abs() < min * 0.7);
            let step = jump - trend;
            // And far out of the noise: five times the spread of the changes around, each less
            // the median change near it (robust: the median deviation), so the strong flicker of a
            // noisy dark end does not pass for a step, and a fast fall of light does not hide one.
            let rest = |k: usize| -> Option<f64> {
                let at = (*d.get(k)?)?;
                let mut near: Vec<f64> = (k.saturating_sub(5)..=k + 5)
                    .filter(|&j| j != k)
                    .filter_map(|j| d.get(j).copied().flatten())
                    .collect();
                near.sort_by(f64::total_cmp);
                Some(at - near.get(near.len() / 2).copied().unwrap_or(0.0))
            };
            let mut dev: Vec<f64> = (i.saturating_sub(NOISE)..=i + NOISE)
                .filter(|&k| k != i)
                .filter_map(|k| rest(k).map(f64::abs))
                .collect();
            dev.sort_by(f64::total_cmp);
            let spread = dev.get(dev.len() / 2).copied().unwrap_or(0.0) * 1.4826;
            // Or clearly out of it and against the light (the camera of a ramp steps opposite
            // to the light's fall or rise, which goes on around it): third-stop steps every few
            // frames widen the spread, and a whole stop between them is still the camera's.
            let mut wide: Vec<f64> = (i.saturating_sub(NOISE)..=i + NOISE)
                .filter(|&k| k != i)
                .filter_map(|k| d.get(k).copied().flatten())
                .collect();
            wide.sort_by(f64::total_cmp);
            let light = wide.get(wide.len() / 2).copied().unwrap_or(0.0);
            let against = step * light < 0.0 && light.abs() >= 0.05 && step.abs() >= 3.5 * spread;
            let clear = step.abs() >= 5.0 * spread || against;
            (calm && clear && step.abs() >= min && alike(tiles, i, step, min)).then_some(ExposureStep {
                frame: i,
                stops: step,
            })
        })
        .collect()
}

/// Whether at least three quarters of the tiles change by about `step` from frame `i - 1` to `i`,
/// each less its own trend (the median change over the [`SIDE`] frames either side: a sky darkens
/// faster than the land at dusk); within a third of `min` and a tenth of a stop; tiles without a
/// measurement left out. No tiles: yes.
fn alike(tiles: &[Vec<Option<f64>>], i: usize, step: f64, min: f64) -> bool {
    let count = tiles.get(i).map_or(0, Vec::len);
    let change = |k: usize, t: usize| -> Option<f64> {
        let a = (*tiles.get(k)?.get(t)?)?;
        let b = (*tiles.get(k.checked_sub(1)?)?.get(t)?)?;
        Some(a - b).filter(|c| c.is_finite())
    };
    let changes: Vec<f64> = (0..count)
        .filter_map(|t| {
            let at = change(i, t)?;
            let mut around: Vec<f64> = (i.saturating_sub(SIDE)..=i + SIDE)
                .filter(|&k| k != i)
                .filter_map(|k| change(k, t))
                .collect();
            around.sort_by(f64::total_cmp);
            Some(at - around.get(around.len() / 2).copied().unwrap_or(0.0))
        })
        .collect();
    if changes.is_empty() {
        return true;
    }
    let close = changes
        .iter()
        .filter(|c| (*c - step).abs() < min / 3.0 + 0.1)
        .count();
    close * 4 >= changes.len() * 3
}

/// Frames on each side of a step seen in the pictures whose changes must stay small.
const SIDE: usize = 3;

/// Frames on each side of a step seen in the pictures whose changes tell the noise.
const NOISE: usize = 20;

/// The compensation (stops per frame, as [`ramp_compensation`]) that undoes `steps`.
#[must_use]
pub fn compensation_of(n: usize, steps: &[ExposureStep]) -> Vec<f64> {
    let mut c = vec![0.0; n];
    for s in steps {
        for v in c.iter_mut().skip(s.frame) {
            *v -= s.stops;
        }
    }
    c
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_whole_stop_between_third_stop_steps_of_a_ramp_is_seen() {
        // A holy-grail ramp without EXIF as measured (frames 101-162): the light falls, the camera
        // opens a third of a stop every few frames (they widen the spread) and a whole stop at
        // the 31st value, against the light: found.
        let v = [
            -3.144, -3.301, -3.437, -3.650, -3.482, -3.672, -3.493, -3.652, -3.545, -3.676, -3.517, -3.703,
            -3.483, -3.638, -3.532, -3.648, -3.516, -3.675, -3.485, -3.636, -3.805, -3.603, -3.774, -3.519,
            -3.651, -3.497, -3.685, -3.434, -3.586, -3.654, -2.823, -2.932, -3.017, -3.156, -3.283, -3.306,
            -3.538, -3.604, -3.366, -3.446, -3.513, -3.326, -3.414, -3.435, -3.581, -3.642, -3.810, -3.790,
            -3.862, -3.984, -4.010, -4.029, -4.130, -4.168, -4.276, -4.330, -4.330, -4.370, -4.467, -4.490,
            -4.561, -4.653,
        ];
        let stops: Vec<Option<f64>> = v.iter().map(|x| Some(*x)).collect();
        let found = visible_steps(&stops, &[], 0.5);
        assert_eq!(
            found.iter().map(|s| s.frame).collect::<Vec<_>>(),
            [30],
            "{found:?}"
        );
    }

    #[test]
    fn noise_in_a_dark_end_is_no_step() {
        // Measured at the noisy dark end of a long series (no EXIF): -5.81 to -6.31 at the 22nd
        // value is no step, the frames after come back.
        let v = [
            -6.011, -6.226, -6.153, -5.864, -6.226, -6.153, -6.226, -6.174, -6.292, -6.174, -5.932, -5.929,
            -5.864, -6.226, -5.932, -6.226, -6.153, -6.011, -5.864, -5.932, -5.808, -6.308, -6.153, -6.107,
            -6.308, -6.308, -6.153, -6.107, -6.292, -6.308, -6.226, -5.932, -6.107, -5.864, -6.107, -5.864,
            -6.107, -6.153, -6.153, -6.395, -6.395, -6.107, -5.929, -6.395, -6.308, -6.226, -6.226, -5.932,
            -6.292, -6.292, -6.153, -5.929, -6.292, -6.174, -6.395,
        ];
        let stops: Vec<Option<f64>> = v.iter().map(|x| Some(*x)).collect();
        let found = visible_steps(&stops, &[], 0.5);
        assert!(found.is_empty(), "{found:?}");
        // The same noise with a real step of -2 from the 30th value on: found (smaller ones are
        // left to the deflicker in noise like this).
        let stepped: Vec<Option<f64>> = v
            .iter()
            .enumerate()
            .map(|(i, x)| Some(if i >= 30 { x - 2.0 } else { *x }))
            .collect();
        let found = visible_steps(&stepped, &[], 0.5);
        assert!(found.iter().any(|s| s.frame == 30), "{found:?}");
    }

    use super::*;

    #[test]
    fn steps_without_exif_are_seen_in_the_pictures() {
        // A steep fall of light, +2 stops at frame 20 (the aperture opened), a flash at 30,
        // +0.33 at 40 (too small to tell from the scene).
        let stops: Vec<Option<f64>> = (0..60)
            .map(|i| {
                let mut v = -0.1 * f64::from(i);
                if i >= 20 {
                    v += 2.0;
                }
                if i == 30 {
                    v += 1.5;
                }
                if i >= 40 {
                    v += 0.33;
                }
                Some(v)
            })
            .collect();
        let found = visible_steps(&stops, &[], 0.5);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].frame, 20);
        assert!((found[0].stops - 2.0).abs() < 0.1);
        // Street lights coming on in the lower half: the whole frame jumps, its tiles not alike.
        let lit: Vec<Vec<Option<f64>>> = (0..60)
            .map(|i| {
                let base = -0.1 * f64::from(i);
                let lights = if i >= 20 { 2.0 } else { 0.0 };
                vec![Some(base), Some(base), Some(base + lights), Some(base + lights)]
            })
            .collect();
        let whole: Vec<Option<f64>> = (0..60)
            .map(|i| Some(-0.1 * f64::from(i) + if i >= 20 { 1.0 } else { 0.0 }))
            .collect();
        assert!(visible_steps(&whole, &lit, 0.5).is_empty());
        // The camera's step moves every tile alike: found.
        let stepped: Vec<Vec<Option<f64>>> = whole.iter().map(|w| vec![*w; 4]).collect();
        assert_eq!(visible_steps(&whole, &stepped, 0.5).len(), 1);
        let c = compensation_of(60, &found);
        assert_eq!(c[19], 0.0);
        assert!((c[20] + found[0].stops).abs() < 1e-12);
    }

    fn exp(shutter: f64, aperture: f64, iso: f64) -> Option<Exposure> {
        Some(Exposure::new(shutter, aperture, iso).unwrap())
    }

    #[test]
    fn validation() {
        assert_eq!(Exposure::new(0.0, 2.8, 100.0), Err(ExposureError::Shutter));
        assert_eq!(Exposure::new(1.0, f64::NAN, 100.0), Err(ExposureError::Aperture));
        assert_eq!(Exposure::new(1.0, 2.8, -1.0), Err(ExposureError::Iso));
        assert!(Exposure::new(1.0 / 8000.0, 1.4, 50.0).is_ok());
    }

    #[test]
    fn nominal_third_stop_values_snap_to_exact_steps() {
        // 1/60 → 1/80 is one third of a stop darker, though the nominal ratio is 0.415 stops.
        let a = Exposure::new(1.0 / 60.0, 5.6, 100.0).unwrap();
        let b = Exposure::new(1.0 / 80.0, 5.6, 100.0).unwrap();
        let d = a.stops(StepGrid::Thirds) - b.stops(StepGrid::Thirds);
        assert!((d - 1.0 / 3.0).abs() < 1e-12, "{d}");
        let exact = a.stops(StepGrid::Exact) - b.stops(StepGrid::Exact);
        assert!((exact - 0.415).abs() < 0.001, "{exact}");
        // ISO 640 is 2⅔ stops above ISO 100.
        let iso = Exposure::new(1.0, 1.0, 640.0).unwrap().stops(StepGrid::Thirds);
        assert!((iso - 8.0 / 3.0).abs() < 1e-12, "{iso}");
    }

    #[test]
    fn compensation_cancels_steps_and_fills_gaps() {
        let frames = vec![
            None,
            exp(1.0 / 125.0, 2.8, 100.0),
            exp(1.0 / 60.0, 2.8, 100.0), // one stop more light
            None,                        // keeps 1/60
            exp(1.0 / 60.0, 2.8, 200.0), // another stop
        ];
        let comp = ramp_compensation(&frames, 1, StepGrid::Thirds).unwrap();
        let expected = [0.0, 0.0, -1.0, -1.0, -2.0];
        for (c, e) in comp.iter().zip(expected) {
            assert!((c - e).abs() < 1e-12, "{comp:?}");
        }
        let steps = exposure_steps(&comp, 0.1);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].frame, 2);
        assert!((steps[0].stops - 1.0).abs() < 1e-12);
        assert_eq!(steps[1].frame, 4);
    }

    #[test]
    fn verified_compensation_keeps_real_steps_and_drops_phantom_ones() {
        // Light rises 0.01 stop per frame. The camera really steps down one stop at frame 50; at frame
        // 100 the EXIF claims another stop, but the sensor data shows no jump (continuous AE).
        let n = 150;
        let measured: Vec<Option<f64>> = (0..n)
            .map(|i| Some(-6.0 + 0.01 * i as f64 - if i >= 50 { 1.0 } else { 0.0 }))
            .collect();
        let steps = [
            ExposureStep {
                frame: 50,
                stops: -1.0,
            },
            ExposureStep {
                frame: 100,
                stops: -1.0,
            },
        ];
        let (comp, checks) = verified_compensation(n, &steps, &measured, 10, 0.1);
        assert!((checks[0].measured.unwrap() + 1.0).abs() < 1e-9);
        assert_eq!(checks[0].used, -1.0);
        assert!(checks[1].measured.unwrap().abs() < 1e-9);
        assert!(checks[1].used.abs() < 1e-9);
        assert_eq!(comp[49], 0.0);
        assert!((comp[50] - 1.0).abs() < 1e-9);
        assert!((comp[149] - 1.0).abs() < 1e-9);
        // Compensated, the series is the smooth light curve.
        for i in 1..n {
            let a = measured[i - 1].unwrap() + comp[i - 1];
            let b = measured[i].unwrap() + comp[i];
            assert!((b - a - 0.01).abs() < 1e-9, "frame {i}");
        }
    }

    #[test]
    fn unmeasurable_steps_of_a_continuous_camera_are_dropped() {
        // Flat light, three phantom steps measured, one at frame 1 without a left side to fit.
        let measured: Vec<Option<f64>> = (0..100).map(|_| Some(-5.0)).collect();
        let steps: Vec<ExposureStep> = [1, 30, 60, 90]
            .iter()
            .map(|&frame| ExposureStep {
                frame,
                stops: -1.0 / 3.0,
            })
            .collect();
        let (comp, checks) = verified_compensation(100, &steps, &measured, 10, 0.1);
        assert_eq!(checks[0].measured, None);
        assert_eq!(checks[0].used, 0.0);
        assert!(comp.iter().all(|c| c.abs() < 1e-9), "{comp:?}");
    }

    #[test]
    fn verified_compensation_without_measurements_trusts_exif() {
        let steps = [ExposureStep { frame: 2, stops: 0.5 }];
        let (comp, checks) = verified_compensation(4, &steps, &[None, None, None, None], 5, 0.1);
        assert_eq!(checks[0].measured, None);
        assert_eq!(comp, vec![0.0, 0.0, -0.5, -0.5]);
    }

    #[test]
    fn compensation_edge_cases() {
        assert_eq!(ramp_compensation(&[], 0, StepGrid::Thirds), None);
        assert_eq!(ramp_compensation(&[None, None], 0, StepGrid::Thirds), None);
        assert_eq!(
            ramp_compensation(&[exp(1.0, 2.0, 100.0)], 1, StepGrid::Thirds),
            None
        );
        assert_eq!(
            ramp_compensation(&[exp(1.0, 2.0, 100.0)], 0, StepGrid::Thirds),
            Some(vec![0.0])
        );
    }
}
