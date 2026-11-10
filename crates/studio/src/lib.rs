//! Lapsewerk's workflow, shared by the command line and the app: open a sequence, measure it,
//! work out the corrections and render the movie.
//!
//! Every long step reports through a [`Reporter`]: how far it is ([`Reporter::progress`], from
//! which [`Remaining`] estimates the time left), what it found ([`Reporter::note`], the lines the
//! command line prints), and whether to stop ([`Reporter::cancelled`]; the step then returns
//! [`CANCELLED`], and a movie being written is deleted).

pub mod cache;
pub mod dust;
pub mod export;
pub mod measure;
pub mod stage;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lapsewerk_core::deflicker::{self, DeflickerParams, Rect};
use lapsewerk_core::exposure::{self, StepCheck, StepGrid};
use lapsewerk_core::keyframes::Interpolation;
use lapsewerk_core::sequence;
use lapsewerk_develop::{DevelopPlan, OneLook, Sequence};
use lapsewerk_video::VideoSettings;

pub use measure::{Pass, PassConfig, jitter};

/// What a step returns when the reporter asked it to stop.
pub const CANCELLED: &str = "cancelled";

/// The long steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    /// Measuring every frame as shot.
    Measure,
    /// Measuring the frames as they will be rendered (the second deflicker pass).
    Check,
    /// Measuring the finished corrections.
    Verify,
    /// Looking for sensor dust.
    Dust,
    /// Rendering and writing the movie.
    Render,
    /// Reading photos for a camera profile.
    Calibrate,
}

impl Step {
    /// The word the command line prints for it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Measure => "measured",
            Self::Check => "checked",
            Self::Verify => "verified",
            Self::Dust => "dust",
            Self::Render => "rendered",
            Self::Calibrate => "read",
        }
    }
}

/// Receives what a step does. Called from worker threads.
pub trait Reporter: Sync {
    /// `done` of `total` items of `step` are finished (0 when it starts).
    fn progress(&self, step: Step, done: usize, total: usize);
    /// A finding, as a line of text (what the command line prints).
    fn note(&self, line: &str);
    /// Whether to stop as soon as possible.
    fn cancelled(&self) -> bool {
        false
    }
}

/// A reporter that ignores everything (tests, quiet use).
pub struct Quiet;

impl Reporter for Quiet {
    fn progress(&self, _: Step, _: usize, _: usize) {}
    fn note(&self, _: &str) {}
}

/// Time left of a step, from how fast it has gone so far.
#[derive(Clone, Copy, Debug)]
pub struct Remaining {
    started: Instant,
}

impl Remaining {
    #[must_use]
    pub fn start() -> Self {
        Self {
            started: Instant::now(),
        }
    }

    /// Since the step started.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Time left for `total - done` items at the rate so far; `None` before the first is done.
    #[must_use]
    pub fn left(&self, done: usize, total: usize) -> Option<Duration> {
        estimate(self.started.elapsed(), done, total)
    }
}

/// Time left after `elapsed` for `done` of `total` items, at the same rate.
#[must_use]
pub fn estimate(elapsed: Duration, done: usize, total: usize) -> Option<Duration> {
    if done == 0 || done > total {
        return None;
    }
    let per = elapsed.as_secs_f64() / done as f64;
    Duration::try_from_secs_f64(per * (total - done) as f64).ok()
}

/// A sequence's frames in a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frames {
    pub files: Vec<PathBuf>,
    /// JPEG/TIFF/HEIC files left out because a raw of the same name is there (RAW+JPEG).
    pub beside_raws: usize,
}

/// File types treated as frames.
pub const EXTENSIONS: &[&str] = &[
    "arw", "cr2", "cr3", "nef", "nrw", "raf", "orf", "rw2", "pef", "dng", "srw", "3fr", "iiq", "jpg", "jpeg",
    "tif", "tiff", "png", "heic",
];

/// A slow swell (stops away from a smooth course, [`measure::slow_wave`]) that `--verify` names:
/// the synthetic series without one stay under 0.002, the ramp's end in HG reads 0.046.
pub const SLOW_WAVE_SAID: f64 = 0.03;

/// What the movie is and how it is made.
#[derive(Clone, Debug)]
pub struct RenderArgs {
    pub out: PathBuf,
    pub video: VideoSettings,
    /// No `--size`: landscape or portrait UHD, following the frames.
    pub auto_size: bool,
    /// Crop position, 0..1 from the left and from the top.
    pub position: (f64, f64),
    /// Stabilize by the exact (sub-pixel) shift, resampled, instead of whole pixels.
    pub subpixel: bool,
    /// Write a folder of frames (`out` is the folder) instead of a movie.
    pub sequence: bool,
    /// The frames' files: 16-bit TIFF, 10-bit DPX or OpenEXR.
    pub frame_format: lapsewerk_video::frames::FrameFormat,
    /// Keep the frames a stopped sequence render left and go on after them.
    pub resume: bool,
    /// In-between frames along the motion (optical flow) after every frame: 0 none, 1 doubles the
    /// frames (smoother, or twice as slow), up to 3.
    pub tween: usize,
    /// A camera move (crop and zoom changing over the sequence), in place of `position`.
    pub framing: Option<lapsewerk_core::framing::Move>,
    /// Deflicker smoothing in frames (larger: steadier, follows quick real changes less);
    /// `None`: no deflicker, only the exposure steps compensated.
    pub deflicker: Option<f64>,
    pub passes: u32,
    /// Regional deflicker grid.
    pub regional: Option<lapsewerk_core::regional::Grid>,
    /// Fraction of the scene's change of light the video keeps (day to night), `None`: as compensated.
    pub keep_light: Option<f64>,
    /// Keep black black where a frame is shown brighter than it was shot (day to night, the
    /// camera's steps undone): the gain's lift of the darkest parts is taken back.
    pub hold_blacks: bool,
    /// The video's brightness shaped by hand: (frame, stops) points, smooth in between, added
    /// last (after deflicker and day to night).
    pub brightness: Vec<(usize, f64)>,
    /// A dark sequence (a night as shot) lifted to a readable brightness, when no brightness is
    /// drawn by hand: see [`AUTO_FLOOR`].
    pub auto_brightness: bool,
    /// The base look ([`BASE_LOOK`]) on every frame: a sequence whose keyframes nobody edited.
    pub base_look: bool,
    /// Stabilize (render 4 % larger and move the crop against the shake).
    pub stabilize: bool,
    /// Find and remove sensor dust.
    pub dust: bool,
    /// Frames blended into each frame (motion blur); 1: none.
    pub blend: usize,
    /// Temporal noise reduction.
    pub denoise: Option<lapsewerk_core::temporal::DenoiseParams>,
    /// Night sky: light pollution, star reduction, star trails.
    pub astro: stage::AstroParams,
    /// Odd frames to replace, gaps to fill.
    pub repair: stage::RepairParams,
    /// Command line: replace the odd frames the analysis finds (fills `repair`).
    pub replace_outliers: bool,
    /// Command line: fill the gaps the analysis finds (fills `repair`).
    pub fill_gaps: bool,
    /// Measure the frames once more with the final corrections, to report the flicker left.
    pub verify: bool,
    pub jobs: usize,
    pub overwrite: bool,
}

/// File types that are developed from the sensor data; the others are already developed pictures.
pub const RAW_EXTENSIONS: &[&str] = &[
    "arw", "cr2", "cr3", "nef", "nrw", "raf", "orf", "rw2", "pef", "dng", "srw", "3fr", "iiq",
];

/// The frames of a sequence in `folder`, by file name.
///
/// Hidden files are left out (`._DSC0001.ARW`: macOS keeps a file's extra data so on SD cards and
/// network shares; not a photo). A camera set to RAW+JPEG writes two files a frame: the developed
/// picture beside its raw (same name, another extension) is left out, so every frame is there once
/// and is developed from the raw.
///
/// # Errors
/// When the folder cannot be read or holds no photos.
pub fn frame_files(folder: &Path) -> Result<Frames, String> {
    let entries = std::fs::read_dir(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    let (files, beside_raws) = pick_frames(found);
    if files.is_empty() {
        return Err(format!("{}: no photos", folder.display()));
    }
    Ok(Frames { files, beside_raws })
}

/// The frames among `found` (see [`frame_files`]), sorted, and how many developed pictures beside
/// raws were left out.
pub fn pick_frames(found: Vec<PathBuf>) -> (Vec<PathBuf>, usize) {
    let ext = |p: &Path| {
        p.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
    };
    let stem = |p: &Path| p.file_stem().map(|s| s.to_string_lossy().to_lowercase());
    let mut photos: Vec<PathBuf> = found
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| !n.to_string_lossy().starts_with('.'))
        })
        .filter(|p| ext(p).is_some_and(|e| EXTENSIONS.contains(&e.as_str())))
        .collect();
    let raw_stems: std::collections::BTreeSet<String> = photos
        .iter()
        .filter(|p| ext(p).is_some_and(|e| RAW_EXTENSIONS.contains(&e.as_str())))
        .filter_map(|p| stem(p))
        .collect();
    let before = photos.len();
    photos.retain(|p| {
        ext(p).is_some_and(|e| RAW_EXTENSIONS.contains(&e.as_str()))
            || stem(p).is_none_or(|s| !raw_stems.contains(&s))
    });
    let beside_raws = before - photos.len();
    photos.sort();
    (photos, beside_raws)
}

/// How a sequence is opened.
#[derive(Clone, Copy, Debug)]
pub struct OpenSettings {
    /// Frame whose camera look every frame is developed with; `None`: each frame's own.
    pub look: Option<usize>,
    /// Find and repair hot pixels.
    pub hot_pixels: bool,
}

impl Default for OpenSettings {
    fn default() -> Self {
        Self {
            look: Some(0),
            hot_pixels: true,
        }
    }
}

/// Opens `files` as a sequence: one look for every frame, hot pixels repaired, and the plan of every
/// frame's settings from the keyframes.
///
/// # Errors
/// When the files cannot be opened or the keyframes not interpolated.
pub fn open(
    files: &[PathBuf],
    opts: &OpenSettings,
    rep: &dyn Reporter,
) -> Result<(Sequence, DevelopPlan), String> {
    let mut seq = Sequence::open(files).map_err(|e| e.to_string())?;
    if !seq.skipped().is_empty() {
        let names: Vec<String> = seq
            .skipped()
            .iter()
            .take(10)
            .map(|p| {
                p.file_name()
                    .map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
            })
            .collect();
        // Inside the series a gap to fill; at its ends the video is just shorter.
        let inside: usize = seq.skipped_gaps().iter().map(|(_, m)| m).sum();
        let edge = seq.skipped().len().saturating_sub(inside);
        let mut what = Vec::new();
        if inside > 0 {
            what.push(format!(
                "{inside} inside the series: --fill-gaps fills their places"
            ));
        }
        if edge > 0 {
            what.push(format!(
                "{edge} at its start or end: the video is {edge} frame{} shorter",
                if edge == 1 { "" } else { "s" }
            ));
        }
        rep.note(&format!(
            "skipped:       {} unreadable file(s), left out (damaged or cut short): {}{}; {}",
            seq.skipped().len(),
            names.join(", "),
            if seq.skipped().len() > 10 { ", …" } else { "" },
            what.join("; ")
        ));
    }
    if let Some(frame) = opts.look {
        let chosen = seq.use_one_look(frame).map_err(|e| e.to_string())?;
        rep.note(&format!(
            "look:          {}",
            match chosen {
                OneLook::Fitted(i) => format!("the camera look of frame {} (from 1), for every frame", i + 1),
                OneLook::Unfitted => "one for every frame: the raws' colour, no camera look fitted".into(),
            }
        ));
    } else {
        rep.note("look:          each frame's own");
    }
    if opts.hot_pixels && !seq.one_size() {
        rep.note(
            "hot pixels:    not repaired: the frames are not all the same size (a crop mode or another camera)",
        );
    } else if opts.hot_pixels {
        let t = Instant::now();
        let picks = hot_pixel_frames(seq.frames(), 12);
        let paths: Vec<&Path> = picks
            .iter()
            .filter_map(|&i| seq.frames().get(i).map(|f| f.path.as_path()))
            .collect();
        let (defects, checked) = lapsewerk_develop::hot_pixels(&paths, HOT_PIXEL_EXCESS);
        rep.note(&format!(
            "hot pixels:    {} found in {checked} raw frames checked, repaired in every frame ({:.1} s)",
            defects.len(),
            t.elapsed().as_secs_f64()
        ));
        seq.repair_samples(defects);
    }
    let plan = seq.plan(Interpolation::Smooth).map_err(|e| e.to_string())?;
    Ok((seq, plan))
}

