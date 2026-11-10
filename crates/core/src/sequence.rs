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
}
