//! A sequence of frames and what can be learned from their capture times.

use std::path::PathBuf;

use crate::exposure::Exposure;

/// One frame of a sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// The original file; never written to.
    pub path: PathBuf,
    /// Capture time in milliseconds since the Unix epoch (camera clock, no time zone).
    pub captured_ms: Option<i64>,
    /// Camera settings, when the file has them.
    pub exposure: Option<Exposure>,
}

/// Timing of a sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct Timing {
    /// Median time between consecutive frames, in milliseconds.
    pub interval_ms: i64,
    /// Frames that follow a pause noticeably longer than the interval (the shoot was interrupted,
    /// a card was changed, or frames are missing).
    pub gaps_before: Vec<usize>,
    /// Frames whose capture time is earlier than the frame before (files out of order, or the camera
    /// clock was changed).
    pub out_of_order: Vec<usize>,
}

/// A pause counts as a gap when it is longer than one and a half intervals (at least one frame is
/// missing), as `quality::missing_frames` counts them.
const GAP_HALVES: i64 = 3;

/// Timing of `frames` in file order; `None` when fewer than two frames have capture times.
#[must_use]
pub fn timing(frames: &[Frame]) -> Option<Timing> {
    let times: Vec<(usize, i64)> = frames
        .iter()
        .enumerate()
        .filter_map(|(i, f)| Some((i, f.captured_ms?)))
        .collect();
    let mut deltas: Vec<i64> = times
        .windows(2)
        .filter_map(|w| match w {
            [(_, a), (_, b)] if b >= a => Some(b - a),
            _ => None,
        })
        .collect();
    if deltas.is_empty() {
        return None;
    }
    deltas.sort_unstable();
    let interval_ms = deltas.get(deltas.len() / 2).copied()?;
    let mut gaps_before = Vec::new();
    let mut out_of_order = Vec::new();
    for w in times.windows(2) {
        let [(_, a), (j, b)] = w else { continue };
        if b < a {
            out_of_order.push(*j);
        } else if interval_ms > 0 && (b - a).saturating_mul(2) > GAP_HALVES.saturating_mul(interval_ms) {
            gaps_before.push(*j);
        }
    }
    Some(Timing {
        interval_ms,
        gaps_before,
        out_of_order,
    })
}

/// Frames at a series' start or end set apart from the rest by a long pause: test shots taken
/// before the series was started, or frames after it was stopped. Only offered to be left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StrayEnds {
    /// Frames before the series (0: none).
    pub lead: usize,
    /// From the first of them to the series' first frame (ms).
    pub lead_pause_ms: i64,
    /// Frames after the series (0: none).
    pub trail: usize,
    /// From the series' last frame to the last of them (ms).
    pub trail_pause_ms: i64,
}

/// A pause sets frames apart as strays when it is at least this many intervals…
const STRAY_INTERVALS: i64 = 4;

/// …and at least this long (ms): a card changed in a hurry is not a series stopped. (A test shot
/// looked at on the camera and the settings changed: about a minute.)
const STRAY_PAUSE_MS: i64 = 30_000;

/// The frames at the start and end of `frames` cut off from the series by a long pause
/// ([`STRAY_INTERVALS`], [`STRAY_PAUSE_MS`]), at most a quarter of the frames at each end; with
/// several such pauses near an end, the one nearest the series.
#[must_use]
pub fn stray_ends(frames: &[Frame], timing: &Timing) -> StrayEnds {
    let n = frames.len();
    let most = n / 4;
    let pause = |j: usize| -> Option<i64> {
        let b = frames.get(j)?.captured_ms?;
        let a = frames.get(j.checked_sub(1)?)?.captured_ms?;
        let p = b - a;
        (p >= STRAY_PAUSE_MS && p >= timing.interval_ms.saturating_mul(STRAY_INTERVALS)).then_some(p)
    };
    let mut ends = StrayEnds::default();
    for &j in &timing.gaps_before {
        if j > most {
            break;
        }
        if pause(j).is_some() {
            ends.lead = j;
        }
    }
    for &j in timing.gaps_before.iter().rev() {
        if n - j > most || j <= ends.lead {
            break;
        }
        if pause(j).is_some() {
            ends.trail = n - j;
        }
    }
    // How long before the series the first test shot was taken (after it, the last one): what
    // people remember, not the last pause alone.
    let at = |i: usize| frames.get(i).and_then(|f| f.captured_ms);
    if ends.lead > 0
        && let (Some(a), Some(b)) = (at(0), at(ends.lead))
    {
        ends.lead_pause_ms = b - a;
    }
    if ends.trail > 0
        && let (Some(a), Some(b)) = (at(n - ends.trail - 1), at(n - 1))
    {
        ends.trail_pause_ms = b - a;
    }
    ends
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ms: Option<i64>) -> Frame {
        Frame {
            path: PathBuf::from("x.arw"),
            captured_ms: ms,
            exposure: None,
        }
    }

    #[test]
    fn interval_gaps_and_order() {
        let frames: Vec<Frame> = [0, 5000, 10_000, 15_000, 60_000, 65_000, 64_000]
            .into_iter()
            .map(|t| frame(Some(t)))
            .collect();
        let t = timing(&frames).unwrap();
        assert_eq!(t.interval_ms, 5000);
        assert_eq!(t.gaps_before, vec![4]);
        assert_eq!(t.out_of_order, vec![6]);
    }

    #[test]
    fn missing_times() {
        assert_eq!(timing(&[frame(Some(1)), frame(None)]), None);
        let t = timing(&[frame(Some(0)), frame(None), frame(Some(2000)), frame(Some(4000))]).unwrap();
        assert_eq!(t.interval_ms, 2000);
        assert!(t.gaps_before.is_empty());
    }

    #[test]
    fn test_shots_before_and_after() {
        // Three test shots minutes apart, then 40 frames every 5 s, then one more 10 min later.
        let mut times = vec![0, 120_000, 240_000];
        times.extend((0..40).map(|k| 600_000 + k * 5000));
        times.push(600_000 + 39 * 5000 + 600_000);
        let frames: Vec<Frame> = times.iter().map(|t| frame(Some(*t))).collect();
        let t = timing(&frames).unwrap();
        let s = stray_ends(&frames, &t);
        assert_eq!((s.lead, s.lead_pause_ms), (3, 600_000));
        assert_eq!((s.trail, s.trail_pause_ms), (1, 600_000));
        // A card changed in 25 s near the start, or a long pause in the middle: no strays.
        let mut times: Vec<i64> = (0..5).map(|k| k * 5000).collect();
        times.extend((0..35).map(|k| 45_000 + k * 5000));
        times.extend((0..40).map(|k| 600_000 + k * 5000));
        let frames: Vec<Frame> = times.iter().map(|t| frame(Some(*t))).collect();
        let t = timing(&frames).unwrap();
        assert_eq!(stray_ends(&frames, &t), StrayEnds::default());
    }
}
