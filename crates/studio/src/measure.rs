//! Measuring passes: every frame rendered small as linear Rec. 2020 and its brightness measured,
//! on worker threads.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lapsewerk_core::deflicker::{self, ImageView, MeasureOptions, Rect};
use lapsewerk_core::regional::{self, Grid};
use lapsewerk_core::stabilize::Thumb;
use lapsewerk_develop::{
    DeepSamples, DevelopPlan, Output, REC2020_LUMINANCE, Rendered, Sequence, raw_brightness,
};

use crate::{CANCELLED, Reporter, Step};

/// Size of the measuring renders and threads.
#[derive(Clone, Copy, Debug)]
pub struct PassConfig {
    /// Long edge of the measuring renders (pixels).
    pub edge: usize,
    /// Frames measured at a time.
    pub jobs: usize,
}

/// A frame's settings as it is measured: without the point curves, the tone curve's regions and
/// the contrast.
/// They change every frame alike (no flicker), but bend how brightness follows exposure: an S-curve
/// made the deflicker's corrections, worked out in its bent brightness, miss by half (0.026 stops
/// of flicker left instead of 0.003).
#[must_use]
pub fn for_measuring(mut d: lapsewerk_develop::DevelopSettings) -> lapsewerk_develop::DevelopSettings {
    let c = &mut d.curve;
    c.master.clear();
    c.red.clear();
    c.green.clear();
    c.blue.clear();
    (c.highlights, c.lights, c.darks, c.shadows) = (0.0, 0.0, 0.0, 0.0);
    // Contrast is such a curve too (an S around the middle grey): +70 left 0.017 stops of flicker.
    d.light.contrast = 0.0;
    // So are shadows and highlights (they lift or lower a tonal range, each frame by its own
    // content): shadows +40 at a night keyframe threw the check pass away (0.014 stops left at
    // the night's end).
    (d.light.shadows, d.light.highlights) = (0.0, 0.0);
    d
}

/// What a pass measured.
#[derive(Clone, Debug)]
pub struct Pass {
    /// Developed brightness in stops (trimmed log mean), per frame.
    pub stops: Vec<Option<f64>>,
    /// Raw sensor brightness in stops, per frame, when asked for.
    pub raw: Vec<Option<f64>>,
    pub failures: Vec<(usize, String)>,
    /// Perceived brightness (log2 of the mean encoded luminance) per frame, for reporting.
    pub perceived: Vec<Option<f64>>,
    /// Brightness of each tile per frame (empty when no grid was asked for).
    pub tiles: Vec<Vec<Option<f64>>>,
    /// Luminance thumbs for measuring motion, per frame, when asked for.
    pub thumbs: Vec<Option<Thumb>>,
    /// Size of each frame's render: its aspect ratio, crop included.
    pub sizes: Vec<Option<(usize, usize)>>,
    /// Share of each frame at white (burnt-out highlights), per frame.
    pub clipped: Vec<Option<f32>>,
    pub time: Duration,
}

/// What else a pass does besides measuring the whole frame.
#[derive(Clone, Copy, Default)]
pub struct Extras<'a> {
    /// Measure the raw sensor brightness too.
    pub raw: bool,
    /// Keep a luminance thumb of every frame (for stabilization).
    pub thumbs: bool,
    /// Measure each tile of this grid too.
    pub grid: Option<Grid>,
    /// Apply these regional corrections (per frame, per tile of `grid`) before measuring.
    pub regional: Option<&'a [Vec<f64>]>,
    /// Measure the frame's brightness only inside this area (`--reference`); tiles keep their own.
    pub reference: Option<Rect>,
    /// What an earlier pass measured, and which frames to measure again (`true`); the others keep
    /// its values (a keyframe changed outside: only the frames whose development changed).
    pub kept: Option<(&'a Pass, &'a [bool])>,
}