/// How frames are measured.
#[derive(Clone, Copy, Debug)]
pub struct MeasureSettings {
    /// Long edge of the measuring renders (pixels).
    pub edge: usize,
    /// Frames measured at a time.
    pub jobs: usize,
    /// Measure brightness only in this area (`--reference`).
    pub reference: Option<Rect>,
    /// Keep thumbs to measure camera shake.
    pub stabilize: bool,
}

impl MeasureSettings {
    #[must_use]
    pub fn pass(&self) -> PassConfig {
        PassConfig {
            edge: self.edge,
            jobs: self.jobs,
        }
    }
}

impl Default for MeasureSettings {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
        Self {
            edge: 768,
            // A measuring job holds one decoded raw at a time; four keep 16 GB machines comfortable.
            jobs: (cores / 2).clamp(1, 4),
            reference: None,
            stabilize: false,
        }
    }
}

/// The sequence measured as shot, the camera's exposure steps checked against the sensor, the
/// flicker that is left.
pub struct Analysis {
    pub exif_ramp: Vec<f64>,
    /// Compensation of the camera's steps that really happened, per frame (stops).
    pub ramp: Vec<f64>,
    pub checks: Vec<StepCheck>,
    pub as_shot: Pass,
    /// As shot plus `ramp`: the brightness the sequence has once the camera's steps are undone.
    pub compensated: Vec<Option<f64>>,
    /// Deflicker corrections on top of `ramp`.
    pub corrections: Vec<f64>,
    /// Stabilization: per-frame correction in thumb pixels, when asked for.
    pub motion: Option<Vec<(f64, f64)>>,
    /// The scene's brightness at constant exposure, smoothed (stops).
    pub scene: Vec<f64>,
    /// The video's brightness trend after ramp compensation, smoothed (stops).
    pub video_trend: Vec<f64>,
    /// How much brighter the keyframes' own settings make each frame (stops, against the first
    /// keyframe's settings; between keyframes interpolated): an exposure, a mask or a curve set
    /// on purpose. Taken out before deflickering, so the deflicker never undoes it.
    pub edits: Vec<f64>,
}

/// Measures every frame as shot and works out the camera's exposure steps, the flicker and the
/// scene's change of light.
///
/// # Errors
/// When a render job cannot be made, or [`CANCELLED`].
pub fn analyse(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    opts: &MeasureSettings,
    rep: &dyn Reporter,
) -> Result<Analysis, String> {
    analyse_reusing(seq, plan, opts, rep, None)
}

/// Each frame's measuring offset (stops): the camera's steps evened out locally, less the room a
/// bright keyframe look needs. See [`analyse`].
pub(crate) fn measuring_offsets(seq: &Sequence, plan: &DevelopPlan) -> Vec<f64> {
    let n = seq.len();
    let exposures: Vec<_> = seq.frames().iter().map(|f| f.exposure).collect();
    let exif_ramp =
        exposure::ramp_compensation(&exposures, 0, StepGrid::Thirds).unwrap_or_else(|| vec![0.0; n]);
    let brightest = plan
        .keyframes()
        .iter()
        .map(|&k| plan.settings(k, 0.0).light.exposure)
        .filter(|e| e.is_finite())
        .fold(0.0_f64, f64::max);
    let headroom = measure_headroom(brightest);
    local_steps(&exif_ramp).iter().map(|o| o - headroom).collect()
}

/// [`measuring_offsets`] without the headroom all frames share: what tells one frame's measuring
/// from another's. Measurements are kept with their offset taken off, so a brighter keyframe that
/// only adds headroom for all leaves the others' measurements as they are.
pub(crate) fn measuring_steps(seq: &Sequence) -> Vec<f64> {
    let n = seq.len();
    let exposures: Vec<_> = seq.frames().iter().map(|f| f.exposure).collect();
    let exif_ramp =
        exposure::ramp_compensation(&exposures, 0, StepGrid::Thirds).unwrap_or_else(|| vec![0.0; n]);
    local_steps(&exif_ramp)
}

/// [`analyse`], keeping what `kept` (an earlier analysis's frames as shot) measured of the frames
/// not marked to be measured again.
pub(crate) fn analyse_reusing(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    opts: &MeasureSettings,
    rep: &dyn Reporter,
    kept: Option<(&measure::Pass, &[bool])>,
) -> Result<Analysis, String> {
    let n = seq.len();
    let exposures: Vec<_> = seq.frames().iter().map(|f| f.exposure).collect();
    let exif_ramp =
        exposure::ramp_compensation(&exposures, 0, StepGrid::Thirds).unwrap_or_else(|| vec![0.0; n]);
    let steps = exposure::exposure_steps(&exif_ramp, 0.1);
    if exposures.iter().all(Option::is_none) {
        rep.note("exposure steps: no exposure data in the photos (EXIF): looked for in the pictures");
    } else {
        rep.note(&format!("exposure steps: {} (EXIF)", steps.len()));
    }

    // Measure every frame as shot (keyframes, no offset): the camera exposed each frame usable,
    // whereas adding a ramp of many stops would clip whole frames. The ramp compensation is then
    // added in stops. The raw sensor brightness decides which EXIF steps really happened.
    let extras = measure::Extras {
        raw: true,
        thumbs: opts.stabilize,
        reference: opts.reference,
        // Without exposure data: tiles tell the camera's steps from changes of the light.
        grid: if exposures.iter().all(Option::is_none) {
            lapsewerk_core::regional::Grid::new(4, 3)
        } else {
            None
        },
        ..Default::default()
    };
    // The camera's steps, evened out locally: each frame is measured about as bright as its
    // neighbours (a few stops of the ramp would clip whole frames; a frame just past a step of
    // two stops, measured as shot under a bright look, burnt out and read darker than it is, and
    // the deflicker brightened it by a stop and more). What was added is taken off again.
    // And a stop and a half darker than shot: a bright or cool keyframe look burns out less while
    // measuring (a burnt-out frame no longer moves with its exposure, and taking the added
    // exposure off again then went wrong by a quarter stop).
    // As much room as the brightest keyframe look needs (a stop over its added exposure), at least
    // 1.5 stops: more than needed costs precision in the darkest frames. Up to 8 stops: a night
    // pushed by +6 in another raw editor still measures without a wave (with a cap of 4 it pumped by a stop).
    let local = measuring_offsets(seq, plan);
    // Kept frames as they were measured (their offset back on; it is the same, or they would be
    // measured again).
    let kept_raw = kept.map(|(k, again)| {
        let mut k = k.clone();
        for (s, o) in k.stops.iter_mut().zip(&local) {
            *s = s.map(|v| v + o);
        }
        for (s, o) in k.perceived.iter_mut().zip(&local) {
            *s = s.map(|v| v + o);
        }
        for (tiles, o) in k.tiles.iter_mut().zip(&local) {
            for t in tiles.iter_mut() {
                *t = t.map(|v| v + o);
            }
        }
        (k, again)
    });
    let extras = measure::Extras {
        kept: kept_raw.as_ref().map(|(k, again)| (k, *again)),
        ..extras
    };
    let mut as_shot = measure::pass(seq, plan, &local, opts.pass(), extras, Step::Measure, rep)?;
    for (s, o) in as_shot.stops.iter_mut().zip(&local) {
        *s = s.map(|v| v - o);
    }
    for (s, o) in as_shot.perceived.iter_mut().zip(&local) {
        *s = s.map(|v| v - o);
    }
    for (tiles, o) in as_shot.tiles.iter_mut().zip(&local) {
        for t in tiles.iter_mut() {
            *t = t.map(|v| v - o);
        }
    }
    let motion = if extras.thumbs {
        let thumbs = std::mem::take(&mut as_shot.thumbs);
        Some(stabilization(&thumbs, rep))
    } else {
        None
    };
    let (mut ramp, mut checks) = exposure::verified_compensation(n, &steps, &as_shot.raw, 10, 0.1);
    let edits = measure::keyframe_edits(seq, plan, &as_shot.stops, &local, opts.pass(), opts.reference)?;
    if edits.iter().any(|e| e.abs() > 0.05) {
        let (lo, hi) = edits
            .iter()
            .fold((f64::MAX, f64::MIN), |(lo, hi), e| (lo.min(*e), hi.max(*e)));
        rep.note(&format!(
            "keyframes:     their settings change the brightness by {lo:+.2} to {hi:+.2} stops; kept (not deflickered)"
        ));
    }
    // What the light did, without what the keyframes do on purpose.
    let unedited: Vec<Option<f64>> = as_shot
        .stops
        .iter()
        .zip(&edits)
        .map(|(m, e)| m.map(|m| m - e))
        .collect();
    // No exposure data at all (exported JPEGs): the camera's larger steps (an aperture or ISO
    // change of half a stop and more) are found in the pictures and undone like EXIF steps; the
    // deflicker takes care of smaller ones.
    let mut scene_ramp = exif_ramp.clone();
    if exposures.iter().all(Option::is_none) {
        let seen = exposure::visible_steps(&unedited, &as_shot.tiles, 0.5);
        if !seen.is_empty() {
            let c = exposure::compensation_of(n, &seen);
            for ((r, s), c) in ramp.iter_mut().zip(scene_ramp.iter_mut()).zip(&c) {
                *r += c;
                *s += c;
            }
            checks.extend(seen.iter().map(|s| exposure::StepCheck {
                frame: s.frame,
                reported: 0.0,
                measured: Some(s.stops),
                used: s.stops,
            }));
            rep.note(&format!(
                "exposure steps: {} seen in the pictures (no EXIF data), at frame(s) {:?} (from 1): {}",
                seen.len(),
                seen.iter().map(|s| s.frame + 1).collect::<Vec<_>>(),
                seen.iter()
                    .map(|s| format!("{:+.2}", s.stops))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    // The scene's own change of light (as if exposed the same throughout).
    let scene = lapsewerk_core::daynight::scene_brightness(&unedited, &scene_ramp);
    // The camera's steps are undone towards the frame where the scene is brightest (the day): a
    // sunset's first frame, a sunrise's last. Undone towards the first frame, a sunrise's day came
    // out the sum of all its steps brighter than shot: white.
    if let Some(anchor) = scene
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_finite())
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(i, _)| i)
        && let Some(&shift) = ramp.get(anchor)
        && shift != 0.0
    {
        for r in &mut ramp {
            *r -= shift;
        }
    }
    let compensated: Vec<Option<f64>> = unedited
        .iter()
        .zip(&ramp)
        .map(|(m, r)| m.map(|m| m + r))
        .collect();
    let corrections = deflicker::corrections(&compensated, &DeflickerParams::default());
    // The video's trend.
    let compensated_values: Vec<f64> = compensated.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
    let video_trend = lapsewerk_core::smooth::robust_local_linear(
        &compensated_values,
        lapsewerk_core::daynight::SCENE_SMOOTHING,
        2,
    );
    Ok(Analysis {
        scene,
        video_trend,
        exif_ramp,
        ramp,
        checks,
        as_shot,
        compensated,
        corrections,
        motion,
        edits,
    })
}

/// Deflicker parameters for a smoothing of `smoothing` frames (1..60; `None`: the default).
#[must_use]
pub fn deflicker_params(smoothing: Option<f64>) -> DeflickerParams {
    let d = DeflickerParams::default();
    DeflickerParams {
        smoothing_frames: smoothing
            .filter(|s| s.is_finite())
            .map_or(d.smoothing_frames, |s| s.clamp(1.0, 60.0)),
        ..d
    }
}

/// Smoothing of the camera path for stabilization, in frames (Gaussian sigma): shake faster than
/// this goes, slower motion (a slider, a pan, a slowly settling tripod) stays.
const STABILIZE_FRAMES: f64 = 12.0;

/// Shifts between consecutive thumbs (on all cores), and the per-frame corrections they give.
fn stabilization(thumbs: &[Option<lapsewerk_core::stabilize::Thumb>], rep: &dyn Reporter) -> Vec<(f64, f64)> {
    use lapsewerk_core::stabilize;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let t = Instant::now();
    let pairs = thumbs.len().saturating_sub(1);
    let steps: Vec<Mutex<Option<(f64, f64)>>> = (0..pairs).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= pairs {
                        break;
                    }
                    let (Some(Some(a)), Some(Some(b))) = (thumbs.get(i), thumbs.get(i + 1)) else {
                        continue;
                    };
                    // A weak peak: the frames do not agree on any motion (a flash, fog): count none.
                    // A step counts when the match is clear and the move plausible for shake on a
                    // tripod (under 2 % of the picture from one frame to the next): a structureless
                    // sky, a flash or something big moving through otherwise reads as tens of pixels.
                    let most = stabilize::SIZE as f64 * 0.02;
                    let step = stabilize::shift(a, b)
                        .filter(|s| s.2 >= 0.1 && s.0.hypot(s.1) <= most)
                        .map(|s| (s.0, s.1));
                    if let (Some(slot), Some(step)) = (steps.get(i), step)
                        && let Ok(mut slot) = slot.lock()
                    {
                        *slot = Some(step);
                    }
                }
            });
        }
    });
    let steps: Vec<Option<(f64, f64)>> = steps.into_iter().map(|m| m.into_inner().ok().flatten()).collect();
    let c = stabilize::corrections(&steps, STABILIZE_FRAMES);
    let rms = (c.iter().map(|(x, y)| x * x + y * y).sum::<f64>() / c.len().max(1) as f64).sqrt();
    let largest = c.iter().map(|(x, y)| x.hypot(*y)).fold(0.0, f64::max);
    // The frame is rendered 4 % larger: 2 % each side is all the crop can move. More than that is
    // seldom shake (a flash, something big moving through, a bumped tripod): held to the room and
    // said so.
    let room = stabilize::SIZE as f64 * 0.02;
    let held = c.iter().filter(|(x, y)| x.abs() > room || y.abs() > room).count();
    let c: Vec<(f64, f64)> = c
        .into_iter()
        .map(|(x, y)| (x.clamp(-room, room), y.clamp(-room, room)))
        .collect();
    if held > 0 {
        rep.note(&format!(
            "warning:       {held} frame(s) would need a larger correction than the 4 % margin allows (a flash, something big moving through, a bumped tripod?): held to the margin"
        ));
    }
    rep.note(&format!(
        "stabilize:     shake {rms:.2} px RMS, largest correction {largest:.2} px (of {} measured); {} of {pairs} steps measured ({:.1} s)",
        stabilize::SIZE,
        steps.iter().flatten().count(),
        t.elapsed().as_secs_f64()
    ));
    c
}

