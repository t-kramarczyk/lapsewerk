//! Sensor dust: frames spread over the sequence rendered as they will be in the movie (but small),
//! and the dust found in them ([`lapsewerk_core::dust`]).

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use lapsewerk_core::dust::{self, DustMap, DustParams, Luma};
use lapsewerk_develop::{DeepSamples, DevelopPlan, Output, OutputSpace, Sequence};

use crate::{CANCELLED, Reporter, Step};

/// Long edge of the renders dust is looked for in.
const EDGE: usize = 1536;

/// Frames looked at, spread over the sequence.
const FRAMES: usize = 24;

/// The dust in `seq` as rendered with `plan` and `offsets`; `None` when too few frames rendered.
/// `video` is the movie's size, to report spot sizes in its pixels.
///
/// # Errors
/// When a render job cannot be made, or [`CANCELLED`].
pub fn find(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    offsets: &[f64],
    jobs: usize,
    video: (usize, usize),
    rep: &dyn Reporter,
) -> Result<Option<DustMap>, String> {
    let started = Instant::now();
    let n = seq.len();
    let picks = spread(n, FRAMES);
    let mut queue = Vec::with_capacity(picks.len());
    for &i in &picks {
        let settings = plan.settings(i, offsets.get(i).copied().unwrap_or(0.0));
        queue.push(Mutex::new(Some(
            seq.render_job(i, settings, EDGE, EDGE, Output::Display16(OutputSpace::Srgb))
                .map_err(|e| e.to_string())?,
        )));
    }
    let frames: Vec<Mutex<Option<Luma>>> = picks.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let total = picks.len();
    rep.progress(Step::Dust, 0, total);
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1) {
            s.spawn(|| {
                loop {
                    if rep.cancelled() {
                        return;
                    }
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = queue
                        .get(k)
                        .and_then(|j| j.lock().ok().and_then(|mut j| j.take()))
                    else {
                        return;
                    };
                    // A crash in one frame leaves it out; the others still tell the dust.
                    let luma = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.run()))
                        .ok()
                        .and_then(|done| done.rendered.ok())
                        .and_then(|r| r.deep)
                        .and_then(|d| match d.samples {
                            DeepSamples::U16(data) => Luma::from_srgb16(&data, d.width, d.height),
                            DeepSamples::F32(_) => None,
                        });
                    if let Some(slot) = frames.get(k)
                        && let Ok(mut slot) = slot.lock()
                    {
                        *slot = luma;
                    }
                    rep.progress(Step::Dust, done.fetch_add(1, Ordering::Relaxed) + 1, total);
                }
            });
        }
    });
    if rep.cancelled() {
        return Err(CANCELLED.into());
    }
    let frames: Vec<Luma> = frames
        .into_iter()
        .filter_map(|f| f.into_inner().ok().flatten())
        .collect();
    let rendered = started.elapsed();
    let Some(map) = dust::estimate(&frames, &DustParams::default()) else {
        rep.note(&format!(
            "dust:          too few frames to tell ({} of {} rendered)",
            frames.len(),
            picks.len()
        ));
        return Ok(None);
    };
    // Spot sizes in movie pixels (the map covers the whole frame; the movie, its short side at most).
    let scale = video.0.min(video.1) as f32 / map.width.min(map.height) as f32;
    let deepest = map.spots.iter().map(|s| s.depth).fold(0.0f32, f32::max);
    let largest = map.spots.iter().map(|s| s.radius).fold(0.0f32, f32::max);
    if map.is_empty() {
        rep.note(&format!(
            "dust:          none found in {} frames ({:.1} s)",
            frames.len(),
            started.elapsed().as_secs_f64()
        ));
    } else {
        rep.note(&format!(
            "dust:          {} spot(s) in {} frames, darkest {:.0} %, largest radius {:.0} px in the movie (render {:.1} s, find {:.1} s)",
            map.spots.len(),
            frames.len(),
            deepest * 100.0,
            largest * scale,
            rendered.as_secs_f64(),
            (started.elapsed() - rendered).as_secs_f64()
        ));
    }
    for spot in map.spots.iter().take(8) {
        rep.note(&format!(
            "               at {:.0} %, {:.0} % (from the left, top): radius {:.0} px, {:.1} % darker",
            spot.x / map.width as f32 * 100.0,
            spot.y / map.height as f32 * 100.0,
            spot.radius * scale,
            spot.depth * 100.0
        ));
    }
    Ok(Some(map))
}

/// Up to `count` frame numbers of `n`, spread evenly, ends included.
fn spread(n: usize, count: usize) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    if n <= count {
        return (0..n).collect();
    }
    let mut out: Vec<usize> = (0..count).map(|k| k * (n - 1) / (count - 1).max(1)).collect();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::spread;

    #[test]
    fn spreads_over_the_sequence() {
        assert_eq!(spread(5, 24), vec![0, 1, 2, 3, 4]);
        let s = spread(169, 24);
        assert_eq!(s.len(), 24);
        assert_eq!((s[0], s[23]), (0, 168));
        assert!(spread(0, 24).is_empty());
    }
}