/// How much brighter the keyframes' settings make each frame than the first keyframe's settings
/// would (stops): measured at every other keyframe (`as_shot` holds the frames measured with their
/// own settings), interpolated linearly between keyframes, held before the first and after the
/// last. All zeros with one keyframe, or where a measurement fails.
///
/// # Errors
/// When a render job cannot be made.
pub fn keyframe_edits(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    as_shot: &[Option<f64>],
    offsets: &[f64],
    cfg: PassConfig,
    reference: Option<Rect>,
) -> Result<Vec<f64>, String> {
    let n = seq.len();
    let keys: Vec<usize> = plan.keyframes().iter().copied().filter(|&k| k < n).collect();
    let Some(&first) = keys.first() else {
        return Ok(vec![0.0; n]);
    };
    if keys.len() < 2 {
        return Ok(vec![0.0; n]);
    }
    let base = for_measuring(plan.settings(first, 0.0));
    let opts = MeasureOptions {
        luminance: REC2020_LUMINANCE,
        roi: reference,
        ..Default::default()
    };
    // Where the effect is measured: at every keyframe, and between two about every 20 frames (up
    // to 8): the looks blend along a smooth curve, not a straight line, and a straight line
    // between keyframes left a slow wave of half a stop where a strong look changes.
    let mut samples: Vec<usize> = Vec::new();
    for w in keys.windows(2) {
        if let [a, b] = *w {
            let parts = (b - a).div_ceil(20).clamp(1, 8);
            samples.extend((0..parts).map(|j| a + (b - a) * j / parts));
        }
    }
    samples.extend(keys.last().copied());
    samples.dedup();
    // (frame, effect) at every sample.
    let mut at: Vec<(usize, f64)> = vec![(first, 0.0)];
    for &k in samples.iter().filter(|k| **k != first) {
        // The keyframe's frame with the first keyframe's settings (its own crop and orientation are
        // the same for a sequence).
        // Exposed as the measurement was (its offset, taken off again), so both burn out alike.
        let offset = offsets.get(k).copied().unwrap_or(0.0);
        let mut settings = base.clone();
        settings.light.exposure += offset;
        let job = seq
            .render_job(k, settings, cfg.edge, cfg.edge, Output::LinearRec2020)
            .map_err(|e| e.to_string())?;
        let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.run()))
            .map_err(|_| "the renderer crashed".to_owned())
            .and_then(|done| done.rendered);
        let plain = rendered
            .ok()
            .and_then(|r| measure_rendered(&r, &opts, None, None).ok())
            .and_then(|m| m.0)
            .map(|v| v - offset);
        let effect = match (as_shot.get(k).copied().flatten(), plain) {
            (Some(with), Some(without)) if (with - without).is_finite() => with - without,
            _ => 0.0,
        };
        at.push((k, effect));
    }
    Ok(interpolate_edits(&at, n))
}

/// Values given at some frames (sorted), linear in between, held at the ends.
fn interpolate_edits(at: &[(usize, f64)], n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| {
            let after = at.iter().position(|&(f, _)| f >= i);
            match after {
                None => at.last().map_or(0.0, |p| p.1),
                Some(0) => at.first().map_or(0.0, |p| p.1),
                Some(j) => {
                    let (Some(&(f0, v0)), Some(&(f1, v1))) = (at.get(j - 1), at.get(j)) else {
                        return 0.0;
                    };
                    if f1 == f0 {
                        v1
                    } else {
                        v0 + (v1 - v0) * (i - f0) as f64 / (f1 - f0) as f64
                    }
                }
            }
        })
        .collect()
}