impl Analysis {
    /// Exposure offsets of the camera's steps undone and the flicker taken out (before the check
    /// pass and day-to-night).
    #[must_use]
    pub fn offsets(&self) -> Vec<f64> {
        self.ramp
            .iter()
            .zip(&self.corrections)
            .map(|(r, c)| r + c)
            .collect()
    }

    /// [`Self::offsets`] with the deflicker smoothing `smoothing` (frames), or none.
    #[must_use]
    pub fn offsets_with(&self, smoothing: Option<f64>) -> Vec<f64> {
        match smoothing {
            None => self.ramp.clone(),
            Some(s) if (s - DeflickerParams::default().smoothing_frames).abs() < 1e-9 => self.offsets(),
            Some(s) => {
                let c = deflicker::corrections(&self.compensated, &deflicker_params(Some(s)));
                self.ramp.iter().zip(&c).map(|(r, c)| r + c).collect()
            }
        }
    }

    /// The offsets the video will have before the check pass: [`Self::offsets_with`] plus the
    /// day-to-night brightness of `keep` (as [`correct`] adds it). What the curves show until the
    /// corrections are worked out.
    #[must_use]
    pub fn planned_offsets(
        &self,
        smoothing: Option<f64>,
        keep: Option<f64>,
        shape: &[(usize, f64)],
    ) -> Vec<f64> {
        let mut o = self.offsets_with(smoothing);
        for (o, s) in o
            .iter_mut()
            .zip(lapsewerk_core::daynight::shaped(shape, self.ramp.len()))
        {
            *o += s;
        }
        if let Some(keep) = keep {
            let base = self.offsets_with(smoothing);
            let trend = self.trend_after(&base, smoothing);
            let (extra, _) = lapsewerk_core::daynight::brightness_offsets(&trend, &self.scene, keep);
            for (o, e) in o.iter_mut().zip(&extra) {
                *o += e;
            }
        }
        o
    }

    /// The video's brightness trend once `offsets` (ramp and deflicker) are applied, followed as
    /// closely as the deflicker follows it (a robust fit over a few frames, so a flash or a
    /// failed measurement does not count): what the day-to-night brightness works against. The
    /// wider [`Self::video_trend`] rounds a steep ramp's knees, and the difference came out as a
    /// dip and a bump of a quarter stop at each knee. Without a deflicker: [`Self::video_trend`].
    #[must_use]
    pub fn trend_after(&self, offsets: &[f64], smoothing: Option<f64>) -> Vec<f64> {
        let Some(s) = smoothing else {
            return self.video_trend.clone();
        };
        let shown: Vec<f64> = self
            .as_shot
            .stops
            .iter()
            .zip(&self.edits)
            .zip(offsets)
            .map(|((m, e), o)| m.map_or(f64::NAN, |m| m - e + o))
            .collect();
        // Narrow (a quarter of the smoothing, at least 2 frames): a steep ramp's knee bends faster
        // than a wide local line follows (it cut the corner by a fifth of a stop, which came out as
        // a bump before the drop); what is left after the deflicker is followed too, so the video
        // keeps to the day-to-night share more closely. The bisquare still drops a flash.
        let fit = lapsewerk_core::smooth::robust_local_linear(&shown, (s / 4.0).max(2.0), 2);
        fit.iter()
            .zip(&self.video_trend)
            .map(|(f, v)| if f.is_finite() { *f } else { *v })
            .collect()
    }

    /// The scene's and the video's change of light, in stops.
    #[must_use]
    pub fn light_ranges(&self) -> (f64, f64) {
        (range(&self.scene), range(&self.video_trend))
    }

    /// Whether this is a day-to-night (or night-to-day) series, where keeping a share of the
    /// change of light is recommended: the light changes by more than 1.5 stops and the camera
    /// followed it by 3 stops or more, or it changes by more than 4 stops. A moonrise (over a
    /// fixed camera, or a bramper's few steps) is no such series: its change of light is the
    /// picture.
    #[must_use]
    pub fn day_to_night(&self) -> bool {
        let scene = range(&self.scene);
        scene > 4.0 || (scene > 1.5 && range(&self.ramp) >= 3.0)
    }

    /// Frames worth editing as keyframes: more where the scene's light changes.
    #[must_use]
    pub fn suggested_keyframes(&self) -> Vec<usize> {
        let count = ((range(&self.scene) / 1.5).ceil() as usize + 2).clamp(2, 8);
        lapsewerk_core::daynight::suggest_keyframes(&self.scene, count)
    }
}

/// The span of the finite values (max − min).
#[must_use]
pub fn range(v: &[f64]) -> f64 {
    let finite = v.iter().copied().filter(|x| x.is_finite());
    finite.clone().fold(f64::MIN, f64::max) - finite.fold(f64::MAX, f64::min)
}

/// Everything worked out for the movie.
pub struct Corrected {
    /// Exposure offsets per frame (stops): steps, flicker, check pass, day to night.
    pub offsets: Vec<f64>,
    /// How much brighter each frame is shown than shot, smoothed over ±12 frames (stops): the
    /// camera's exposure steps make `offsets` a staircase, and the blacks held against it would
    /// jump at every step. The shape drawn by hand is not in it: a brightening asked for is left
    /// alone. What holding the blacks works with.
    pub brightening: Vec<f64>,
    /// The check pass and the corrections it added.
    pub second: Option<(Pass, Vec<f64>)>,
    /// Regional corrections per frame and tile.
    pub regional: Option<Vec<Vec<f64>>>,
    /// Brightness of the finished corrections (`verify`).
    pub verified: Option<Vec<Option<f64>>>,
    /// The same as seen (perceived brightness, what `verify` reports): what the app says.
    pub verified_seen: Option<Vec<Option<f64>>>,
    /// Sensor dust to remove.
    pub dust: Option<lapsewerk_core::dust::DustMap>,
    /// The size the frames render at (aspect ratio) and the video settings, its orientation fitted.
    pub size: (usize, usize),
    pub render: RenderArgs,
}

/// Frames the hand-drawn shape's effect is measured at (at most).
const SHAPE_SAMPLES: usize = 12;

/// The least the hand-drawn shape is taken to act by: where it acts by less, the highlights are at
/// white, and a larger change would burn them out further rather than show.
const SHAPE_LEAST_GAIN: f64 = 0.7;

/// How strongly the hand-drawn `shape` acts on the finished look, frame by frame (1: fully):
/// measured at up to [`SHAPE_SAMPLES`] frames where it moves the brightness by more than 0.05
/// stops, each rendered as the export renders it, with and without it (light averaged linearly,
/// highlights at white included), interpolated between them and held beyond; at least [`SHAPE_LEAST_GAIN`]
/// (a measurement outside 0.2..1.5 is no tone curve at work and left out). `None` when nowhere to
/// measure, with how many frames it was measured at otherwise.
///
/// # Errors
/// When a render job cannot be made.
fn shape_gains(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    base: &[f64],
    shape: &[f64],
    cfg: measure::PassConfig,
    rep: &dyn Reporter,
) -> Result<Option<(Vec<f64>, usize)>, String> {
    let n = base.len().min(shape.len());
    let acting: Vec<usize> = (0..n)
        .filter(|&i| shape.get(i).is_some_and(|s| s.abs() > 0.05))
        .collect();
    if acting.is_empty() {
        return Ok(None);
    }
    let step = acting.len().div_ceil(SHAPE_SAMPLES).max(1);
    let picked: Vec<usize> = acting.iter().copied().step_by(step).collect();
    // Each picked frame as the export renders it (16-bit display output), with and without the
    // shape: its brightness as the delivery has it, light averaged linearly with white as the most.
    let shown = |seq: &mut Sequence, i: usize, offset: f64| shown_stops(seq, plan, i, offset, cfg, rep);
    let mut points: Vec<(usize, f64)> = Vec::with_capacity(picked.len());
    for (k, &i) in picked.iter().enumerate() {
        rep.progress(Step::Verify, k, picked.len());
        let (Some(&b), Some(&s)) = (base.get(i), shape.get(i)) else {
            continue;
        };
        let (Some(plain), Some(with)) = (shown(seq, i, b)?, shown(seq, i, b + s)?) else {
            continue;
        };
        let g = (with - plain) / s;
        // Below 0.7 the highlights are at white: more would only burn them out further.
        if g.is_finite() && (0.2..=1.5).contains(&g) {
            points.push((i, g.max(SHAPE_LEAST_GAIN)));
        }
    }
    rep.progress(Step::Verify, picked.len(), picked.len());
    Ok(gains_between(&points, n).map(|g| (g, points.len())))
}

/// The look a sequence nobody edited gets (`--base-look`), on top of its keyframes: LightCraft's
/// Auto recipe (calibrated on the Auto of common raw editors) with the shadows opened further and less contrast
/// and vibrance, fitted to the author's film from his earlier time-lapse workflow of a Hamburg day to night (untouched Lapsewerk:
/// ground 1.9 stops darker; with this and the day lift 0.2). Exposure is not part of it:
/// `--auto-brightness` sets how bright.
pub const BASE_LOOK: [(&str, f64); 4] = [
    ("light.contrast", 40.0),
    ("light.highlights", -66.0),
    ("light.shadows", 80.0),
    ("color.vibrance", 15.0),
];

/// `plan` with the base look when `on`.
#[must_use]
pub fn with_base_look(plan: &DevelopPlan, on: bool) -> std::borrow::Cow<'_, DevelopPlan> {
    if on {
        std::borrow::Cow::Owned(plan.clone().with_look(&BASE_LOOK))
    } else {
        std::borrow::Cow::Borrowed(plan)
    }
}

/// Turns the video upright for upright frames when its size follows the photos (`auto_size`, the
/// app's presets, `--fit-orientation`): a size given upright already stays as it is.
pub fn upright(r: &mut RenderArgs, frames: (usize, usize)) {
    if r.auto_size && frames.1 > frames.0 && r.video.width > r.video.height {
        std::mem::swap(&mut r.video.width, &mut r.video.height);
    }
}