/// Renders every frame with `plan` plus `offsets` (stops) to fit `edge` and measures it.
///
/// # Errors
/// When a render job cannot be made, or [`CANCELLED`] when the reporter asks to stop.
pub fn pass(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    offsets: &[f64],
    cfg: PassConfig,
    extras: Extras<'_>,
    step: Step,
    rep: &dyn Reporter,
) -> Result<Pass, String> {
    let PassConfig { edge, jobs } = cfg;
    let with_raw = extras.raw;
    let n = seq.len();
    // Kept from an earlier pass: those frames are not rendered again (all of them when its
    // lengths do not fit, or thumbs are needed, which it does not keep).
    let kept = extras.kept.filter(|(k, again)| {
        !extras.thumbs
            && again.len() == n
            && k.stops.len() == n
            && k.raw.len() == n
            && k.tiles.len() == n
            && k.perceived.len() == n
            && k.sizes.len() == n
            && k.clipped.len() == n
    });
    let keep = |i: usize| kept.is_some_and(|(_, again)| again.get(i) == Some(&false));
    // The check pass lifts every frame to the day's brightness: with a white balance that turns
    // towards the night (3500 K), one channel is lifted past white there and its pixels drop out
    // of the measurement frame by frame, which then follows corrections only in part. It measures
    // with one white balance for all frames (the first's): the brightness, not the colour.
    let one_wb = (step == Step::Check).then(|| plan.settings(0, 0.0).wb);
    let mut queue = Vec::with_capacity(n);
    for i in 0..n {
        if keep(i) {
            queue.push(Mutex::new(None));
            continue;
        }
        // Verifying measures the frames as they will look (curve and contrast too): it reports
        // what is left to see; working the corrections out leaves them out.
        let full = plan.settings(i, offsets.get(i).copied().unwrap_or(0.0));
        let mut settings = if step == Step::Verify {
            full
        } else {
            for_measuring(full)
        };
        if let Some(wb) = one_wb {
            settings.wb = wb;
        }
        queue.push(Mutex::new(Some(
            seq.render_job(i, settings, edge, edge, Output::LinearRec2020)
                .map_err(|e| e.to_string())?,
        )));
    }
    let from_kept = |f: &dyn Fn(&Pass, usize) -> Option<f64>| -> Vec<Mutex<Option<f64>>> {
        (0..n)
            .map(|i| Mutex::new(kept.filter(|_| keep(i)).and_then(|(k, _)| f(k, i))))
            .collect()
    };
    let measured = from_kept(&|k, i| k.stops.get(i).copied().flatten());
    let tiles: Vec<Mutex<Vec<Option<f64>>>> = (0..n)
        .map(|i| {
            Mutex::new(
                kept.filter(|_| keep(i))
                    .and_then(|(k, _)| k.tiles.get(i).cloned())
                    .unwrap_or_default(),
            )
        })
        .collect();
    let thumbs: Vec<Mutex<Option<Thumb>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let perceived = from_kept(&|k, i| k.perceived.get(i).copied().flatten());
    let clipped: Vec<Mutex<Option<f32>>> = (0..n)
        .map(|i| {
            Mutex::new(
                kept.filter(|_| keep(i))
                    .and_then(|(k, _)| k.clipped.get(i).copied().flatten()),
            )
        })
        .collect();
    let raw = from_kept(&|k, i| k.raw.get(i).copied().flatten());
    let sizes: Vec<Mutex<Option<(usize, usize)>>> = (0..n)
        .map(|i| {
            Mutex::new(
                kept.filter(|_| keep(i))
                    .and_then(|(k, _)| k.sizes.get(i).copied().flatten()),
            )
        })
        .collect();
    let again = (0..n).filter(|&i| !keep(i)).count();
    let paths: Vec<PathBuf> = seq.frames().iter().map(|f| f.path.clone()).collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    // A kept frame that failed then fails now too.
    let failures = Mutex::new(
        kept.map(|(k, _)| {
            k.failures
                .iter()
                .filter(|(i, _)| keep(*i))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default(),
    );
    let opts = MeasureOptions {
        luminance: REC2020_LUMINANCE,
        roi: extras.reference,
        ..Default::default()
    };
    let started = Instant::now();
    rep.progress(step, 0, again);
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    if rep.cancelled() {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(slot) = queue.get(i) else { break };
                    let Some(job) = slot.lock().ok().and_then(|mut j| j.take()) else {
                        continue;
                    };
                    let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.run()))
                        .map_err(|_| "the renderer crashed".to_owned())
                        .and_then(|done| done.rendered);
                    let value = match rendered {
                        Ok(r) => {
                            if let (Some(d), Some(Ok(mut s))) = (&r.deep, sizes.get(i).map(Mutex::lock)) {
                                *s = Some((d.width, d.height));
                            }
                            if extras.thumbs
                                && let (Some(d), Some(slot)) = (&r.deep, thumbs.get(i))
                                && let lapsewerk_develop::DeepSamples::F32(data) = &d.samples
                                && let Ok(mut slot) = slot.lock()
                            {
                                *slot = Thumb::from_linear_rgb(data, d.width, d.height, REC2020_LUMINANCE);
                            }
                            let regional = extras.regional.and_then(|r| r.get(i)).map(Vec::as_slice);
                            measure_rendered(&r, &opts, extras.grid, regional).map(|(v, t, p, c)| {
                                if let Some(slot) = perceived.get(i)
                                    && let Ok(mut slot) = slot.lock()
                                {
                                    *slot = p;
                                }
                                if let Some(slot) = clipped.get(i)
                                    && let Ok(mut slot) = slot.lock()
                                {
                                    *slot = c;
                                }
                                if let Some(slot) = tiles.get(i)
                                    && let Ok(mut slot) = slot.lock()
                                {
                                    *slot = t;
                                }
                                v
                            })
                        }
                        Err(e) => Err(e),
                    };
                    match value {
                        Ok(v) => {
                            if let Some(m) = measured.get(i)
                                && let Ok(mut m) = m.lock()
                            {
                                *m = v;
                            }
                        }
                        Err(e) => {
                            if let Ok(mut f) = failures.lock() {
                                f.push((i, e));
                            }
                        }
                    }
                    if with_raw
                        && let (Some(path), Some(slot)) = (paths.get(i), raw.get(i))
                        && let Ok(mut slot) = slot.lock()
                    {
                        *slot = raw_brightness(path);
                    }
                    let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                    rep.progress(step, d, again);
                }
            });
        }
    });
    if rep.cancelled() {
        return Err(CANCELLED.into());
    }
    let mut failures = failures.into_inner().unwrap_or_default();
    failures.sort_by_key(|f| f.0);
    Ok(Pass {
        stops: measured
            .into_iter()
            .map(|m| m.into_inner().ok().flatten())
            .collect(),
        raw: raw.into_iter().map(|m| m.into_inner().ok().flatten()).collect(),
        tiles: tiles
            .into_iter()
            .map(|m| m.into_inner().unwrap_or_default())
            .collect(),
        thumbs: thumbs
            .into_iter()
            .map(|m| m.into_inner().ok().flatten())
            .collect(),
        perceived: perceived
            .into_iter()
            .map(|m| m.into_inner().ok().flatten())
            .collect(),
        failures,
        sizes: sizes.into_iter().map(|m| m.into_inner().ok().flatten()).collect(),
        clipped: clipped
            .into_iter()
            .map(|m| m.into_inner().ok().flatten())
            .collect(),
        time: started.elapsed(),
    })
}

/// A frame's brightness (log mean), its tiles', its perceived brightness and its share at white.
type FrameMeasures = (Option<f64>, Vec<Option<f64>>, Option<f64>, Option<f32>);

/// The whole frame's brightness and, with a grid, each tile's; `regional` corrections (one per tile)
/// are applied first.
fn measure_rendered(
    r: &Rendered,
    opts: &MeasureOptions,
    grid: Option<Grid>,
    regional: Option<&[f64]>,
) -> Result<FrameMeasures, String> {
    let deep = r.deep.as_ref().ok_or("no float output")?;
    let DeepSamples::F32(data) = &deep.samples else {
        return Err("unexpected sample format".into());
    };
    let corrected;
    let data = match (grid, regional) {
        (Some(g), Some(stops)) => {
            let mut d = data.clone();
            regional::apply_linear(&mut d, deep.width, deep.height, g, stops);
            corrected = d;
            &corrected
        }
        _ => data,
    };
    let view = ImageView {
        width: deep.width,
        height: deep.height,
        channels: 3,
        data,
    };
    let tiles = grid
        .map(|g| regional::measure_tiles(view, g, opts))
        .unwrap_or_default();
    Ok((
        deflicker::measure(view, opts).map(|m| m.stops),
        tiles,
        deflicker::perceived(view, opts),
        // White in the measuring render (linear, 0..1): burnt out.
        lapsewerk_core::quality::clipped_fraction(view, 0.995),
    ))
}

/// Frame-to-frame jitter: RMS of second differences (a steady trend contributes nothing).
pub fn jitter(values: &[Option<f64>]) -> f64 {
    let d2: Vec<f64> = values
        .windows(3)
        .filter_map(|w| match w {
            [Some(a), Some(b), Some(c)] => Some(a - 2.0 * b + c),
            _ => None,
        })
        .collect();
    if d2.is_empty() {
        return 0.0;
    }
    (d2.iter().map(|v| v * v).sum::<f64>() / d2.len() as f64).sqrt()
}