/// Says so when upright frames become a landscape video: how much of their height is left, and how
/// to keep them whole (a landscape photo in an upright video is chosen so: the app's upright preset).
fn note_orientation(r: &RenderArgs, frames: (usize, usize), rep: &dyn Reporter) {
    let (fw, fh) = (frames.0 as f64, frames.1.max(1) as f64);
    let (vw, vh) = (r.video.width as f64, r.video.height.max(1) as f64);
    if r.framing.is_none() && fh > fw && vw > vh {
        rep.note(&format!(
            "orientation:   the photos are upright, the video is not: it shows {:.0} % of their height (--fit-orientation turns the video upright)",
            ((fw / fh) / (vw / vh)).min(1.0) * 100.0
        ));
    }
}

/// `plan` as a video of `video` (width, height) shows it: with the base look when `look`, and a
/// cropped sequence framed as established time-lapse tools frame it (every frame's crop cut to the video's shape
/// about its centre, [`DevelopPlan::with_crop_fit`]).
#[must_use]
pub fn framed_plan(seq: &Sequence, plan: &DevelopPlan, look: bool, video: (usize, usize)) -> DevelopPlan {
    let looked = with_base_look(plan, look).into_owned();
    match seq.source_size(0) {
        Some(photo) if video.0 > 0 && video.1 > 0 => {
            looked.with_crop_fit(video.0 as f64 / video.1 as f64, photo)
        }
        _ => looked,
    }
}

/// The size of frame `frame` as `plan` develops it, before any video framing: the photo turned
/// as shot and cut to its crop. Known before the analysis (from the catalog), so that the
/// previews of an upright cropped series are framed upright from the start.
#[must_use]
pub fn developed_size(seq: &Sequence, plan: &DevelopPlan, frame: usize) -> Option<(usize, usize)> {
    let (pw, ph) = seq.source_size(frame)?;
    let d = plan.settings(frame, 0.0);
    let (w, h) = if d.orientation.swaps_axes() {
        (ph, pw)
    } else {
        (pw, ph)
    };
    let r = d.crop.geometry.rect_px(w as f64, h as f64);
    let (rw, rh) = (r.width().round(), r.height().round());
    (rw >= 1.0 && rh >= 1.0).then_some((rw as usize, rh as usize))
}

/// The size frames of `measured` (as the analysis rendered them) have with `plan`'s crop: the
/// same width, the height of the video's shape when [`framed_plan`] cut them to it.
fn fitted_size(seq: &Sequence, plan: &DevelopPlan, measured: (usize, usize)) -> (usize, usize) {
    let Some(&first) = plan.keyframes().first() else {
        return measured;
    };
    let (Some((pw, ph)), Ok(own)) = (seq.source_size(first), seq.settings(first)) else {
        return measured;
    };
    let d = plan.settings(first, 0.0);
    let turned = |s: &lapsewerk_develop::DevelopSettings| {
        let (w, h) = if s.orientation.swaps_axes() {
            (ph, pw)
        } else {
            (pw, ph)
        };
        let r = s.crop.geometry.rect_px(w as f64, h as f64);
        r.width() / r.height()
    };
    // The analysis rendered the keyframe's own crop: only when the plan's is another shape.
    let (was, now) = (turned(&own), turned(&d));
    if !(was.is_finite() && now.is_finite() && now > 0.0) || (was / now - 1.0).abs() < 1e-3 {
        return measured;
    }
    (measured.0, ((measured.0 as f64 / now).round() as usize).max(1))
}

/// The brightness `--auto-brightness` lifts a dark sequence to (log2 of the linear mean of the
/// finished frames, white = 0). A starry night is mostly sky, so this is the sky's background: at
/// -6.2 about 30 of 255, dark as a night sky is (an astrophotographer's 25-35; -4.75, from a night
/// the author edited in another raw editor, showed it grey at 52). The same night as shot: -8.1 (11 of 255; the
/// camera's own JPEG 7). A sequence at least this bright is left alone; nothing is made darker.
pub const AUTO_FLOOR: f64 = -6.2;

/// Where a sequence's brightest frames start to count as day for the day lift ([`DAY_FROM`]).
const DAY_FADE_FROM: f64 = -4.75;

/// The most `--auto-brightness` lifts (stops).
const AUTO_MOST: f64 = 4.0;

/// Frames `--auto-brightness` measures, spread over the sequence.
const AUTO_SAMPLES: usize = 7;

/// How far to lift a dark sequence (stops) and the median it measured, when its median frame
/// (rendered as the export renders it, at `base` offsets) is darker than [`AUTO_FLOOR`];
/// `None` when it is bright enough.
///
/// # Errors
/// When a render job cannot be made.
fn auto_lift(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    base: &[f64],
    cfg: measure::PassConfig,
    rep: &dyn Reporter,
) -> Result<Option<(f64, f64)>, String> {
    let n = base.len();
    if n == 0 {
        return Ok(None);
    }
    let mut seen = Vec::with_capacity(AUTO_SAMPLES);
    for k in 0..AUTO_SAMPLES {
        if rep.cancelled() {
            return Err(CANCELLED.into());
        }
        let i = (k * (n - 1)) / (AUTO_SAMPLES - 1).max(1);
        let offset = base.get(i).copied().unwrap_or(0.0);
        let stops = shown_stops(seq, plan, i, offset, cfg, rep)?;
        seen.extend(stops);
    }
    if seen.is_empty() {
        return Ok(None);
    }
    seen.sort_by(f64::total_cmp);
    let Some(&median) = seen.get(seen.len() / 2) else {
        return Ok(None);
    };
    let top = seen.last().copied().unwrap_or(median);
    let night = (AUTO_FLOOR - median).min(AUTO_MOST);
    // A day under-shown: its brightest frames brought to DAY_TARGET, fully once they are clearly
    // day (DAY_FROM), not at all at night's level, so a night is never made a day.
    let day_share = ((top - DAY_FADE_FROM) / (DAY_FROM - DAY_FADE_FROM)).clamp(0.0, 1.0);
    let day = (DAY_TARGET - top).clamp(0.0, DAY_MOST) * day_share;
    let lift = night.max(day);
    Ok((lift > 0.1).then_some((lift, median)))
}

/// The brightness `--auto-brightness` brings a sequence's brightest frames to when they show
/// darker (log2 of the linear mean, white = 0): the author's film from his earlier time-lapse workflow of a Hamburg day showed
/// -1.8 (a mean of about 129 of 255), the same frames untouched in Lapsewerk -2.7 (91).
pub const DAY_TARGET: f64 = -1.9;

/// From this brightness up the brightest frames are taken as day and lifted fully to
/// [`DAY_TARGET`]; between [`DAY_FADE_FROM`] and here, partly.
const DAY_FROM: f64 = -3.5;

/// The most a day is lifted (stops).
const DAY_MOST: f64 = 1.5;

/// The brightness of a 16-bit sRGB-encoded frame (3 samples a pixel) in stops: its luminance
/// decoded to linear light and averaged (white, 1, the most there is), as a delivery's frames
/// measure; `None` for an empty or black frame.
fn linear_mean_stops(rgb: &[u16]) -> Option<f64> {
    let decode = |v: u16| {
        let e = f64::from(v) / 65535.0;
        if e <= 0.040_45 {
            e / 12.92
        } else {
            ((e + 0.055) / 1.055).powf(2.4)
        }
    };
    let (px, _) = rgb.as_chunks::<3>();
    let sum: f64 = px
        .iter()
        .map(|[r, g, b]| 0.2126 * decode(*r) + 0.7152 * decode(*g) + 0.0722 * decode(*b))
        .sum();
    let mean = sum / px.len().max(1) as f64;
    (mean > 0.0 && mean.is_finite()).then(|| mean.log2())
}

/// A frame's brightness as the export shows it (16-bit sRGB display output, light averaged
/// linearly, white = 0) when developed at `offset`; `None` when it does not render.
///
/// # Errors
/// When a render job cannot be made, or [`CANCELLED`].
fn shown_stops(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    i: usize,
    offset: f64,
    cfg: measure::PassConfig,
    rep: &dyn Reporter,
) -> Result<Option<f64>, String> {
    if rep.cancelled() {
        return Err(CANCELLED.into());
    }
    let job = seq
        .render_job(
            i,
            plan.settings(i, offset),
            cfg.edge,
            cfg.edge,
            lapsewerk_develop::Output::Display16(lapsewerk_develop::OutputSpace::Srgb),
        )
        .map_err(|e| e.to_string())?;
    Ok(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.run()))
            .ok()
            .and_then(|done| done.rendered.ok())
            .and_then(|r| r.deep)
            .and_then(|d| match d.samples {
                lapsewerk_develop::DeepSamples::U16(data) => linear_mean_stops(&data),
                lapsewerk_develop::DeepSamples::F32(_) => None,
            }),
    )
}

/// Frames the day-to-night share is checked at in the finished look (besides its anchor).
const KEEP_SAMPLES: usize = 9;

/// The least and most a stop of exposure is taken to move the finished look in the keep-light
/// check: outside, the frame is at black or white and more would not show.
const KEEP_GAIN: (f64, f64) = (0.5, 2.0);

/// The most the keep-light check moves a frame (stops).
const KEEP_MOST: f64 = 3.0;

/// What the day-to-night share needs on top so the finished look keeps `keep` of the scene's
/// change, frame by frame: the tone curve's toe pushes dark frames down further than their exposure
/// (Hamburg, 30 % of 8.9 stops: 3.9 stops darker instead of 2.7). Measured at up to
/// [`KEEP_SAMPLES`] frames rendered as the export renders them at `base` (the anchor too), each
/// off target corrected by one secant step, interpolated between and held beyond. With the
/// change meant and as measured before (stops, from the anchor to the sample furthest from it),
/// `None` when nothing could be measured.
///
/// # Errors
/// When a render job cannot be made, or [`CANCELLED`].
#[allow(clippy::too_many_arguments)]
fn keep_light_fix(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    base: &[f64],
    scene: &[f64],
    anchor: usize,
    keep: f64,
    cfg: measure::PassConfig,
    rep: &dyn Reporter,
) -> Result<Option<(Vec<f64>, f64, f64)>, String> {
    let n = base.len().min(scene.len());
    let (Some(&sa), Some(&ba)) = (scene.get(anchor), base.get(anchor)) else {
        return Ok(None);
    };
    if n < 2 || !sa.is_finite() {
        return Ok(None);
    }
    let Some(ya) = shown_stops(seq, plan, anchor, ba, cfg, rep)? else {
        return Ok(None);
    };
    let mut picked: Vec<usize> = (0..KEEP_SAMPLES)
        .map(|k| (k * (n - 1)) / (KEEP_SAMPLES - 1))
        .filter(|&i| i != anchor)
        .collect();
    picked.dedup();
    let mut points = vec![(anchor, 0.0)];
    // The change from the anchor meant and as measured, at the sample furthest from it.
    let (mut meant, mut seen) = (0.0_f64, 0.0_f64);
    for (k, &i) in picked.iter().enumerate() {
        rep.progress(Step::Verify, k, picked.len());
        let (Some(&b), Some(&s)) = (base.get(i), scene.get(i)) else {
            continue;
        };
        if !s.is_finite() {
            continue;
        }
        let Some(y) = shown_stops(seq, plan, i, b, cfg, rep)? else {
            continue;
        };
        let target = keep * (s - sa);
        let off = target - (y - ya);
        let mut fix = 0.0;
        if off.abs() > 0.03
            && let Some(y2) = shown_stops(seq, plan, i, b + off, cfg, rep)?
        {
            let g = (y2 - y) / off;
            if g.is_finite() {
                fix = (off / g.clamp(KEEP_GAIN.0, KEEP_GAIN.1)).clamp(-KEEP_MOST, KEEP_MOST);
            }
        }
        if target.abs() > meant.abs() {
            (meant, seen) = (target, y - ya);
        }
        points.push((i, fix));
    }
    rep.progress(Step::Verify, picked.len(), picked.len());
    points.sort_by_key(|p| p.0);
    Ok(gains_between(&points, n).map(|f| (f, meant, seen)))
}

/// Gains for `n` frames from those measured at `points` (frame, gain; in order): linear between
/// them, held before the first and after the last; `None` without any.
fn gains_between(points: &[(usize, f64)], n: usize) -> Option<Vec<f64>> {
    let (&(first, g0), &(last, g1)) = (points.first()?, points.last()?);
    Some(
        (0..n)
            .map(|i| {
                if i <= first {
                    return g0;
                }
                if i >= last {
                    return g1;
                }
                let k = points.partition_point(|(p, _)| *p <= i);
                match (points.get(k.wrapping_sub(1)), points.get(k)) {
                    (Some(&(a, ga)), Some(&(b, gb))) if b > a => {
                        ga + (gb - ga) * (i - a) as f64 / (b - a) as f64
                    }
                    (Some(&(_, ga)), _) => ga,
                    _ => 1.0,
                }
            })
            .collect(),
    )
}

/// How far either way the brightening that holding the blacks works with is smoothed (frames): a
/// camera step without EXIF moves the offsets by a whole stop at once, and smoothed over ±4 the
/// brightening fell through zero within two frames, the hold switched off and the ground jumped
/// 0.12 stops (HG without EXIF, frames 89-90).
const BRIGHTENING_RADIUS: usize = 12;

/// `values` averaged over `radius` frames either way with weights falling off linearly (fewer at
/// the ends): a step becomes an S over twice the radius, without a kink where it starts or ends.
fn smoothed_triangle(values: &[f64], radius: usize) -> Vec<f64> {
    (0..values.len())
        .map(|i| {
            let (mut sum, mut weights) = (0.0, 0.0);
            for j in i.saturating_sub(radius)..(i + radius + 1).min(values.len()) {
                let w = (radius + 1 - i.abs_diff(j)) as f64;
                if let Some(v) = values.get(j) {
                    sum += w * v;
                    weights += w;
                }
            }
            if weights > 0.0 { sum / weights } else { 0.0 }
        })
        .collect()
}

/// The most the check pass moves a frame (stops): after the first pass, flicker left is a few
/// hundredths of a stop.
const CHECK_MOST: f64 = 0.1;

/// Works out the corrections for the movie: the check pass, regional deflicker, day to night,
/// verification and dust, as `render` asks.
///
/// # Errors
/// When a pass fails, the frames are of different shapes, or [`CANCELLED`].
pub fn correct(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    a: &Analysis,
    opts: &MeasureSettings,
    render: &RenderArgs,
    rep: &dyn Reporter,
) -> Result<Corrected, String> {
    let mut offsets = a.offsets_with(render.deflicker);
    let mut second = None;
    // The check pass measured far more than the first pass left: something in the picture changes
    // on its own (clouds, light moving, bright parts burning out), said with the result.
    let mut scene_changes = false;
    let mut regional = None;
    let mut verified = None;
    let mut verified_seen = None;
    let size = frame_size(&a.as_shot.sizes)?;
    let mut r = render.clone();
    upright(&mut r, size);
    note_orientation(&r, size, rep);
    let looked = framed_plan(seq, plan, r.base_look, (r.video.width, r.video.height));
    let plan: &DevelopPlan = &looked;
    let size = fitted_size(seq, plan, size);
    rep.note(&format!(
        "video:         {}×{}, position {:.2},{:.2}",
        r.video.width, r.video.height, r.position.0, r.position.1
    ));
    note_upscale(seq, &r, rep);
    // How much brighter or darker each frame is shown than the light it had: the day-to-night
    // share and the shape drawn by hand.
    let mut brighten = vec![0.0; offsets.len()];
    // The shape drawn by hand on its own: a brightening asked for in so many words, which holding
    // the blacks leaves alone.
    let mut drawn = vec![0.0; offsets.len()];
    let mut keep_at = None;
    if let Some(keep) = r.keep_light {
        let trend = a.trend_after(&offsets, r.deflicker);
        let (extra, anchor) = lapsewerk_core::daynight::brightness_offsets(&trend, &a.scene, keep);
        for (o, e) in brighten.iter_mut().zip(&extra) {
            *o += e;
        }
        rep.note(&format!(
            "keep light:    {:.0} % of the scene's change ({:.1} stops in the video), as bright as shot at frame {} (from 1)",
            keep * 100.0,
            keep * range(&a.scene),
            anchor + 1
        ));
        keep_at = Some((keep, anchor));
    }
    // A sequence shown too dark (a night as shot, a day under-shown) lifted to a readable
    // brightness, as one point drawn by hand (so the tone curve is made up for like any drawn
    // shape).
    let mut lifted = 0.0;
    if r.auto_brightness && r.brightness.is_empty() {
        let base: Vec<f64> = offsets.iter().zip(&brighten).map(|(o, b)| o + b).collect();
        if let Some((lift, median)) = auto_lift(seq, plan, &base, opts.pass(), rep)? {
            rep.note(&format!(
                "auto bright:   the frames' median brightness {median:+.2} stops (linear mean): lifted by {lift:+.2} stops"
            ));
            r.brightness = vec![(0, lift)];
            lifted = lift;
        } else {
            rep.note("auto bright:   bright enough as shot: left as it is");
        }
    }
    // The day-to-night share checked in the finished look (with the lift, which moves the dark
    // frames out of the tone curve's toe).
    if let Some((keep, anchor)) = keep_at {
        let base: Vec<f64> = offsets
            .iter()
            .zip(&brighten)
            .map(|(o, b)| o + b + lifted)
            .collect();
        if let Some((fix, meant, seen)) =
            keep_light_fix(seq, plan, &base, &a.scene, anchor, keep, opts.pass(), rep)?
        {
            let most = fix
                .iter()
                .fold(0.0_f64, |m, f| if f.abs() > m.abs() { *f } else { m });
            rep.note(&format!(
                "keep light:    in the finished look {seen:+.2} stops where {meant:+.2} were meant (the tone curve): made up for by up to {most:+.2}"
            ));
            for (o, f) in brighten.iter_mut().zip(&fix) {
                *o += f;
            }
        }
    }
    // The brightness shaped by hand, last: what the user drew is what the video does.
    if !r.brightness.is_empty() {
        let mut shape = lapsewerk_core::daynight::shaped(&r.brightness, offsets.len());
        // As drawn (said below), before it is made up for.
        let (lo, hi) = shape
            .iter()
            .fold((f64::MAX, f64::MIN), |(lo, hi), s| (lo.min(*s), hi.max(*s)));
        // The tone curve squeezes the highlights: in a bright frame a stop asked for showed as about
        // three quarters of one (0.24 of +0.30, -0.18 of -0.25). Measured in the finished look
        // where the shape acts, and made up for, so what is drawn is what the video shows.
        let base: Vec<f64> = offsets.iter().zip(&brighten).map(|(o, b)| o + b).collect();
        if let Some((gains, at)) = shape_gains(seq, plan, &base, &shape, opts.pass(), rep)? {
            for (s, g) in shape.iter_mut().zip(&gains) {
                *s /= g;
            }
            let (lo, hi) = gains
                .iter()
                .fold((f64::MAX, f64::MIN), |(lo, hi), g| (lo.min(*g), hi.max(*g)));
            rep.note(&format!(
                "brightness:    the {} acts by {:.0}-{:.0} % in the finished look (measured at {at} frame(s)): made up for, so it shows as {}",
                if lifted > 0.0 { "auto lift" } else { "hand-drawn shape" },
                lo * 100.0,
                hi * 100.0,
                if lifted > 0.0 { "meant" } else { "drawn" }
            ));
        }
        for ((o, d), s) in brighten.iter_mut().zip(drawn.iter_mut()).zip(&shape) {
            *o += s;
            *d = *s;
        }
        let beyond = r.brightness.iter().filter(|(f, _)| *f >= offsets.len()).count();
        let mut frames: Vec<usize> = r.brightness.iter().map(|(f, _)| *f).collect();
        frames.sort_unstable();
        let twice = frames.windows(2).filter(|w| w.first() == w.get(1)).count();
        // The auto lift is said above ("auto bright:"): nothing was drawn by hand.
        if lifted <= 0.0 {
            rep.note(&format!(
                "brightness:    shaped by hand at {} point(s), {lo:+.2} to {hi:+.2} stops{}{}",
                r.brightness.len(),
                if beyond > 0 {
                    format!(
                        "; {beyond} past the last frame ({}): they shape the curve towards them",
                        offsets.len()
                    )
                } else {
                    String::new()
                },
                if twice > 0 {
                    format!("; {twice} frame(s) given twice: the first point counts")
                } else {
                    String::new()
                }
            ));
        }
    }
    if r.passes >= 2 && r.deflicker.is_some() {
        // Check pass: measure the frames as they will be rendered. The tone curve bends exposure
        // changes (a correction of x stops moves the result by more or less than x); what is
        // left is corrected once more, divided by how strongly corrections acted.
        let extras = measure::Extras {
            grid: r.regional,
            reference: opts.reference,
            ..Default::default()
        };
        // Each frame measured as bright as the series' brightest (its day), less the first
        // [`LIFT_FREE`] stops, which it is measured as it is: a deep holy grail's
        // night measured as compensated sits stops down in the tone curve's toe, where
        // measurements stop following exposure and the corrections came out several times too
        // large (a jump of a stop) - whatever the day-to-night share; and nothing is measured
        // brighter than the day, where a frame would clip.
        // The keyframes' own brightness changes count: a keyframe a stop brighter at the end
        // is already a stop up, and lifted by the whole night as well it burnt out (0.037 stops
        // of "flicker" read off a finished series that shows 0.001).
        let shown_trend: Vec<f64> = a
            .video_trend
            .iter()
            .zip(a.edits.iter().chain(std::iter::repeat(&0.0)))
            .map(|(v, e)| v + e)
            .collect();
        let top = shown_trend
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .fold(f64::MIN, f64::max);
        let lift: Vec<f64> = shown_trend
            .iter()
            .map(|v| {
                if v.is_finite() && top.is_finite() {
                    (top - v - LIFT_FREE).max(0.0)
                } else {
                    0.0
                }
            })
            .collect();
        let shown: Vec<f64> = offsets.iter().zip(&lift).map(|(o, b)| o + b).collect();
        let mut check = measure::pass(seq, plan, &shown, opts.pass(), extras, Step::Check, rep)?;
        // The keyframes' own changes and the brightness are meant: out of what the check pass
        // smooths.
        for ((m, e), b) in check.stops.iter_mut().zip(&a.edits).zip(&lift) {
            if let Some(m) = m {
                *m -= e + b;
            }
        }
        let params = deflicker_params(r.deflicker);
        let residual = deflicker::corrections(&check.stops, &params);
        // How strongly corrections act, frame by frame (over about 30 frames): along a day-to-
        // night sequence the frames move along the tone curve, and corrections act differently.
        let gains = deflicker::correction_gains(&a.compensated, &check.stops, &a.corrections, 30.0);
        // The check pass refines: it never more than doubles what is left, and what it would move
        // a frame by more than [`CHECK_MOST`] is a measurement gone wrong (a channel burnt out in a
        // moonlit sky, a frame deep in the dark), not flicker the first pass left: that frame
        // stays as the first pass has it (a flash of a third of a stop otherwise).
        let gains: Vec<f64> = gains.iter().map(|g| g.max(0.5)).collect();
        // A check that measures far more flicker than the first pass leaves by its own
        // measurements (three times, and over 0.04 stops: a look's contrast or highlights alone
        // read 0.015 here and are real) measures something else: a
        // moon burning out as frames are lifted, pixels at white coming and going. Its corrections
        // would put flicker in, not take it out: none are made.
        let predicted: Vec<Option<f64>> = a
            .as_shot
            .stops
            .iter()
            .zip(&a.edits)
            .zip(&offsets)
            .map(|((m, e), o)| m.map(|m| m - e + o))
            .collect();
        let (expected, measured) = (jitter(&predicted), jitter(&check.stops));
        let trusted = measured <= (3.0 * expected).max(0.04);
        if !trusted {
            scene_changes = true;
            rep.note(&format!(
                "check pass:    left out: it measures {measured:.4} stops where the first pass left {expected:.4}, so something in the picture changes on its own (clouds or light moving, bright parts burning out); the first pass's corrections are kept"
            ));
        }
        let more: Vec<f64> = residual
            .iter()
            .zip(&gains)
            .map(|(c, g)| {
                let m = c / g;
                if trusted && m.is_finite() && m.abs() <= CHECK_MOST {
                    m
                } else {
                    0.0
                }
            })
            .collect();
        let (lo, hi) = gains
            .iter()
            .fold((f64::MAX, f64::MIN), |(lo, hi), g| (lo.min(*g), hi.max(*g)));
        let (lo, hi) = ((lo * 100.0).round(), (hi * 100.0).round());
        let how = if lo == hi && lo == 100.0 {
            "an exposure correction acts fully on the frames' brightness".to_owned()
        } else if lo == hi {
            format!(
                "an exposure correction moves the frames' brightness by {lo:.0} % of its size (tone curve and tonal settings), which the check pass allows for"
            )
        } else {
            format!(
                "an exposure correction moves the frames' brightness by {lo:.0}-{hi:.0} % of its size (tone curve and tonal settings), which the check pass allows for"
            )
        };
        // Not trusted: said once above, not again as flicker left.
        if trusted {
            rep.note(&format!(
            "check pass:    flicker {:.4} stops left by the first pass (measured on the frames without tone curve, contrast, shadows and highlights, before this pass corrects it; --verify measures the finished frames as seen); {how}",
            jitter(&check.stops)
        ));
        }
        // The check's corrections checked themselves: measured once more, lifted as before.
        // Where they do not make the flicker clearly smaller (steps of a few hundredths built into
        // a night by a measurement that was off), the first pass's offsets stay.
        let mut more = more;
        if trusted && more.iter().any(|m| *m != 0.0) {
            let after: Vec<f64> = offsets
                .iter()
                .zip(&more)
                .zip(&lift)
                .map(|((o, m), l)| o + m + l)
                .collect();
            let mut again = measure::pass(seq, plan, &after, opts.pass(), extras, Step::Check, rep)?;
            for ((m, e), b) in again.stops.iter_mut().zip(&a.edits).zip(&lift) {
                if let Some(m) = m {
                    *m -= e + b;
                }
            }
            let (before, now) = (jitter(&check.stops), jitter(&again.stops));
            if now > 0.9 * before {
                rep.note(&format!(
                    "check pass:    its corrections would leave {now:.4} stops of flicker where there was {before:.4}{}: not used (the first pass's corrections stay; nothing gets worse)",
                    if now > before { ", more" } else { ", less than a tenth better" }
                ));
                more = vec![0.0; more.len()];
            }
        }
        for (o, m) in offsets.iter_mut().zip(&more) {
            *o += m;
        }
        if r.regional.is_some() {
            let c = lapsewerk_core::regional::corrections(&check.tiles, &check.stops, &params);
            let largest = c.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
            rep.note(&format!(
                "regional:      tile flicker {:.4} stops (mean over tiles); largest regional correction {largest:.3}",
                lapsewerk_core::regional::tile_jitter(&check.tiles)
            ));
            regional = Some(c);
        }
        second = Some((check, more));
    }
    // The day-to-night brightness and the hand-drawn shape go on top of everything else: flicker
    // is an error in the light that reached the sensor, so its correction holds however bright
    // the frame is shown.
    for (o, b) in offsets.iter_mut().zip(&brighten) {
        *o += b;
    }
    if r.verify {
        let extras = measure::Extras {
            grid: r.regional,
            regional: regional.as_deref(),
            reference: opts.reference,
            ..Default::default()
        };
        let mut v = measure::pass(seq, plan, &offsets, opts.pass(), extras, Step::Verify, rep)?;
        for (m, e) in v
            .stops
            .iter_mut()
            .chain(v.perceived.iter_mut())
            .zip(a.edits.iter().chain(&a.edits))
        {
            if let Some(m) = m {
                *m -= e;
            }
        }
        // Photos the render replaces (a flash) and the jump across a gap it fills are not what the
        // video shows: left out (else a replaced flash read as 0.2 stops of flicker).
        for i in r
            .repair
            .replace
            .iter()
            .copied()
            .chain(r.repair.fill.iter().map(|(b, _)| *b))
        {
            for list in [&mut v.stops, &mut v.perceived] {
                if let Some(s) = list.get_mut(i) {
                    *s = None;
                }
            }
        }
        // The flicker as seen only: the log-mean figure beside it read as too much flicker where
        // bright parts burn out, and said nothing more.
        let mut line = format!("verified:      flicker as seen {:.4} stops", jitter(&v.perceived));
        if r.regional.is_some() {
            line.push_str(&format!(
                ", tile flicker {:.4}",
                lapsewerk_core::regional::tile_jitter(&v.tiles)
            ));
        }
        let whole = jitter(&v.perceived);
        // The roughest stretch of 25 frames, when it stands out (a night end the mean hides).
        let n = v.perceived.len();
        let worst = (0..n.saturating_sub(24))
            .step_by(5)
            .filter_map(|a| v.perceived.get(a..a + 25).map(|w| (a, jitter(w))))
            .max_by(|p, q| p.1.total_cmp(&q.1));
        if let Some((a, j)) = worst.filter(|(_, j)| *j > 2.0 * whole && *j > 0.004) {
            line.push_str(&format!("; roughest at frames {}-{}: {j:.4}", a + 1, a + 25));
        }
        // A slow swell or kink over 10-20 frames, which the frame-to-frame figure hardly counts
        // (a ramp's knee pumping, a moonrise pulled forward): said from 0.03 stops on.
        // Each knee of a day-to-night-to-day series can swell: every one named.
        for (at, a) in measure::slow_waves(&v.perceived, SLOW_WAVE_SAID) {
            let (from, to) = measure::wave_span(&v.perceived, at);
            let (from, to) = (from + 1, to + 1);
            line.push_str(&format!(
                "; strays up to {a:.3} stops from a smooth course around frames {from}-{to} (a slow swell or kink over 10-20 frames)"
            ));
            // The hand-drawn brightness bends there itself: as drawn, not a fault.
            let drawn_there: Vec<Option<f64>> = drawn.iter().map(|d| Some(*d)).collect();
            let hand = (at.saturating_sub(5)..=at + 5)
                .filter_map(|i| measure::wave_at(&drawn_there, i))
                .fold(0.0f64, f64::max);
            if hand > SLOW_WAVE_SAID {
                line.push_str(", where the hand-drawn brightness bends (as drawn)");
            } else if let Some(k) = plan.keyframes().iter().find(|k| k.abs_diff(at) <= 10) {
                line.push_str(&format!(", near keyframe {} (its change of look?)", k + 1));
            }
        }
        // Where frame-to-frame jumps are largest: at keyframes, it is their own change of look.
        let d2: Vec<(usize, f64)> = v
            .perceived
            .windows(3)
            .enumerate()
            .filter_map(|(i, w)| match w {
                [Some(a), Some(b), Some(c)] => Some((i + 1, (a - 2.0 * b + c).abs())),
                _ => None,
            })
            .collect();
        let mut top = d2.clone();
        top.sort_by(|p, q| q.1.total_cmp(&p.1));
        let at_keyframes = !top.is_empty()
            && top
                .iter()
                .take(3)
                .all(|(i, _)| plan.keyframes().iter().any(|k| k.abs_diff(*i) <= 1));
        // A hundredth of a stop frame to frame is about what shows: say so, with what helps.
        if whole > 0.01 && at_keyframes {
            let ks: Vec<String> = top.iter().take(3).map(|(i, _)| (i + 1).to_string()).collect();
            line.push_str(&format!(
                "; mostly at keyframe(s) {} (frames from 1): the keyframes' own change of look, as edited (a softer change there, or keyframes further apart, smooths it)",
                ks.join(", ")
            ));
        } else if whole > 0.01 && r.deflicker.is_none() {
            line.push_str("; warning: deflicker is off (--deflicker 8 takes the flicker out)");
        } else if whole > 0.01
            && let Some((i, j)) = top.first().filter(|_| !line.contains("roughest at"))
        {
            // No stretch stands out: the photo that stands out most, as the app names it.
            line.push_str(&format!(
                "; largest jump at frame {}: {j:.4} stops against its neighbours (look there: a camera step, something bright such as the moon? --reference may help)",
                i + 1
            ));
        } else if whole > 0.01 && scene_changes {
            // Not flicker the deflicker left, as far as can be told: the scene moving.
            line.push_str(
                "; most likely the scene itself (clouds or light moving, see the check pass): nothing to change unless the movie shows it",
            );
        } else if whole > 0.01 {
            line.push_str(
                "; warning: more than expected, look at the movie (a longer --deflicker smoothing, or --passes 1, may help)",
            );
        }
        rep.note(&line);
        verified_seen = Some(v.perceived.clone());
        verified = Some(v.stops);
    }
    // Dust last of the measurements: looked for in frames as they will be rendered.
    let dust = if r.dust {
        dust::find(seq, plan, &offsets, r.jobs, (r.video.width, r.video.height), rep)?
    } else {
        None
    };
    let undrawn: Vec<f64> = offsets.iter().zip(&drawn).map(|(o, d)| o - d).collect();
    let brightening = smoothed_triangle(&undrawn, BRIGHTENING_RADIUS);
    Ok(Corrected {
        offsets,
        brightening,
        second,
        regional,
        verified,
        verified_seen,
        dust,
        size,
        render: r,
    })
}