/// Half the window of the trend a slow wave is measured against (frames each side).
const WAVE_TREND: usize = 20;

/// Half the window that takes frame-to-frame flicker out of a slow wave (frames each side).
const WAVE_SHORT: usize = 2;

/// A slow wave (a swell over about 8 to 30 frames that [`jitter`] does not see, as its second
/// differences are small): the brightness averaged over 5 frames against a quadratic trend over 41
/// (a ramp and its knees follow the trend). Its largest swing, as the frame (from 0) where the
/// brightness stands furthest from the trend and how far (stops), when there are enough frames.
pub fn slow_wave(values: &[Option<f64>]) -> Option<(usize, f64)> {
    let n = values.len();
    if n < 2 * WAVE_TREND + 1 {
        return None;
    }
    (WAVE_TREND..n - WAVE_TREND)
        .filter_map(|i| Some((i, wave_at(values, i)?)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// Where the slow wave found at `at` lies, as frames (from 0, inclusive): every frame within twice
/// the trend's reach where it is at least 70 % as large, 10 frames either side. A swell up and down
/// again has two near-equal peaks: named as one stretch, whichever of them measured larger.
#[must_use]
pub fn wave_span(values: &[Option<f64>], at: usize) -> (usize, usize) {
    let n = values.len();
    let peak = wave_at(values, at).unwrap_or(0.0);
    let near: Vec<usize> = (at.saturating_sub(2 * WAVE_TREND)..=at + 2 * WAVE_TREND)
        .filter(|&i| wave_at(values, i).is_some_and(|a| a >= 0.7 * peak))
        .collect();
    let first = near.first().copied().unwrap_or(at);
    let last = near.last().copied().unwrap_or(at);
    (first.saturating_sub(10), (last + 10).min(n.saturating_sub(1)))
}

/// The slow swells over `limit` stops, largest first, each at least 41 frames (the trend's
/// window) from a larger one: a day-to-night-to-day series has one at each knee.
pub fn slow_waves(values: &[Option<f64>], limit: f64) -> Vec<(usize, f64)> {
    let n = values.len();
    if n < 2 * WAVE_TREND + 1 {
        return Vec::new();
    }
    let mut all: Vec<(usize, f64)> = (WAVE_TREND..n - WAVE_TREND)
        .filter_map(|i| Some((i, wave_at(values, i)?)))
        .filter(|(_, a)| *a > limit)
        .collect();
    all.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut picked: Vec<(usize, f64)> = Vec::new();
    for (i, a) in all {
        if picked.iter().all(|(p, _)| p.abs_diff(i) > 2 * WAVE_TREND) {
            picked.push((i, a));
        }
        if picked.len() == 4 {
            break;
        }
    }
    picked
}

/// How far the brightness around frame `at` (averaged over 5 frames) stands from the quadratic
/// trend over 41 ([`slow_wave`] there); `None` near the ends or without enough values.
pub fn wave_at(values: &[Option<f64>], at: usize) -> Option<f64> {
    let n = values.len();
    if at < WAVE_TREND || at + WAVE_TREND >= n {
        return None;
    }
    let w: Vec<f64> = values
        .get(at.saturating_sub(WAVE_SHORT)..(at + WAVE_SHORT + 1).min(n))?
        .iter()
        .flatten()
        .copied()
        .collect();
    if w.len() <= WAVE_SHORT {
        return None;
    }
    let here = w.iter().sum::<f64>() / w.len() as f64;
    Some((here - quadratic_at(values, at, WAVE_TREND)?).abs())
}

/// The least-squares parabola through the values within `half` frames of `at`, at `at` (`None`
/// with too few values or a degenerate fit).
fn quadratic_at(values: &[Option<f64>], at: usize, half: usize) -> Option<f64> {
    // Sums of x^k (k to 4) and x^k*y (k to 2), x centred on `at`.
    let (mut sx, mut sy) = ([0.0f64; 5], [0.0f64; 3]);
    let from = at.saturating_sub(half);
    for (i, v) in values.iter().enumerate().skip(from).take(2 * half + 1) {
        let Some(y) = v else { continue };
        let x = i as f64 - at as f64;
        let mut p = 1.0;
        for (k, s) in sx.iter_mut().enumerate() {
            *s += p;
            if let Some(t) = sy.get_mut(k) {
                *t += p * y;
            }
            p *= x;
        }
    }
    if sx[0] < (half + 1) as f64 {
        return None;
    }
    // Normal equations [s0 s1 s2; s1 s2 s3; s2 s3 s4] (a b c) = (y0 y1 y2); the value at x=0 is a.
    let m = [
        [sx[0], sx[1], sx[2]],
        [sx[1], sx[2], sx[3]],
        [sx[2], sx[3], sx[4]],
    ];
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(m);
    if d.abs() < 1e-9 {
        return None;
    }
    let mut ma = m;
    for (row, y) in ma.iter_mut().zip(sy) {
        row[0] = y;
    }
    let a = det(ma) / d;
    a.is_finite().then_some(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_knee_s_swell_is_named() {
        // Two swells, at frames 60 and 200 (about 0.06 and 0.05 stops from a smooth course).
        let v: Vec<Option<f64>> = (0..260)
            .map(|i| {
                let bump = |at: f64, a: f64| a * (-((i as f64 - at) / 5.0).powi(2)).exp();
                Some(bump(60.0, 0.13) + bump(200.0, 0.11))
            })
            .collect();
        let found = slow_waves(&v, 0.03);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found[0].0.abs_diff(60) <= 2 && found[1].0.abs_diff(200) <= 2,
            "{found:?}"
        );
        assert!(slow_waves(&v[..20], 0.03).is_empty());
    }

    #[test]
    fn a_slow_wave_is_found_where_jitter_sees_little() {
        // A day-to-night ramp with a soft knee: no wave.
        let ramp: Vec<Option<f64>> = (0..200)
            .map(|i| {
                let x = (f64::from(i) - 100.0) / 25.0;
                Some(-2.0 * (1.0 + x.tanh()) / 2.0)
            })
            .collect();
        assert!(
            slow_wave(&ramp).is_some_and(|(_, a)| a < 0.01),
            "{:?}",
            slow_wave(&ramp)
        );
        // A swell of ±0.05 stops over 16 frames at frames 80-120, on the ramp.
        let waved: Vec<Option<f64>> = ramp
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let swell = if (80..120).contains(&i) {
                    0.05 * (std::f64::consts::TAU * i as f64 / 16.0).sin()
                } else {
                    0.0
                };
                v.map(|v| v + swell)
            })
            .collect();
        let (at, a) = slow_wave(&waved).unwrap();
        assert!((80..120).contains(&at) && a > 0.03, "{at} {a}");
        // Its frame-to-frame flicker is small: jitter alone would not say it.
        assert!(jitter(&waved) < 0.01, "{}", jitter(&waved));
        // Too short, or gaps everywhere: nothing to say, no panic.
        assert!(slow_wave(&ramp[..30]).is_none());
        assert!(slow_wave(&vec![None; 100]).is_none());
    }

    #[test]
    fn what_bends_brightness_by_content_is_not_measured() {
        let mut d = lapsewerk_develop::DevelopSettings::default();
        d.light.exposure = 0.5;
        d.light.contrast = 40.0;
        d.light.shadows = 40.0;
        d.light.highlights = -30.0;
        let m = for_measuring(d);
        assert_eq!(m.light.exposure, 0.5);
        assert_eq!(
            (m.light.contrast, m.light.shadows, m.light.highlights),
            (0.0, 0.0, 0.0)
        );
    }

    #[test]
    fn keyframe_edits_are_interpolated_between_keyframes() {
        let v = interpolate_edits(&[(2, 0.0), (6, 1.0), (8, 0.0)], 10);
        let want = [0.0, 0.0, 0.0, 0.25, 0.5, 0.75, 1.0, 0.5, 0.0, 0.0];
        for (a, b) in v.iter().zip(want) {
            assert!((a - b).abs() < 1e-12, "{v:?}");
        }
        assert_eq!(interpolate_edits(&[], 3), vec![0.0; 3]);
    }
}