/// `c`'s corrections for the render arguments `render` (the movie after the preview, or a smaller
/// test video): the offsets, the check pass and regional deflicker carry over when what they were
/// worked out from is the same (passes, regional grid, light kept); dust is looked for when asked.
/// `None` when they do not carry over, or `render` asks for verification: then [`correct`] anew.
///
/// # Errors
/// When looking for dust fails, or [`CANCELLED`].
pub fn reuse(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    c: &Corrected,
    render: &RenderArgs,
    rep: &dyn Reporter,
) -> Result<Option<Corrected>, String> {
    let old = &c.render;
    if render.verify
        || render.passes != old.passes
        || render.regional != old.regional
        || render.keep_light != old.keep_light
        || render.deflicker != old.deflicker
        || render.brightness != old.brightness
        || render.auto_brightness != old.auto_brightness
        || render.base_look != old.base_look
    {
        return Ok(None);
    }
    let mut r = render.clone();
    upright(&mut r, c.size);
    // Another shape of video frames a wide crop otherwise (`framed_plan`): worked out anew.
    let aspect = |v: &lapsewerk_video::VideoSettings| v.width as f64 / v.height.max(1) as f64;
    if (aspect(&r.video) / aspect(&old.video) - 1.0).abs() > 1e-3 {
        return Ok(None);
    }
    rep.note(&format!(
        "video:         {}×{}, position {:.2},{:.2} (corrections of the preview)",
        r.video.width, r.video.height, r.position.0, r.position.1
    ));
    note_upscale(seq, &r, rep);
    let dust = if r.dust {
        dust::find(
            seq,
            plan,
            &c.offsets,
            r.jobs,
            (r.video.width, r.video.height),
            rep,
        )?
    } else {
        None
    };
    Ok(Some(Corrected {
        offsets: c.offsets.clone(),
        brightening: c.brightening.clone(),
        second: c.second.clone(),
        regional: c.regional.clone(),
        verified: None,
        verified_seen: None,
        dust,
        size: c.size,
        render: r,
    }))
}

/// Flicker (stops, frame to frame) of the series as shot and as the deflicker with `smoothing` frames
/// would leave it, from the analysis; frames in `skip` (replaced ones) left out.
#[must_use]
pub fn predicted_flicker(a: &Analysis, smoothing: Option<f64>, skip: &[usize]) -> (f64, f64) {
    let values: Vec<Option<f64>> = a
        .compensated
        .iter()
        .enumerate()
        .map(|(i, v)| if skip.contains(&i) { None } else { *v })
        .collect();
    let before = jitter(&values);
    let Some(s) = smoothing else {
        return (before, before);
    };
    let c = deflicker::corrections(&values, &deflicker_params(Some(s)));
    let after: Vec<Option<f64>> = values.iter().zip(&c).map(|(v, c)| v.map(|v| v + c)).collect();
    (before, jitter(&after))
}

/// Warns when the frames are smaller than the video (or its closest zoom) needs.
fn note_upscale(seq: &Sequence, r: &RenderArgs, rep: &dyn Reporter) {
    let Some(source) = seq.source_size(0) else { return };
    let zoom = r.framing.map_or(1.0, |m| m.max_zoom());
    let f = upscale(source, (r.video.width, r.video.height), zoom);
    if f > 1.05 {
        rep.note(&format!(
            "warning:       the frames ({}×{}) are scaled up {f:.1}× for this video{}: it will look softer than its size",
            source.0,
            source.1,
            if zoom > 1.0 { " and zoom" } else { "" }
        ));
    }
}

/// How far below the series' brightest (stops) the check pass measures a frame as it is: only what
/// lies deeper is lifted. A deep night sits in the tone curve's toe and is lifted out of it; a
/// sunrise shown within a couple of stops is not lifted into its sun and sky (a real A7 IV
/// sunrise, 2 stops in the video, lifted to its day: the check pass measured corrections acting
/// at 50-294 % and was thrown away, 0.019 stops of flicker left instead of 0.010).
const LIFT_FREE: f64 = 2.0;

/// How much darker than shot frames are measured (stops): room for a keyframe look's highlights
/// (a strong night look, +2 stops and cool, still measured unclipped).
const MEASURE_HEADROOM: f64 = 1.5;

/// Smallest step of the camera (stops) a measurement evens out. Steps of a third or two thirds of
/// a stop burn nothing out, and cameras that follow the light steplessly between frames (the
/// A7 IV's interval exposure) write such steps into EXIF that the sensor data do not show: evened
/// out, they came back as a saw tooth (the tone curve moves a frame by 114-148 % of an offset, so
/// taking the offset off again leaves the rest), and a real sunrise kept 0.039 stops of flicker
/// instead of 0.008.
const LOCAL_STEP_MIN: f64 = 0.75;

/// The part of an EXIF ramp a measurement evens out: the ramp's steps of at least
/// [`LOCAL_STEP_MIN`] less their smooth trend (a few frames wide), so a big step shows as a
/// fraction of a stop either side of it rather than at full height.
fn local_steps(exif_ramp: &[f64]) -> Vec<f64> {
    let mut big = Vec::with_capacity(exif_ramp.len());
    let mut sum = 0.0;
    let mut last = exif_ramp.first().copied().unwrap_or(0.0);
    for &r in exif_ramp {
        if r.is_finite() {
            let d = r - last;
            if d.abs() >= LOCAL_STEP_MIN {
                sum += d;
            }
            last = r;
        }
        big.push(sum);
    }
    let trend = lapsewerk_core::smooth::local_linear(&big, None, 6.0);
    big.iter()
        .zip(&trend)
        .map(|(r, t)| {
            let d = r - t;
            if d.is_finite() { d.clamp(-3.0, 3.0) } else { 0.0 }
        })
        .collect()
}

/// The folder a drive is mounted on (`/media/NAME/DRIVE`, `/mnt/DRIVE`, `/Volumes/DRIVE`) along
/// `out`, when nothing is mounted there: an empty folder on the same disk as the folder above it.
/// Rendering there would fill the system disk instead of the delivery drive.
fn unmounted_drive(out: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let parts: Vec<_> = out.components().collect();
        // Where drives are mounted: /media/DRIVE or /media/USER/DRIVE, /run/media/USER/DRIVE,
        // /mnt/DRIVE, /Volumes/DRIVE.
        let depths: &[usize] = match parts.get(1).and_then(|c| c.as_os_str().to_str()) {
            Some("media") => &[3, 4],
            Some("run") => &[5],
            Some("mnt" | "Volumes") => &[3],
            _ => return None,
        };
        depths.iter().find_map(|&depth| {
            if parts.len() < depth {
                return None;
            }
            let mount: PathBuf = parts.iter().take(depth).collect();
            let parent = mount.parent()?;
            let (m, p) = (std::fs::metadata(&mount).ok()?, std::fs::metadata(parent).ok()?);
            let empty = std::fs::read_dir(&mount).ok()?.next().is_none();
            // Mounted: another device than the folder above. Not mounted and empty: nothing
            // there (the frames would go onto the system disk, the mount point itself too).
            (m.dev() == p.dev() && empty).then_some(mount)
        })
    }
    #[cfg(not(unix))]
    {
        let _ = out;
        None
    }
}

/// Refuses an output that would mix with or write over the photos: a frame sequence into the
/// folder holding them (its frames would sit among them, and frames of the same name be taken for
/// them), a movie in place of one of them, an empty path.
///
/// # Errors
/// When the output is one of those.
pub fn check_target(photos: &[PathBuf], out: &Path, sequence: bool) -> Result<(), String> {
    if out.as_os_str().is_empty() {
        return Err("the output path is empty".into());
    }
    // Compared as the system resolves them (links, "..", a relative path); a path not made yet
    // by its folder's resolved path and its name.
    let resolved = |p: &Path| -> PathBuf {
        if let Ok(c) = p.canonicalize() {
            return c;
        }
        match (p.parent(), p.file_name()) {
            (Some(d), Some(n)) => {
                let d = if d.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    d
                };
                d.canonicalize().map_or_else(|_| p.to_path_buf(), |d| d.join(n))
            }
            _ => p.to_path_buf(),
        }
    };
    // As given too: on a Mac /tmp, /var and /etc are links into /private, three levels deep.
    let given_top = out.is_absolute() && out.components().count() <= 2;
    let out = resolved(out);
    let mut folders: Vec<PathBuf> = Vec::new();
    for photo in photos {
        let photo = resolved(photo);
        if photo == out {
            return Err(format!(
                "{} is one of the photos: choose another output",
                out.display()
            ));
        }
        if let Some(d) = photo.parent()
            && !folders.iter().any(|f| f == d)
        {
            folders.push(d.to_path_buf());
        }
    }
    if sequence && folders.contains(&out) {
        return Err(format!(
            "{} is the photo folder itself: choose a folder of its own for the frames (inside it, or elsewhere)",
            out.display()
        ));
    }
    // Another series' folder (its project file, its photos): frames there would join that series.
    if sequence
        && out.is_dir()
        && let Ok(entries) = std::fs::read_dir(&out)
    {
        const PHOTOS: [&str; 17] = [
            "jpg", "jpeg", "arw", "cr2", "cr3", "nef", "dng", "raf", "orf", "rw2", "pef", "srw", "heic",
            "heif", "tif", "tiff", "png",
        ];
        // Frames of this output (TIFF too) are its own, not a series' photos.
        let own = out
            .file_name()
            .map(|n| format!("{}_", n.to_string_lossy().to_lowercase()))
            .unwrap_or_default();
        // Its project file, or several photos (one reference JPEG in a delivery folder is none).
        let (mut project, mut photos) = (false, 0usize);
        for e in entries.flatten().take(100_000) {
            let name = e.file_name().to_string_lossy().to_lowercase();
            project |= name == lapsewerk_develop::project::FILE_NAME;
            photos += usize::from(
                !name.starts_with(&own)
                    && name
                        .rsplit_once('.')
                        .is_some_and(|(_, ext)| PHOTOS.contains(&ext)),
            );
        }
        // A folder of Lapsewerk's own frames (its manifest beside them) is no series, whatever they are named.
        let ours = out.join(".lapsewerk-frames").is_file();
        let series = project || (photos > 3 && !ours);
        if series {
            return Err(format!(
                "{} holds a series' photos or pictures not made by Lapsewerk ({photos} picture files{}): choose a folder of its own for the frames",
                out.display(),
                if project { ", and a project file" } else { "" }
            ));
        }
    }
    // The root, a top-level system folder or the home folder itself: never a delivery's folder.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if sequence && (given_top || out.components().count() <= 2 || home.as_deref() == Some(out.as_path())) {
        return Err(format!(
            "{} is a system or home folder: choose a folder of its own for the frames",
            out.display()
        ));
    }
    // The system's own trees (devices, processes, the kernel): never a place for a delivery,
    // a movie or frames (/dev/null took the frames; /proc said "0 MB free").
    if out.starts_with("/dev") || out.starts_with("/proc") || out.starts_with("/sys") {
        return Err(format!(
            "{} is a system or home folder: choose a folder of its own for the frames",
            out.display()
        ));
    }
    if let Some(mount) = unmounted_drive(&out) {
        return Err(format!(
            "{} looks like a drive that is not mounted (an empty folder on the system disk where a drive belongs): mount it, or choose another output",
            mount.display()
        ));
    }
    Ok(())
}

/// Renders the movie with `c`'s corrections; returns its path.
///
/// # Errors
/// As [`export::render`].
pub fn render(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    a: &Analysis,
    c: &Corrected,
    rep: &dyn Reporter,
) -> Result<PathBuf, String> {
    let photos: Vec<PathBuf> = seq.frames().iter().map(|f| f.path.clone()).collect();
    check_target(&photos, &c.render.out, c.render.sequence)?;
    let fix = export::Corrections {
        offsets: &c.offsets,
        brightening: &c.brightening,
        dust: c.dust.as_ref().filter(|m| !m.is_empty()),
        regional: c.regional.as_deref(),
        motion: a.motion.as_deref(),
    };
    let looked = framed_plan(
        seq,
        plan,
        c.render.base_look,
        (c.render.video.width, c.render.video.height),
    );
    export::render(seq, &looked, &fix, c.size, &c.render, rep)
}

/// A frame for the screen: 8-bit sRGB, RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// A render job for a preview of `frame`: developed with `plan` (its keyframes) plus `offset` stops,
/// fitting `edge` pixels. Make it while holding the sequence, run it with [`run_preview`] anywhere.
///
/// # Errors
/// When `frame` is out of range or LightCraft cannot make the job.
pub fn preview_job(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    frame: usize,
    offset: f64,
    edge: usize,
) -> Result<lapsewerk_develop::RenderJob, String> {
    preview_job_with(seq, frame, plan.settings(frame, offset), edge)
}

/// [`preview_job`] with explicit settings (a keyframe being edited).
///
/// # Errors
/// As [`preview_job`].
pub fn preview_job_with(
    seq: &mut Sequence,
    frame: usize,
    settings: lapsewerk_develop::DevelopSettings,
    edge: usize,
) -> Result<lapsewerk_develop::RenderJob, String> {
    seq.render_job(frame, settings, edge, edge, lapsewerk_develop::Output::Preview8)
        .map_err(|e| e.to_string())
}

/// Runs a [`preview_job`]; a crash in the renderer becomes an error.
///
/// # Errors
/// When the frame cannot be rendered.
pub fn run_preview(job: lapsewerk_develop::RenderJob) -> Result<Picture, String> {
    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.run()))
        .map_err(|_| "the renderer crashed".to_owned())?;
    let r = done.rendered?;
    let img = r.image;
    Ok(Picture {
        width: img.width,
        height: img.height,
        rgba: img.data.into_iter().flatten().collect(),
    })
}

/// Raw files in `folders` and their subfolders (four levels down at most), sorted; hidden files and
/// folders left out.
#[must_use]
pub fn raw_files_in(folders: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() && depth < 4 {
                walk(&path, depth + 1, out);
            } else if kind.is_file()
                && path
                    .extension()
                    .is_some_and(|x| RAW_EXTENSIONS.contains(&x.to_string_lossy().to_lowercase().as_str()))
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    for f in folders {
        if f.is_file() {
            out.push(f.clone());
        } else {
            walk(f, 0, &mut out);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Most photos read for a camera profile (spread over all that were found).
pub const CALIBRATE_MAX: usize = 300;

/// A camera profile from the raw files in `folders` (see `lapsewerk_develop::calibrate`), written
/// to LightCraft's profiles folder (or `dir`).
///
/// # Errors
/// When no raw file is found, nothing can be written, or [`CANCELLED`].
pub fn calibrate(
    folders: &[PathBuf],
    dir: Option<&Path>,
    rep: &dyn Reporter,
) -> Result<lapsewerk_develop::calibrate::Calibrated, String> {
    let found = raw_files_in(folders);
    if found.is_empty() {
        return Err("no raw files found".into());
    }
    let files = lapsewerk_develop::calibrate::spread(&found, CALIBRATE_MAX);
    rep.note(&format!(
        "calibrate:     {} of {} raw files",
        files.len(),
        found.len()
    ));
    rep.progress(Step::Calibrate, 0, files.len());
    let result = lapsewerk_develop::calibrate::calibrate(&files, dir, &|done, total| {
        rep.progress(Step::Calibrate, done, total);
        !rep.cancelled()
    });
    match result {
        Err(_) if rep.cancelled() => Err(CANCELLED.into()),
        Err(e) => Err(e),
        Ok(c) => {
            for (model, n, path) in &c.written {
                rep.note(&format!(
                    "profile:       {model} from {n} photos → {}",
                    path.display()
                ));
            }
            for (model, n) in &c.too_few {
                rep.note(&format!(
                    "too few:       {model}: {n} usable photo(s), {} needed",
                    lapsewerk_develop::calibrate::MIN_PHOTOS
                ));
            }
            Ok(c)
        }
    }
}

/// How far a `source` frame is scaled up to cover the `video` size at `zoom` (1: not at all; the
/// shape is turned to match, as the export does). Over 1, the video is softer than its size.
#[must_use]
pub fn upscale(source: (usize, usize), video: (usize, usize), zoom: f64) -> f64 {
    let (mut sw, mut sh) = (source.0.max(1) as f64, source.1.max(1) as f64);
    let (vw, vh) = (video.0 as f64, video.1 as f64);
    // An upright video from an upright frame: compare like with like.
    if (sw > sh) != (vw > vh) && sw != sh {
        std::mem::swap(&mut sw, &mut sh);
    }
    let cover = (vw / sw).max(vh / sh);
    let zoom = if zoom.is_finite() { zoom.max(1.0) } else { 1.0 };
    (cover * zoom).max(1.0)
}

/// What could spoil the movie, found by the analysis.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Quality {
    /// Frames that stand out from their steady neighbours (a flash, headlights).
    pub outliers: Vec<usize>,
    /// Frames with more than 1 % at white.
    pub clipped: Vec<usize>,
    /// The largest share at white of any frame.
    pub clipped_most: f32,
    /// (before which frame, how many missing): pauses in the capture.
    pub missing: Vec<(usize, usize)>,
}

/// How far (stops) a frame must stand out to count as odd.
pub const OUTLIER_STOPS: f64 = 0.4;

/// Most frames put into one gap; a longer pause is a cut.
pub const MAX_FILL: usize = 12;

/// The analysis's warnings for the frames of `seq`.
#[must_use]
pub fn quality(a: &Analysis, frames: &[sequence::Frame]) -> Quality {
    quality_with(a, frames, &[])
}

/// [`quality`], with `skipped` (before which frame, how many) unreadable photos left out: gaps,
/// whatever the capture times say.
#[must_use]
pub fn quality_with(a: &Analysis, frames: &[sequence::Frame], skipped: &[(usize, usize)]) -> Quality {
    let clipped: Vec<usize> = a
        .as_shot
        .clipped
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_some_and(|c| c > 0.01))
        .map(|(i, _)| i)
        .collect();
    let clipped_most = a.as_shot.clipped.iter().flatten().fold(0.0f32, |m, c| m.max(*c));
    let times: Vec<Option<i64>> = frames.iter().map(|f| f.captured_ms).collect();
    let interval = sequence::timing(frames).map_or(0, |t| t.interval_ms);
    Quality {
        outliers: lapsewerk_core::quality::outliers(&a.compensated, OUTLIER_STOPS),
        clipped,
        clipped_most,
        missing: {
            let mut m = lapsewerk_core::quality::missing_frames(&times, interval, MAX_FILL);
            for &(at, n) in skipped {
                match m.iter_mut().find(|(f, _)| *f == at) {
                    Some(g) => g.1 = g.1.max(n),
                    None => m.push((at, n.min(MAX_FILL))),
                }
            }
            m.sort_unstable();
            m
        },
    }
}

/// The frames' common size as measured (the video is cut from it): every frame rendered must have
/// the same shape, or the movie would stop at the first one that does not (a camera turned upright
/// or pointed at the zenith flips the orientation in EXIF).
pub fn frame_size(sizes: &[Option<(usize, usize)>]) -> Result<(usize, usize), String> {
    let (first, size) = sizes
        .iter()
        .enumerate()
        .find_map(|(i, s)| s.map(|s| (i, s)))
        .ok_or("no frame could be rendered")?;
    let aspect = |(w, h): (usize, usize)| w as f64 / h.max(1) as f64;
    let other: Vec<String> = sizes
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            s.filter(|s| (aspect(*s) / aspect(size) - 1.0).abs() > 0.02)
                .map(|s| (i, s))
        })
        .take(5)
        .map(|(i, (w, h))| format!("frame {} ({w}×{h})", i + 1))
        .collect();
    if !other.is_empty() {
        return Err(format!(
            "frames of another shape than frame {} ({}×{}): {}; crop them alike in LightCraft or leave them out (frames from 1)",
            first + 1,
            size.0,
            size.1,
            other.join(", ")
        ));
    }
    Ok(size)
}

/// How far above its surroundings a raw sample must be to count as a hot pixel: 2 % of the sensor's
/// range (about 330 of 16383 on a 14-bit raw).
pub const HOT_PIXEL_EXCESS: f32 = 0.02;

/// Up to `count` frames to look for hot pixels in: from the third of the sequence with the most light
/// gathered per frame (exposure time × ISO ÷ aperture²: hot pixels grow with exposure time and gain),
/// spread out over it so that stars have moved between them.
pub fn hot_pixel_frames(frames: &[sequence::Frame], count: usize) -> Vec<usize> {
    let mut by_exposure: Vec<(usize, f64)> = frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let e = f
                .exposure
                .map_or(0.0, |e| e.shutter_s * e.iso / (e.aperture * e.aperture));
            (i, e)
        })
        .collect();
    by_exposure.sort_by(|a, b| b.1.total_cmp(&a.1));
    let third = by_exposure.len().div_ceil(3).max(count.min(by_exposure.len()));
    let mut top: Vec<usize> = by_exposure.iter().take(third).map(|(i, _)| *i).collect();
    top.sort_unstable();
    if top.len() <= count {
        return top;
    }
    (0..count)
        .filter_map(|k| top.get(k * (top.len() - 1) / (count - 1).max(1)).copied())
        .collect()
}

pub fn apply(values: &[Option<f64>], corrections: &[f64]) -> Vec<Option<f64>> {
    values
        .iter()
        .zip(corrections)
        .map(|(m, c)| m.map(|m| m + c))
        .collect()
}

/// How many stops darker than shot the frames are measured: a stop over the brightest keyframe
/// look, at least [`MEASURE_HEADROOM`], at most 8; none when no keyframe look brightens the
/// frames (`brightest` at most 0). Nothing then burns out that was not burnt out as shot, and
/// frames measured darker sink towards black, where the measurement stops following exposure: a
/// real sunrise (A7 IV, measured about six stops under white) measured 1.5 stops darker kept
/// several times the flicker.
fn measure_headroom(brightest: f64) -> f64 {
    if !brightest.is_finite() {
        MEASURE_HEADROOM
    } else if brightest <= 0.0 {
        0.0
    } else {
        (1.0 + brightest).clamp(MEASURE_HEADROOM, 8.0)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn headroom_follows_the_brightest_look() {
        assert!(
            super::measure_headroom(0.0).abs() < 1e-9,
            "no look brightens: none"
        );
        assert!(super::measure_headroom(-1.0).abs() < 1e-9);
        assert!((super::measure_headroom(0.3) - super::MEASURE_HEADROOM).abs() < 1e-9);
        assert!((super::measure_headroom(4.0) - 5.0).abs() < 1e-9);
        assert!((super::measure_headroom(6.0) - 7.0).abs() < 1e-9);
        assert!((super::measure_headroom(20.0) - 8.0).abs() < 1e-9);
        assert!((super::measure_headroom(f64::NAN) - super::MEASURE_HEADROOM).abs() < 1e-9);
    }

    use super::*;

    #[test]
    fn the_hand_shapes_gains_run_between_the_frames_measured() {
        assert!(gains_between(&[], 5).is_none());
        let g = gains_between(&[(2, 0.7), (6, 1.0)], 9).unwrap();
        assert_eq!(g.len(), 9);
        assert!((g[0] - 0.7).abs() < 1e-12 && (g[2] - 0.7).abs() < 1e-12);
        assert!((g[4] - 0.85).abs() < 1e-12, "{g:?}");
        assert!((g[8] - 1.0).abs() < 1e-12);
        assert_eq!(gains_between(&[(3, 0.8)], 5).unwrap(), vec![0.8; 5]);
    }

    #[test]
    fn a_camera_step_brightens_the_blacks_back_over_two_dozen_frames() {
        // A whole stop less brightening from frame 50 on (a camera step without EXIF).
        let steps: Vec<f64> = (0..100).map(|i| if i < 50 { 0.6 } else { -0.4 }).collect();
        let b = smoothed_triangle(&steps, BRIGHTENING_RADIUS);
        let most = b.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max);
        // Over ±4 frames it fell 0.11 a frame; now under a twelfth of the step.
        assert!(most < 0.085, "{most}");
        assert!((b[10] - 0.6).abs() < 1e-12 && (b[90] + 0.4).abs() < 1e-12);
        assert!(smoothed_triangle(&[], 12).is_empty());
        assert!((smoothed_triangle(&[0.3], 12)[0] - 0.3).abs() < 1e-12);
    }

    #[test]
    fn the_output_never_goes_among_the_photos() {
        let dir = std::env::temp_dir().join(format!("lapsewerk-studio-target-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("dir_000001.tif");
        std::fs::write(&photo, b"x").unwrap();
        let photos = vec![photo.clone()];
        // A sequence into the photos' folder (however written), a movie over a photo, no path.
        assert!(check_target(&photos, &dir, true).is_err());
        assert!(check_target(&photos, &dir.join("..").join(dir.file_name().unwrap()), true).is_err());
        assert!(check_target(&photos, &photo, false).is_err());
        assert!(check_target(&photos, Path::new(""), true).is_err());
        assert!(check_target(&photos, Path::new("/"), true).is_err());
        assert!(check_target(&photos, Path::new("/tmp"), true).is_err());
        // The system's own trees: refused for frames and movies alike.
        for p in ["/dev/null", "/proc/x", "/sys/x"] {
            assert!(check_target(&photos, Path::new(p), false).is_err(), "{p}");
            assert!(check_target(&photos, Path::new(p), true).is_err(), "{p}");
        }
        // A folder of its own inside, or a movie beside them: fine.
        assert!(check_target(&photos, &dir.join("Lapsewerk-Export").join("clip"), true).is_ok());
        assert!(check_target(&photos, &dir.join("clip.mov"), false).is_ok());
        // Another series' folder.
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("ref.jpg"), b"x").unwrap();
        // One reference picture in a delivery folder is no series.
        assert!(check_target(&photos, &other, true).is_ok());
        for i in 1..5 {
            std::fs::write(other.join(format!("IMG_000{i}.JPG")), b"x").unwrap();
        }
        assert!(check_target(&photos, &other, true).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upscaling_is_measured_like_the_export_fits() {
        // A 24 MP 3:2 frame covers 4K easily; a 640×400 one is scaled 6×; zoom adds to it.
        assert_eq!(upscale((6000, 4000), (3840, 2160), 1.0), 1.0);
        assert!((upscale((640, 400), (3840, 2160), 1.0) - 6.0).abs() < 1e-9);
        assert!((upscale((4000, 2250), (3840, 2160), 1.5) - 1.44).abs() < 1e-9);
        // An upright frame for an upright video.
        assert_eq!(upscale((4000, 6000), (2160, 3840), 1.0), 1.0);
    }

    #[test]
    fn raw_and_jpeg_pairs_give_one_frame_and_hidden_files_none() {
        let found: Vec<PathBuf> = [
            "DSC0002.JPG",
            "DSC0001.ARW",
            "DSC0001.JPG",
            "._DSC0001.ARW",
            ".DS_Store",
            "DSC0002.arw",
            "DSC0003.jpg",
            "notes.txt",
            "lapsewerk.json",
            "DSC0001.xmp",
        ]
        .iter()
        .map(|n| PathBuf::from("/seq").join(n))
        .collect();
        let (frames, beside) = pick_frames(found);
        let names: Vec<String> = frames
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["DSC0001.ARW", "DSC0002.arw", "DSC0003.jpg"]);
        assert_eq!(beside, 2);
    }

    #[test]
    fn frames_of_another_shape_are_found_before_rendering() {
        assert_eq!(
            frame_size(&[None, Some((768, 512)), Some((768, 513))]),
            Ok((768, 512))
        );
        let err = frame_size(&[Some((768, 512)), Some((512, 768)), None]).unwrap_err();
        assert!(err.contains("frame 2 (512×768)"), "{err}");
        assert!(frame_size(&[None, None]).is_err());
    }

    #[test]
    fn time_left_follows_the_rate_so_far() {
        let d = |s: f64| Duration::from_secs_f64(s);
        assert_eq!(estimate(d(10.0), 0, 100), None);
        assert_eq!(estimate(d(10.0), 25, 100), Some(d(30.0)));
        assert_eq!(estimate(d(10.0), 100, 100), Some(Duration::ZERO));
        assert_eq!(estimate(d(10.0), 101, 100), None);
    }
}
