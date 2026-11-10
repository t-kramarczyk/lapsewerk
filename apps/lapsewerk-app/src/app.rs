//! The app's state and the work behind its pages: opening a project, analysing, keyframes,
//! corrections, export. Drawing is in `start` (the project manager) and `pages` (the six steps).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lapsewerk_core::sequence::Timing;
use lapsewerk_develop::project::Project as ProjectFile;
use lapsewerk_develop::{DevelopPlan, DevelopSettings, Sequence};
use lapsewerk_studio::{Analysis, Corrected, MeasureSettings, OpenSettings, RenderArgs};
use lapsewerk_video::frames::FrameFormat;
use lapsewerk_video::{Codec, Transfer, VideoSettings};
use serde_json::{Value, json};

use crate::preview::{self, Kind, Previews};
use crate::recent::Recent;
use crate::text::Lang;
use crate::work::{Outcome, Task, lock};

/// The steps, 1 to 6.
pub const STEPS: usize = 6;

/// The share of a day to night's change of light the app keeps unless told otherwise: the author's
/// film from his earlier time-lapse workflow of a Hamburg day to night (8.9 stops) kept about 12 % (1.1 stops); 30 % made
/// the night far darker than his.
pub const KEEP_LIGHT_DEFAULT: f32 = 0.15;

/// Options of step 4.
#[derive(Clone, Debug, PartialEq)]
pub struct Finish {
    pub deflicker: bool,
    /// Deflicker smoothing, frames.
    pub smoothing: f32,
    pub thorough: bool,
    /// Fraction of the real change of light kept (day to night); `None`: as compensated.
    pub keep_light: Option<f32>,
    /// Black stays black in frames shown brighter than shot.
    pub hold_blacks: bool,
    /// The video's brightness shaped by hand: (frame, stops) points.
    pub brightness: Vec<(usize, f64)>,
    pub stabilize: bool,
    /// Stabilize by the exact shift (resampled).
    pub subpixel: bool,
    pub dust: bool,
    pub smooth_motion: bool,
    /// In-between frames along the motion (0: none).
    pub tween: usize,
    pub denoise: bool,
    pub light_pollution: bool,
    pub reduce_stars: Option<f32>,
    /// Star trails fading over this many frames (0: endless).
    pub trails: Option<usize>,
    /// Upper part of the video frame that is sky (night options).
    pub sky: f32,
    /// Measure flicker only in the upper part (fraction), else the whole frame.
    pub reference: Option<f32>,
    /// The measuring part is the lower one (the foreground), not the upper.
    pub reference_low: bool,
    pub regional: bool,
    pub edge: usize,
    /// Replace the odd frames the analysis found.
    pub replace_outliers: bool,
    /// Frames marked by hand to replace.
    pub replace_marked: Vec<usize>,
    /// Fill the gaps of the capture.
    pub fill_gaps: bool,
}

impl Default for Finish {
    fn default() -> Self {
        Self {
            deflicker: true,
            smoothing: 8.0,
            thorough: true,
            keep_light: None,
            hold_blacks: true,
            brightness: Vec::new(),
            stabilize: false,
            subpixel: false,
            dust: false,
            smooth_motion: false,
            tween: 0,
            denoise: false,
            light_pollution: false,
            reduce_stars: None,
            trails: None,
            sky: 0.6,
            reference: None,
            reference_low: false,
            regional: false,
            edge: 768,
            replace_outliers: true,
            replace_marked: Vec::new(),
            fill_gaps: true,
        }
    }
}

/// Whether LightCraft can be started: named by `LAPSEWERK_LIGHTCRAFT`, the app (macOS), or
/// `lightcraft` on the PATH.
fn lightcraft_installed() -> bool {
    if std::env::var_os("LAPSEWERK_LIGHTCRAFT").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let places = [
            Some(PathBuf::from("/Applications")),
            home.map(|h| h.join("Applications")),
        ];
        if places.iter().flatten().any(|d| d.join("LightCraft.app").is_dir()) {
            return true;
        }
    }
    let exe = if cfg!(windows) {
        "lightcraft.exe"
    } else {
        "lightcraft"
    };
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(exe).is_file()))
}

/// Starts LightCraft on `photo`: `program` if given, else the LightCraft app (macOS) or
/// `lightcraft` on the PATH.
fn launch_lightcraft(program: Option<&std::ffi::OsStr>, photo: &Path) -> std::io::Result<()> {
    use std::process::Command;
    let app = program.is_none() && cfg!(target_os = "macos");
    let mut cmd = match program {
        Some(program) => Command::new(program),
        None if app => {
            let mut c = Command::new("open");
            c.args(["-a", "LightCraft"]);
            c
        }
        None => Command::new("lightcraft"),
    };
    let mut child = cmd.arg(photo).spawn()?;
    if app {
        // `open` ends at once; it fails when there is no such app.
        if !child.wait()?.success() {
            return Err(std::io::Error::other("LightCraft.app not found"));
        }
    } else {
        // Reaped in the background so it never lingers as a zombie.
        std::thread::spawn(move || child.wait());
    }
    Ok(())
}

/// A camera move (step 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraMove {
    Still,
    ZoomIn,
    ZoomOut,
    PanRight,
    PanLeft,
}

impl CameraMove {
    pub const ALL: [Self; 5] = [
        Self::Still,
        Self::ZoomIn,
        Self::ZoomOut,
        Self::PanRight,
        Self::PanLeft,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Still => "still",
            Self::ZoomIn => "zoomIn",
            Self::ZoomOut => "zoomOut",
            Self::PanRight => "panRight",
            Self::PanLeft => "panLeft",
        }
    }
}

/// Framing: where the video sits in the frame and how it moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub kind: CameraMove,
    /// How far it zooms in (0..1: up to twice).
    pub amount: f32,
    /// Position of the view, 0..1 across and down.
    pub x: f32,
    pub y: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            kind: CameraMove::Still,
            amount: 0.3,
            x: 0.5,
            y: 0.5,
        }
    }
}

impl Camera {
    /// The move, `None` when still (a plain crop at `x`, `y`).
    #[must_use]
    pub fn framing(&self) -> Option<lapsewerk_core::framing::Move> {
        // As the command line reads them back from `cli_args` (four places), so the app's render
        // and `--from-project` or the queue are the same to the bit.
        use lapsewerk_core::framing::{Move, View};
        let (x, y) = (as_written(f64::from(self.x)), as_written(f64::from(self.y)));
        let zoom = as_written(1.0 + f64::from(self.amount.clamp(0.05, 1.0)));
        let at = |x: f64, zoom: f64| View { x, y, zoom };
        match self.kind {
            CameraMove::Still => None,
            CameraMove::ZoomIn => Some(Move {
                from: at(x, 1.0),
                to: at(x, zoom),
            }),
            CameraMove::ZoomOut => Some(Move {
                from: at(x, zoom),
                to: at(x, 1.0),
            }),
            CameraMove::PanRight => Some(Move {
                from: at(0.0, zoom),
                to: at(1.0, zoom),
            }),
            CameraMove::PanLeft => Some(Move {
                from: at(1.0, zoom),
                to: at(0.0, zoom),
            }),
        }
    }

    /// The view at frame `i` of `n`.
    #[must_use]
    pub fn view(&self, i: usize, n: usize) -> lapsewerk_core::framing::View {
        self.framing().map_or(
            lapsewerk_core::framing::View {
                x: as_written(f64::from(self.x)),
                y: as_written(f64::from(self.y)),
                zoom: 1.0,
            },
            |m| m.at_frame(i, n),
        )
    }
}

/// The plan the previews show, as the export has it: with the base look while no keyframe is
/// edited, and a crop wider than the video of `video` made taller to its shape.
fn shown_plan(
    plan: &Arc<DevelopPlan>,
    base_look: bool,
    seq: &Mutex<Sequence>,
    video: (usize, usize),
) -> Arc<DevelopPlan> {
    Arc::new(lapsewerk_studio::framed_plan(&lock(seq), plan, base_look, video))
}

/// Export formats (step 6): size, codec.
pub const PRESETS: [((usize, usize), Codec); 4] = [
    ((3840, 2160), Codec::ProRes422Hq),
    ((3840, 2160), Codec::Hevc),
    ((1920, 1080), Codec::Hevc),
    ((2160, 3840), Codec::Hevc),
];

/// The folder beside the photos that exports go into unless another place is chosen.
pub const EXPORT_DIR: &str = "Lapsewerk-Export";

/// Codecs to choose from (Advanced).
pub const CODECS: [Codec; 4] = [
    Codec::ProRes422Hq,
    Codec::ProRes422,
    Codec::ProRes4444,
    Codec::Hevc,
];

/// A codec's name as the command line writes it.
#[must_use]
pub fn codec_name(c: Codec) -> &'static str {
    match c {
        Codec::ProRes422Hq => "prores422hq",
        Codec::ProRes422 => "prores422",
        Codec::ProRes4444 => "prores4444",
        Codec::Hevc => "hevc",
    }
}

/// Sizes to choose from (Advanced): name, width × height.
pub const SIZES: [(&str, (usize, usize)); 6] = [
    ("HD", (1920, 1080)),
    ("4K UHD", (3840, 2160)),
    ("DCI 4K", (4096, 2160)),
    ("5K", (5120, 2880)),
    ("6K", (6144, 3456)),
    ("8K UHD", (7680, 4320)),
];

/// The format a new project exports in: 4K HEVC, good to watch and to share.
pub const DEFAULT_PRESET: usize = 1;

pub const RATES: [u32; 5] = [24, 25, 30, 50, 60];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewMode {
    Play,
    Compare,
    Frames,
}

/// The dialog copying settings to other keyframes: which parts (as `sliders::copy_parts`), to which
/// keyframes.
pub struct CopyDialog {
    pub parts: Vec<bool>,
    pub targets: Vec<(usize, bool)>,
    /// The edited keyframe's masks, each to copy or not (by name: it replaces the mask of that
    /// name in the other keyframes, or is added).
    pub masks: Vec<(String, bool)>,
}

/// A keyframe opened in LightCraft, watched for its sidecar to change.
pub struct External {
    pub frame: usize,
    pub sidecar: PathBuf,
    pub modified: Option<std::time::SystemTime>,
    pub checked: Instant,
}

/// A keyframe being edited.
pub struct Edit {
    pub frame: usize,
    pub settings: DevelopSettings,
    pub original: DevelopSettings,
    pub version: u64,
    /// Earlier settings, the last on top; and what was undone.
    pub undo: Vec<DevelopSettings>,
    pub redo: Vec<DevelopSettings>,
    /// The control changed last and when: one drag of a slider is one step to undo.
    last: Option<(String, Instant)>,
    /// The mask whose settings are shown (and whose shape is drawn on the picture).
    pub mask: Option<usize>,
}

/// Most steps kept to undo.
const UNDO_STEPS: usize = 100;

impl Edit {
    #[must_use]
    pub fn new(frame: usize, settings: DevelopSettings, version: u64) -> Self {
        Self {
            frame,
            original: settings.clone(),
            settings,
            version,
            undo: Vec::new(),
            redo: Vec::new(),
            last: None,
            mask: None,
        }
    }

    #[must_use]
    pub fn dirty(&self) -> bool {
        self.settings != self.original
    }

    /// Notes that control `id` is about to change: a new step to undo unless the same control
    /// changed a moment ago (one drag).
    fn before_change(&mut self, id: &str) {
        // A handle's X and Y change together: one drag.
        let key = |id: &str| id.trim_end_matches(['X', 'Y']).to_owned();
        let same = self
            .last
            .as_ref()
            .is_some_and(|(last, at)| key(last) == key(id) && at.elapsed().as_secs_f32() < 0.8);
        if !same {
            self.undo.push(self.settings.clone());
            if self.undo.len() > UNDO_STEPS {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        self.last = Some((id.to_owned(), Instant::now()));
    }

    /// Steps back (`back`) or forward; whether anything changed.
    pub fn step(&mut self, back: bool) -> bool {
        let (from, to) = if back {
            (&mut self.undo, &mut self.redo)
        } else {
            (&mut self.redo, &mut self.undo)
        };
        let Some(s) = from.pop() else { return false };
        to.push(std::mem::replace(&mut self.settings, s));
        self.last = None;
        true
    }
}

pub struct ProjectState {
    pub folder: PathBuf,
    pub name: String,
    pub files: Vec<PathBuf>,
    pub beside_raws: usize,
    pub seq: Option<Arc<Mutex<Sequence>>>,
    pub plan: Option<Arc<DevelopPlan>>,
    pub timing: Option<Timing>,
    pub camera_data: usize,
    /// Where damaged photos were left out: (frame after the gap, photos missing).
    pub skipped_gaps: Vec<(usize, usize)>,
    /// The damaged photos left out, by name (said in the hints and when the export is done).
    pub skipped_names: Vec<String>,
    /// Keyframes made from the suggestions and never saved by hand: they follow the edited ones
    /// (auto transition) whenever a keyframe is saved.
    pub untouched: Vec<(usize, String)>,
    /// Frames with a sidecar of their own (keyframes), as last planned.
    pub sidecar_keyframes: Vec<usize>,
    /// What the edited keyframes' sidecars held when the untouched ones last followed them.
    pub edited_seen: String,
    /// Keyframes whose sidecar can't be read (they develop with the defaults).
    pub unreadable: Vec<usize>,
    /// Keyframes holding settings this version does not know (a newer LightCraft's), with them.
    pub unknown: Vec<(usize, Vec<String>)>,
    /// Keyframes saved by another raw editor beside Lapsewerk's settings (that editor's edit does not show).
    pub other_editor: Vec<usize>,
    /// What happened to a damaged project file on opening (said in step 1 until closed).
    pub project_note: Option<String>,
    /// What the target held when last looked at, for which question (see `render_there`).
    pub there_seen: std::sync::Mutex<Option<(Instant, String, Option<lapsewerk_video::frames::RenderThere>)>>,
    /// The photos' size as last read: kept while an export holds the series, so step 6 does not
    /// lose its notes about it and jump.
    pub source_seen: std::sync::Mutex<Option<(usize, usize)>>,
    /// The last export was refused for room; "start anyway" offered.
    pub space_refused: bool,
    /// The next export skips the room check once.
    pub space_anyway: bool,
    pub analysis: Option<Arc<Analysis>>,
    /// What the analysis found that could spoil the movie.
    pub quality: Option<lapsewerk_studio::Quality>,
    /// The keyframes changed since the analysis: measure again before correcting.
    pub stale: bool,
    pub corrected: Option<Arc<Corrected>>,
    /// The finishing options the corrections were made with.
    pub corrected_with: Option<Finish>,
    pub video_version: u64,
    pub step: usize,
    pub reached: usize,
    pub cursor: usize,
    pub edit: Option<Edit>,
    pub copy: Option<CopyDialog>,
    /// A click on the picture picks the white balance.
    pub picking_wb: bool,
    /// A keyframe opened in LightCraft: its frame, its sidecar, when that changed last, and when it
    /// was checked last (once a second).
    pub external: Option<External>,
    /// Every keyframe's sidecar and its time, looked at every two seconds while the project is
    /// open: LightCraft (or another raw editor) saving several keyframes at once is taken in.
    pub watched: std::collections::HashMap<usize, Option<std::time::SystemTime>>,
    pub watched_at: Instant,
    pub edit_version: u64,
    pub finish: Finish,
    pub mode: PreviewMode,
    pub playing: Option<Instant>,
    pub play_from: usize,
    pub split: f32,
    pub preset: usize,
    pub camera: Camera,
    /// The format was chosen (or kept from before), not left to the default.
    pub preset_chosen: bool,
    pub fps: u32,
    /// The NTSC variant of the rate (×1000/1001: 23.976, 29.97, 59.94).
    pub ntsc: bool,
    /// Size and codec chosen under "Advanced", in place of the preset's.
    pub size: Option<(usize, usize)>,
    pub codec: Option<Codec>,
    /// Rec. 709 (BT.1886) instead of sRGB for SDR; `None`: for ProRes masters, sRGB otherwise.
    pub rec709: Option<bool>,
    pub hdr: bool,
    /// HDR as HLG (broadcast, looks right on SDR screens too) instead of PQ (HDR10).
    pub hlg: bool,
    /// Export a folder of frames instead of a movie.
    pub sequence: bool,
    /// The frames' files (TIFF, DPX, EXR).
    pub frame_format: FrameFormat,
    /// Timecode of the first frame (`HH:MM:SS:FF`, non-drop) for a movie or DPX / EXR frames;
    /// empty for none.
    pub timecode: String,
    /// The frames' name (`--name`) for a frame sequence; empty: as the frames there are named, or
    /// after the folder.
    pub frame_name: String,
    pub out: PathBuf,
    pub exported: Option<PathBuf>,
    /// What the movie exported last delivers ([`Self::movie_delivery`] when it was made).
    pub delivered: Option<String>,
    /// The delivery as checked against its checksums ("Check delivery"): as written, and what differs.
    pub verified: Option<(usize, Vec<String>)>,
    /// The folder [`Self::verified`] was checked in.
    pub verified_dir: Option<PathBuf>,
    /// The last test video made.
    pub proof: Option<PathBuf>,
    /// "Measure result": flicker left frame to frame in the finished look (stops), and the
    /// roughest stretch of 25 frames when it stands out (first frame from 0, its flicker).
    pub measured: Option<(f64, Option<(usize, f64)>)>,
    /// Photos left out of that figure (replaced flashes, the jumps across filled gaps).
    pub measured_left_out: usize,
    /// The place `measured` names is the largest jump between two photos, not a stretch.
    pub measured_jump: bool,
    /// The jump measured is a step between photo `a` and the next (not one photo standing out).
    pub measured_step: bool,
    /// Automatic keyframes whose sidecars are read-only or linked (kept as they are).
    pub untouched_kept: Vec<usize>,
    /// The video size the previews are framed for ([`ProjectState::preview_video`]).
    pub shown_video: (usize, usize),
    /// The first frame's size as the plan develops it, known before the analysis (kept up to
    /// date in `sync_framing`).
    pub developed: Option<(usize, usize)>,
    /// The frame (from 0) the night sky work found its sky in, once looked for.
    pub sky_frame: Option<usize>,
    /// A slow swell or kink over 10-20 frames, as `--verify` names it (frame from 0, stops).
    pub measured_wave: Option<(usize, f64)>,
    /// Where that wave lies (frames from 0, inclusive), as `--verify` names it.
    pub measured_wave_span: Option<(usize, usize)>,
    /// What the slow swell most likely is: the hand-drawn brightness bending there (as drawn),
    /// the deflicker switched off, or something to look at.
    pub measured_wave_why: WaveWhy,
    /// The corrections "Measure result" measured (a fingerprint of their offsets): kept with the
    /// project, so reopening it with the same corrections shows the result again.
    pub measured_for: Option<String>,
    /// Whether the night sky work finds a sky in the first frame (its share), once looked for.
    pub sky_seen: Option<Option<f64>>,
    /// Whether the sky found lies elsewhere in later frames (a panning camera), once looked for.
    pub sky_moving: bool,
    /// Photos measured (a stretch named is kept inside them).
    pub measured_len: usize,
    /// Flicker before and after for a deflicker smoothing (kept while it stays the same).
    pub flicker_estimate: Option<(Option<f64>, (f64, f64))>,
    pub ask_overwrite: bool,
    /// The next export goes on with the frames there (a stopped frame sequence).
    pub resume_next: bool,
}

/// A fingerprint of corrections' offsets (to a millionth of a stop: the same corrections worked
/// out again agree to that).
fn offsets_print(offsets: &[f64]) -> String {
    let text: String = offsets.iter().map(|o| format!("{o:.4},")).collect();
    lapsewerk_video::frames::fingerprint(&text)
}

/// What a slow swell found by "Measure result" most likely is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveWhy {
    /// The hand-drawn brightness bends there: as drawn.
    Drawn,
    /// Deflicker is off in step 4.
    NoDeflicker,
    /// Something to look at there (a keyframe, a ramp's end).
    Look,
}

impl ProjectState {
    #[must_use]
    pub fn frames(&self) -> usize {
        self.files.len()
    }

    /// Keyframes of the plan (frames with sidecars, or frame 0 alone).
    #[must_use]
    pub fn keyframes(&self) -> Vec<usize> {
        // Drawn every frame: while an export holds the sequence, the plan's keyframes.
        self.seq
            .as_ref()
            .and_then(|s| crate::work::try_lock(s).map(|s| s.keyframe_frames()))
            .or_else(|| self.plan.as_ref().map(|p| p.keyframes().to_vec()))
            .unwrap_or_default()
    }

    /// Exposure offsets of the video, per frame (the corrections, else the analysis's).
    #[must_use]
    pub fn offsets(&self) -> Vec<f64> {
        match (&self.corrected, &self.analysis) {
            (Some(c), _) => c.offsets.clone(),
            // Until the corrections are worked out: the deflicker and day to night as set.
            (None, Some(a)) => a.planned_offsets(
                self.finish.deflicker.then_some(f64::from(self.finish.smoothing)),
                self.finish.keep_light.map(f64::from),
                &self.finish.brightness,
            ),
            _ => Vec::new(),
        }
    }

    /// The video format: size, codec, rate (numerator, denominator) and transfer, the preset's
    /// unless chosen otherwise.
    #[must_use]
    pub fn format(&self) -> ((usize, usize), Codec, (u32, u32), Transfer) {
        let (_, preset_codec) = PRESETS.get(self.preset).copied().unwrap_or(PRESETS[0]);
        // The preset's size as it delivers it (turned for upright photos), so that every place
        // that shows or compares the size says what the file will be.
        let (w, h) = self.size.unwrap_or_else(|| self.preset_size(self.preset));
        let codec = self.codec.unwrap_or(preset_codec);
        let rate = if self.ntsc && matches!(self.fps, 24 | 30 | 60) {
            (self.fps * 1000, 1001)
        } else {
            (self.fps, 1)
        };
        let prores = !matches!(codec, Codec::Hevc);
        let transfer = if self.hdr && self.hlg {
            // HLG's whole headroom (its 1000 cd/m²), as the command line has it by default.
            let ev = lapsewerk_core::hdr::Signal::Hlg.max_headroom_ev().min(5.0);
            Transfer::Hlg {
                peak: 2f64.powf(ev) as f32,
            }
        } else if self.hdr {
            Transfer::Pq { peak: 8.0 }
        } else if self
            .rec709
            // Frames go to editing and grading, where Rec. 709 is what is expected (and what DPX
            // has a code for), as for ProRes and as the command line has it.
            .unwrap_or(prores || self.sequence)
        {
            Transfer::Rec709
        } else {
            Transfer::Srgb
        };
        ((w, h), codec, rate, transfer)
    }

    /// The frames in the export folder now, by format (a stopped or an earlier export): those of
    /// the format chosen first.
    #[must_use]
    pub fn frames_there(&self) -> Vec<(FrameFormat, usize)> {
        if !self.sequence {
            return Vec::new();
        }
        let dir = self.target();
        let stem = lapsewerk_video::frames::frame_stem(&dir);
        let mut found: Vec<(FrameFormat, usize)> = [FrameFormat::Tiff, FrameFormat::Dpx, FrameFormat::Exr]
            .into_iter()
            .map(|f| (f, lapsewerk_video::frames::finished_frames_as(&dir, &stem, f)))
            .filter(|(_, n)| *n > 0)
            .collect();
        found.sort_by_key(|(f, _)| *f != self.frame_format);
        found
    }

    /// What the export folder holds of a frame render in the chosen format: how many frames,
    /// whether that render ended, and (once the corrections are worked out) whether it was made
    /// with the settings now, so "go on" would not mix two renders.
    #[must_use]
    pub fn render_there(&self) -> Option<lapsewerk_video::frames::RenderThere> {
        if !self.sequence {
            return None;
        }
        let args = self.render_args(false);
        let manifest = match (&self.plan, &self.corrected, &self.seq) {
            (Some(plan), Some(c), Some(seq))
                if !self.stale && self.corrected_with.as_ref() == Some(&self.finish) =>
            {
                crate::work::try_lock(seq)
                    .map(|s| lapsewerk_studio::export::frames_manifest(&args, plan, &c.offsets, s.len()))
            }
            _ => None,
        };
        // Asked many times a frame (the page, the status bar): looked at once in two seconds for
        // the same question (a series of thousands reads every frame's header).
        let question = format!(
            "{:?} {:?} {:?} {manifest:?}",
            args.out, args.video, self.frame_format
        );
        let mut kept = crate::work::lock(&self.there_seen);
        if let Some((at, q, answer)) = kept.as_ref()
            && *q == question
            && at.elapsed() < std::time::Duration::from_secs(2)
        {
            return answer.clone();
        }
        let mut answer = lapsewerk_video::frames::render_there(
            &args.out,
            &args.video,
            self.frame_format,
            manifest.as_deref(),
        );
        // Another name in the field than the frames there carry: not the same delivery (the export
        // would be refused), said as what differs.
        if let (Some(r), Some(name)) = (answer.as_mut(), self.frame_name_given())
            && r.frames > 0
        {
            let there = lapsewerk_video::frames::frame_stem(&args.out);
            if there != name {
                r.same = Some(false);
                r.why_other = Some(format!("frame name: there {there}, here {name}"));
            }
        }
        *kept = Some((Instant::now(), question, answer.clone()));
        answer
    }

    /// The export that is done: made this session, or found finished in the target with the
    /// settings now (an earlier session's); none while "make anew" is being asked.
    #[must_use]
    pub fn finished_export(&self) -> Option<PathBuf> {
        // A frames' name typed that can't be used: nothing there is this export.
        if self.ask_overwrite
            || (self.sequence && !self.frame_name.trim().is_empty() && self.frame_name_given().is_none())
        {
            return None;
        }
        self.exported.clone().or_else(|| {
            self.render_there()
                .is_some_and(|r| r.complete && r.same == Some(true))
                .then(|| self.target())
        })
    }

    /// Whether an export now goes on with the frames there: a stopped render in this format, not
    /// known to be made with other settings.
    #[must_use]
    pub fn goes_on(&self) -> bool {
        self.can_resume()
            && self.render_there().is_some_and(|r| {
                (!r.complete || !self.verified_damaged().is_empty()) && !r.busy && r.same != Some(false)
            })
    }

    /// `names` (frames damaged or gone) in words: how many no longer match, how many are missing.
    #[must_use]
    pub fn damage_words(&self, t: &crate::text::Texts, names: &[String]) -> String {
        // Missing ones as "Check delivery" found them; the rest no longer match (or are cut short).
        let missing = match &self.verified {
            Some((_, bad)) => names
                .iter()
                .filter(|n| bad.iter().any(|b| *b == format!("{n}: missing")))
                .count(),
            None => 0,
        };
        let changed = names.len().saturating_sub(missing);
        let part = |text: &str, n: usize| text.replace("{n}", &n.to_string());
        match (changed, missing) {
            (c, 0) => part(t.damaged_changed, c),
            (0, m) => part(t.damaged_missing, m),
            (c, m) => format!("{}, {}", part(t.damaged_changed, c), part(t.damaged_missing, m)),
        }
    }

    /// The frames "Check delivery" found changed or missing in the folder it renders into (going
    /// on makes just them again); empty when not checked there.
    #[must_use]
    pub fn verified_damaged(&self) -> Vec<String> {
        match (&self.verified, &self.verified_dir) {
            (Some((_, bad)), Some(dir)) if *dir == self.target() => bad
                .iter()
                .filter(|b| !b.ends_with(lapsewerk_video::frames::EXTRA))
                .filter_map(|b| b.rsplit_once(": ").map(|(name, _)| name.to_owned()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Whether a frame render with these settings can go on where it stopped: not with what
    /// works over a window of neighbouring frames (denoise, blend, star work, in-between frames).
    #[must_use]
    pub fn can_resume(&self) -> bool {
        let a = self.render_args(false);
        a.denoise.is_none()
            && a.blend <= 1
            && a.astro == lapsewerk_studio::stage::AstroParams::default()
            && a.tween == 0
    }

    /// The brightness shaped by hand as it renders: on the frames there, the first of two points
    /// on one frame (what the editor shows, whether it was opened or not).
    #[must_use]
    pub fn brightness_points(&self) -> Vec<(usize, f64)> {
        let mut points = self.finish.brightness.clone();
        if self.frames() >= 2 {
            crate::curve::tidy(&mut points, self.frames());
        }
        points
    }

    /// Whether the frames get the base look (`--base-look`): as long as no keyframe is edited by
    /// hand. An edited keyframe starts from it, so editing never makes the picture jump.
    #[must_use]
    pub fn base_look(&self) -> bool {
        self.sidecar_keyframes
            .iter()
            .all(|k| self.untouched.iter().any(|(f, _)| f == k))
    }

    /// Whether a dark sequence is lifted to a readable brightness (`--auto-brightness`): as long as
    /// no keyframe is edited by hand and no brightness is drawn. An edited look says how bright.
    #[must_use]
    pub fn auto_brightness(&self) -> bool {
        self.brightness_points().is_empty()
            && self
                .sidecar_keyframes
                .iter()
                .all(|k| self.untouched.iter().any(|(f, _)| f == k))
    }

    /// The render arguments for `finish` and the export choices; `preview` leaves out what only
    /// the movie needs (dust, verification).
    #[must_use]
    pub fn render_args(&self, preview: bool) -> RenderArgs {
        let f = &self.finish;
        let ((w, h), codec, (fps_num, fps_den), transfer) = self.format();
        RenderArgs {
            out: self.target(),
            sequence: self.sequence,
            frame_format: self.frame_format,
            resume: false,
            subpixel: f.stabilize && f.subpixel,
            video: VideoSettings {
                width: w,
                height: h,
                fps_num,
                fps_den,
                codec,
                bitrate: None,
                transfer,
                timecode: self.timecode_count().map(|(c, _)| c),
                drop_frame: self.timecode_count().is_some_and(|(_, d)| d),
            },
            // Upright preset: as chosen. Others follow the frames' orientation.
            auto_size: self.preset != 3,
            position: (
                as_written(f64::from(self.camera.x)),
                as_written(f64::from(self.camera.y)),
            ),
            framing: self.camera.framing(),
            tween: f.tween,
            // As the command line reads them back (see `as_written`): the same offsets, bit for bit.
            deflicker: f.deflicker.then_some(as_written(f64::from(f.smoothing))),
            passes: if f.deflicker && f.thorough { 2 } else { 1 },
            regional: if f.regional {
                lapsewerk_core::regional::Grid::new(6, 4)
            } else {
                None
            },
            keep_light: f.keep_light.map(|k| as_written(f64::from(k))),
            hold_blacks: f.hold_blacks,
            brightness: self.brightness_points(),
            auto_brightness: self.auto_brightness(),
            base_look: self.base_look(),
            stabilize: f.stabilize,
            dust: f.dust && !preview,
            blend: if f.smooth_motion { 3 } else { 1 },
            denoise: f.denoise.then_some(lapsewerk_core::temporal::DenoiseParams {
                radius: 2,
                tolerance: 1.0,
            }),
            verify: false,
            astro: lapsewerk_studio::stage::AstroParams {
                light_pollution: f
                    .light_pollution
                    .then(|| lapsewerk_core::deflicker::Rect::new(0.0, 0.0, 1.0, f.sky))
                    .flatten()
                    .map(|r| (r, 1.0)),
                reduce_stars: f.reduce_stars,
                trails: f.trails,
                trails_area: f
                    .trails
                    .and_then(|_| lapsewerk_core::deflicker::Rect::new(0.0, 0.0, 1.0, f.sky)),
            },
            jobs: 2,
            overwrite: true,
            repair: self.repair(),
            replace_outliers: false,
            fill_gaps: false,
        }
    }

    /// The frames to replace (found ones if wanted, and those marked) and the gaps to fill.
    #[must_use]
    pub fn repair(&self) -> lapsewerk_studio::stage::RepairParams {
        let f = &self.finish;
        let q = self.quality.clone().unwrap_or_default();
        let mut replace: Vec<usize> = f.replace_marked.clone();
        if f.replace_outliers {
            replace.extend(&q.outliers);
        }
        replace.sort_unstable();
        replace.dedup();
        lapsewerk_studio::stage::RepairParams {
            replace,
            fill: if f.fill_gaps { q.missing } else { Vec::new() },
        }
    }

    /// The keyframes suggested, none on a photo the export replaces (a flash): moved to the
    /// nearest photo kept (editing the photo replaced edits what is not shown).
    #[must_use]
    pub fn suggestions(&self, a: &lapsewerk_studio::Analysis) -> Vec<usize> {
        let replace = self.repair().replace;
        let n = self.frames();
        let mut out: Vec<usize> = a
            .suggested_keyframes()
            .into_iter()
            .filter_map(|s| {
                (0..n)
                    .flat_map(|d| [s.checked_add(d), s.checked_sub(d)])
                    .flatten()
                    .find(|f| *f < n && !replace.contains(f))
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Where the deflicker measures: the upper or the lower part, or (`None`) the whole frame.
    #[must_use]
    pub fn reference_rect(&self) -> Option<lapsewerk_core::deflicker::Rect> {
        let h = self.finish.reference?;
        let top = if self.finish.reference_low { 1.0 - h } else { 0.0 };
        lapsewerk_core::deflicker::Rect::new(0.0, top, 1.0, h)
    }

    #[must_use]
    pub fn measure_settings(&self) -> MeasureSettings {
        MeasureSettings {
            edge: self.finish.edge,
            reference: self.reference_rect(),
            stabilize: self.finish.stabilize,
            ..Default::default()
        }
    }

    /// The command line that renders exactly what the app would export
    /// (`lapsewerk render FOLDER` + these), kept in `lapsewerk.json` for `--from-project`.
    #[must_use]
    /// Whether the export can carry a timecode: a movie, or DPX / EXR frames (TIFF has none).
    pub fn timecode_fits(&self) -> bool {
        !self.sequence || self.frame_format != FrameFormat::Tiff
    }

    /// The share of the frame the night sky work takes for sky, as the export finds it
    /// ([`lapsewerk_studio::export::find_sky`]: the first frame, else later ones), `None` when none
    /// is found; looked for once, small (the video's shape at 480 pixels wide).
    pub fn sky_found(&mut self) -> Option<f64> {
        if let Some(seen) = self.sky_seen {
            return seen;
        }
        let found = (|| {
            let (seq, plan) = (self.seq.clone()?, self.plan.clone()?);
            let mut args = self.render_args(false);
            let (vw, vh) = (args.video.width.max(1), args.video.height.max(1));
            args.video.width = 480;
            args.video.height = ((480 * vh / vw).max(2) / 2) * 2;
            // The photos' size: as measured, else that of a small render of the first frame.
            let size = self
                .analysis
                .as_ref()
                .and_then(|a| lapsewerk_studio::frame_size(&a.as_shot.sizes).ok())
                .or_else(|| {
                    let job = crate::work::lock(&seq)
                        .render_job(
                            0,
                            plan.settings(0, 0.0),
                            480,
                            480,
                            lapsewerk_develop::Output::Display16(lapsewerk_develop::OutputSpace::Srgb),
                        )
                        .ok()?;
                    let deep = job.run().rendered.ok()?.deep?;
                    Some((deep.width, deep.height))
                })?;
            let offsets = self
                .corrected
                .as_ref()
                .map(|c| c.offsets.clone())
                .unwrap_or_default();
            let found = lapsewerk_studio::export::find_sky_at(
                &mut crate::work::lock(&seq),
                &plan,
                &offsets,
                size,
                &args,
            );
            self.sky_frame = found.as_ref().map(|(_, i)| *i);
            // A sky that moves in the frame (a panning camera): the share above then limits the
            // trails, as the export does.
            self.sky_moving = found.as_ref().is_some_and(|(m, i)| {
                lapsewerk_studio::export::sky_moves(
                    &mut crate::work::lock(&seq),
                    &plan,
                    &offsets,
                    size,
                    &args,
                    m,
                    *i,
                )
            });
            found.map(|(m, _)| m.coverage())
        })();
        self.sky_seen = Some(found);
        found
    }

    /// A name typed other than the frames in the export folder carry: `(there, here)`. The export
    /// would be refused (one folder holds one sequence), so it is said and not offered.
    #[must_use]
    pub fn name_clash(&self) -> Option<(String, String)> {
        let here = self.frame_name_given()?;
        let r = self.render_there()?;
        let there = lapsewerk_video::frames::frame_stem(&self.target());
        (r.frames > 0 && !r.foreign && there != here).then(|| (there, here.to_owned()))
    }

    /// Whether a frames' name is typed that frames can't take (the export stops on it).
    #[must_use]
    pub fn bad_frame_name(&self) -> bool {
        self.sequence && !self.frame_name.trim().is_empty() && self.frame_name_given().is_none()
    }

    /// The frames' name typed, when there is a sound one and the export is a frame sequence.
    pub fn frame_name_given(&self) -> Option<&str> {
        let n = self.frame_name.trim();
        (self.sequence && !n.is_empty() && lapsewerk_video::frames::clean_name(n).is_ok()).then_some(n)
    }

    /// The timecode typed for the first frame, when there is one and the export carries it.
    pub fn timecode_given(&self) -> Option<&str> {
        let tc = self.timecode.trim();
        (!tc.is_empty() && self.timecode_fits()).then_some(tc)
    }

    /// Whether the timecode typed is one at the export's frame rate (an empty one is).
    pub fn timecode_ok(&self) -> bool {
        let (_, _, (num, den), _) = self.format();
        self.timecode_given().is_none_or(|tc| {
            // Drop-frame (;) at 29.97 / 59.94.
            lapsewerk_video::parse_timecode_df(tc, num, den).is_some()
        })
    }

    /// The timecode typed as a frame count, and whether it is drop-frame.
    fn timecode_count(&self) -> Option<(u32, bool)> {
        let (_, _, (num, den), _) = self.format();
        self.timecode_given()
            .and_then(|tc| lapsewerk_video::parse_timecode_df(tc, num, den))
    }

    pub fn cli_args(&self) -> Vec<String> {
        let f = &self.finish;
        let mut a: Vec<String> = Vec::new();
        let mut put = |k: &str, v: Option<String>| {
            a.push(k.to_owned());
            if let Some(v) = v {
                a.push(v);
            }
        };
        let num = |v: f64| {
            let s = format!("{v:.4}");
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        };
        let ((w, h), codec, (fps_num, fps_den), transfer) = self.format();
        put("--out", Some(self.target().to_string_lossy().into_owned()));
        if self.sequence {
            put("--sequence", None);
            if self.frame_format != FrameFormat::Tiff {
                put("--frames", Some(self.frame_format.ext().to_owned()));
            }
        }
        put("--size", Some(format!("{w}x{h}")));
        if self.preset != 3 {
            put("--fit-orientation", None);
        }
        put(
            "--fps",
            Some(if fps_den == 1 {
                fps_num.to_string()
            } else {
                format!("{fps_num}/{fps_den}")
            }),
        );
        // Always said: the command line's own default (Rec. 709 for its default codec, ProRes)
        // need not be the app's, and a frame sequence names no codec.
        match transfer {
            Transfer::Rec709 => put("--transfer", Some("rec709".into())),
            Transfer::Srgb => put("--transfer", Some("srgb".into())),
            _ => {}
        }
        if !self.sequence {
            put("--codec", Some(codec_name(codec).to_owned()));
        }
        // A name typed is passed on even when frames can't take it: the command line refuses it
        // then, as the app does, instead of rendering under another name.
        if self.sequence && !self.frame_name.trim().is_empty() {
            put("--name", Some(self.frame_name.trim().to_owned()));
        }
        if let Some(tc) = self.timecode_given() {
            put("--timecode", Some(tc.to_owned()));
        }
        if self.hdr && self.hlg {
            put("--hdr", Some("hlg".into()));
        } else if self.hdr {
            put("--hdr", Some("pq".into()));
            put("--hdr-peak", Some("3".into()));
        }
        put("--edge", Some(f.edge.to_string()));
        put(
            "--deflicker",
            Some(if f.deflicker {
                num(f64::from(f.smoothing))
            } else {
                "0".into()
            }),
        );
        put(
            "--passes",
            Some(if f.deflicker && f.thorough { "2" } else { "1" }.into()),
        );
        if let Some(r) = self.reference_rect() {
            put(
                "--reference",
                Some(format!(
                    "{},{},{},{}",
                    num(f64::from(r.x)),
                    num(f64::from(r.y)),
                    num(f64::from(r.width)),
                    num(f64::from(r.height))
                )),
            );
        }
        if f.regional {
            put("--regional", Some("6x4".into()));
        }
        if let Some(k) = f.keep_light {
            put("--keep-light", Some(num(f64::from(k))));
        }
        if !f.hold_blacks {
            put("--no-hold-blacks", None);
        }
        if self.auto_brightness() {
            put("--auto-brightness", None);
        }
        if self.base_look() {
            put("--base-look", None);
        }
        let shape = self.brightness_points();
        if !shape.is_empty() {
            let pts: Vec<String> = shape
                .iter()
                .map(|(fr, s)| format!("{}:{}", fr + 1, num(*s)))
                .collect();
            put("--brightness", Some(pts.join(",")));
        }
        if f.stabilize {
            put("--stabilize", None);
            if f.subpixel {
                put("--subpixel", None);
            }
        }
        if f.dust {
            put("--dust", None);
        }
        if f.smooth_motion {
            put("--blend", Some("3".into()));
        }
        if f.tween > 0 {
            put("--tween", Some(f.tween.to_string()));
        }
        if f.denoise {
            put("--denoise", Some("2".into()));
        }
        let night = f.light_pollution || f.trails.is_some();
        if night {
            put("--sky", Some(format!("0,0,1,{}", num(f64::from(f.sky)))));
        }
        if f.light_pollution {
            put("--light-pollution", Some("1".into()));
        }
        if let Some(r) = f.reduce_stars {
            put("--reduce-stars", Some(num(f64::from(r))));
        }
        if let Some(t) = f.trails {
            put("--star-trails", Some(t.to_string()));
        }
        if f.replace_outliers {
            put("--replace-outliers", None);
        }
        if !f.replace_marked.is_empty() {
            let list: Vec<String> = f.replace_marked.iter().map(|i| (i + 1).to_string()).collect();
            put("--replace-frames", Some(list.join(",")));
        }
        if f.fill_gaps {
            put("--fill-gaps", None);
        }
        put(
            "--position",
            Some(format!(
                "{},{}",
                num(f64::from(self.camera.x)),
                num(f64::from(self.camera.y))
            )),
        );
        if let Some(m) = self.camera.framing() {
            let view =
                |v: lapsewerk_core::framing::View| format!("{},{},{}", num(v.x), num(v.y), num(v.zoom));
            put("--move-from", Some(view(m.from)));
            put("--move-to", Some(view(m.to)));
        }
        a
    }

    /// Flicker before and after with the deflicker as set (stops), from the analysis.
    pub fn flicker_estimate(&mut self) -> Option<(f64, f64)> {
        let a = self.analysis.clone()?;
        let smoothing = self.finish.deflicker.then_some(f64::from(self.finish.smoothing));
        if let Some((s, v)) = self.flicker_estimate
            && s == smoothing
        {
            return Some(v);
        }
        let skip = self.repair().replace;
        let v = lapsewerk_studio::predicted_flicker(&a, smoothing, &skip);
        self.flicker_estimate = Some((smoothing, v));
        Some(v)
    }

    /// The export path as `lapsewerk.json` keeps it: a name when beside the photos.
    #[must_use]
    pub fn out_for_file(&self) -> String {
        // Inside the photo folder it is kept relative, so it moves with them.
        self.out.strip_prefix(&self.folder).map_or_else(
            |_| self.out.to_string_lossy().into_owned(),
            |r| r.to_string_lossy().into_owned(),
        )
    }

    /// About how many bytes the export takes (the frames still to make, when resuming).
    #[must_use]
    pub fn space_needed(&self) -> u64 {
        // Frames there are kept (resuming) or written over (replacing): their room is reused
        // either way.
        self.space_needed_resuming(self.sequence)
    }

    /// [`Self::space_needed`], less the frames a stopped render left when it is `resuming`.
    #[must_use]
    pub fn space_needed_resuming(&self, resuming: bool) -> u64 {
        let video = self.render_args(false).video;
        let frames = (self.frames() * (self.finish.tween + 1)) as u64;
        let format = self.sequence.then_some(self.frame_format);
        let target = self.target();
        // Frames made with other settings (another size, an earlier proof) are replaced: they
        // say nothing of this render's frames, only their room comes free as they are written over.
        if self.sequence && self.render_there().is_some_and(|r| r.same == Some(false)) {
            let stem = lapsewerk_video::frames::frame_stem(&target);
            let freed: u64 = (0..frames as usize)
                .map_while(|i| {
                    std::fs::metadata(lapsewerk_video::frames::frame_path_as(
                        &target,
                        &stem,
                        i,
                        self.frame_format,
                    ))
                    .ok()
                    .map(|m| m.len())
                })
                .sum();
            return lapsewerk_video::space::estimate_bytes(&video, format, frames).saturating_sub(freed);
        }
        let there = if self.sequence && resuming {
            let stem = lapsewerk_video::frames::frame_stem(&target);
            lapsewerk_video::frames::finished_frames_as(&target, &stem, self.frame_format) as u64
        } else {
            0
        };
        let left = frames.saturating_sub(there);
        // Frames of this render there already tell their real size (packed TIFF is often far
        // smaller than the estimate): from them, when there are a few.
        if self.sequence {
            let stem = lapsewerk_video::frames::frame_stem(&target);
            let sizes: Vec<u64> = (0..8)
                .filter_map(|i| {
                    std::fs::metadata(lapsewerk_video::frames::frame_path_as(
                        &target,
                        &stem,
                        i,
                        self.frame_format,
                    ))
                    .ok()
                    .map(|m| m.len())
                })
                .collect();
            if sizes.len() >= 3 {
                let largest = sizes.iter().copied().max().unwrap_or(0);
                // A margin: later frames (night, noise) pack less well.
                return left.saturating_mul(largest.saturating_mul(3) / 2);
            }
        }
        lapsewerk_video::space::estimate_bytes(&video, format, left)
    }

    /// Where the export goes: the movie, or (a frame sequence) a folder named like it.
    #[must_use]
    pub fn target(&self) -> PathBuf {
        if self.sequence {
            self.out.with_extension("")
        } else {
            self.out.clone()
        }
    }

    /// How long the video will be, in seconds (in-between frames and filled gaps counted).
    #[must_use]
    pub fn video_seconds(&self) -> f64 {
        self.video_frames() as f64 / f64::from(self.fps.max(1))
    }

    /// The frames of the video: the photos, in-between frames and frames put into gaps.
    #[must_use]
    pub fn video_frames(&self) -> usize {
        self.frames() * (self.finish.tween + 1) + self.gap_frames()
    }

    /// What a movie export delivers, in one line (as the command line's `delivery:` line).
    #[must_use]
    pub fn movie_delivery(&self) -> String {
        let mut r = self.render_args(false);
        if let Some(size) = self.frames_size() {
            lapsewerk_studio::upright(&mut r, size);
        }
        lapsewerk_studio::export::movie_delivery(&r.video, self.video_frames())
    }

    /// A timecode with the sRGB curve: an edit or a broadcast most likely expects Rec. 709.
    #[must_use]
    pub fn timecode_srgb(&self) -> bool {
        self.timecode_count().is_some() && matches!(self.format().3, Transfer::Srgb)
    }

    /// The frames put in for gaps: those found in the analysis, or before it those of photos left out.
    #[must_use]
    pub fn gap_frames(&self) -> usize {
        if self.finish.fill_gaps {
            self.quality.as_ref().map_or_else(
                || self.skipped_gaps.iter().map(|(_, m)| m).sum(),
                |q| q.missing.iter().map(|(_, m)| m).sum(),
            )
        } else {
            0
        }
    }

    /// [`Self::video_seconds`] in words: "1,2 s" or "2:05".
    #[must_use]
    pub fn video_seconds_text(&self, lang: crate::text::Lang) -> String {
        let s = self.video_seconds();
        let length = if s < 60.0 {
            format!("{} s", crate::text::num(lang, s, 1))
        } else {
            crate::text::clock(std::time::Duration::from_secs_f64(s))
        };
        // Frames put in for gaps count: said, or the length looks wrong for the photos.
        match self.gap_frames() {
            0 => length,
            1 => format!("{length} ({})", lang.t().with_gap_frame),
            g => format!(
                "{length} ({})",
                lang.t().with_gap_frames.replace("{n}", &g.to_string())
            ),
        }
    }

    /// Whether the frames are upright (taller than wide), as measured; `None` before the analysis.
    #[must_use]
    pub fn frames_upright(&self) -> Option<bool> {
        self.frames_size().map(|(w, h)| h > w)
    }

    /// The frames' size: as measured, before the analysis as the catalog and the crop give it.
    #[must_use]
    pub fn frames_size(&self) -> Option<(usize, usize)> {
        if let Some(s) = self
            .analysis
            .as_ref()
            .and_then(|a| lapsewerk_studio::frame_size(&a.as_shot.sizes).ok())
        {
            return Some(s);
        }
        self.developed
    }

    /// The video's size as the export makes it (turned upright for upright frames): what the
    /// previews are framed for.
    #[must_use]
    pub fn preview_video(&self) -> (usize, usize) {
        let mut r = self.render_args(true);
        if let Some(size) = self.frames_size() {
            lapsewerk_studio::upright(&mut r, size);
        }
        (r.video.width, r.video.height)
    }

    /// The size preset `i` delivers: for upright photos all turned upright but the last, which
    /// then gives the other shape (landscape 4K) instead of a second upright 4K.
    #[must_use]
    pub fn preset_size(&self, i: usize) -> (usize, usize) {
        let ((w, h), _) = PRESETS.get(i).copied().unwrap_or(PRESETS[0]);
        let swap = if self.frames_upright() == Some(true) {
            (i == 3) == (h > w)
        } else {
            false
        };
        if swap { (h, w) } else { (w, h) }
    }

    /// Whether preset `i` is the landscape one for upright photos (the upright preset turned).
    #[must_use]
    pub fn preset_turned(&self, i: usize) -> bool {
        i == 3 && self.frames_upright() == Some(true)
    }

    /// The video's shape (width / height) for frames that are `portrait` or not.
    #[must_use]
    pub fn video_aspect(&self, portrait: bool) -> f64 {
        let ((w, h), _) = PRESETS.get(self.preset).copied().unwrap_or(PRESETS[0]);
        // As `render_args`: a size chosen follows the frames' orientation unless the last preset
        // is chosen ([`lapsewerk_studio::upright`]); a preset's as [`Self::preset_size`] turns it.
        let swap = match self.size {
            Some((sw, sh)) => self.preset != 3 && portrait && sw > sh,
            None => portrait && (self.preset == 3) == (h > w),
        };
        let (w, h) = self.size.unwrap_or((w, h));
        let (w, h) = if swap { (h, w) } else { (w, h) };
        w as f64 / h.max(1) as f64
    }

    /// The part of a `size` frame the video shows at frame `i`: left, top, width, height in 0..1.
    #[must_use]
    pub fn view_uv(&self, i: usize, size: [f32; 2]) -> [f32; 4] {
        let (w, h) = (f64::from(size[0].max(1.0)), f64::from(size[1].max(1.0)));
        let view = self.camera.view(i, self.frames());
        let win = lapsewerk_core::framing::window(view, w, h, self.video_aspect(h > w));
        [
            (win.left / w) as f32,
            (win.top / h) as f32,
            (win.width / w) as f32,
            (win.height / h) as f32,
        ]
    }

    /// The app's settings for this folder, as kept in `lapsewerk.json`.
    #[must_use]
    pub fn app_settings(&self) -> Value {
        let f = &self.finish;
        json!({
            "reached": self.reached,
            "untouchedKeyframes": self.untouched.iter().map(|(f, fp)| json!([f, fp])).collect::<Vec<_>>(),
            "editedSeen": self.edited_seen,
            "step": self.step,
            "finish": {
                "deflicker": f.deflicker, "smoothing": f.smoothing, "thorough": f.thorough, "keepLight": f.keep_light, "holdBlacks": f.hold_blacks, "brightness": f.brightness,
                "stabilize": f.stabilize, "subpixel": f.subpixel, "dust": f.dust, "smoothMotion": f.smooth_motion, "tween": f.tween,
                "denoise": f.denoise, "lightPollution": f.light_pollution, "reduceStars": f.reduce_stars,
                "trails": f.trails, "sky": f.sky, "reference": f.reference, "referenceLow": f.reference_low, "regional": f.regional,
                "edge": f.edge, "replaceOutliers": f.replace_outliers, "replaceMarked": f.replace_marked,
                "fillGaps": f.fill_gaps,
            },
            "camera": {
                "move": self.camera.kind.name(), "amount": self.camera.amount,
                "x": self.camera.x, "y": self.camera.y,
            },
            "render": self.cli_args(),
            "export": { "preset": self.preset, "fps": self.fps, "hdr": self.hdr, "hlg": self.hlg, "sequence": self.sequence, "frames": self.frame_format.ext(), "ntsc": self.ntsc,
                "size": self.size.map(|(w, h)| [w, h]), "codec": self.codec.map(codec_name),
                "rec709": self.rec709, "timecode": self.timecode, "frameName": self.frame_name, "out": self.out_for_file(), "folderName": self.name, "folderPath": self.folder },
            "measured": self.measured.zip(self.measured_for.as_ref()).map(|((whole, worst), print)| json!({
                "for": print, "whole": whole, "worst": worst.map(|(a, j)| json!([a, j])),
                "jump": self.measured_jump, "step": self.measured_step, "wave": self.measured_wave.map(|(a, v)| json!([a, v])), "waveSpan": self.measured_wave_span.map(|(a, b)| json!([a, b])),
                "why": match self.measured_wave_why { WaveWhy::Drawn => "drawn", WaveWhy::NoDeflicker => "nodefl", WaveWhy::Look => "look" },
                "len": self.measured_len, "leftOut": self.measured_left_out,
            })),
        })
    }

    fn apply_app_settings(&mut self, v: &Value) {
        let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64);
        // The last "Measure result", for the corrections it measured (shown again when the same
        // ones come back).
        if let Some(m) = v.get("measured").filter(|m| m.is_object()) {
            let pair = |k: &str| {
                let a = m.get(k)?.as_array()?;
                Some((usize::try_from(a.first()?.as_u64()?).ok()?, a.get(1)?.as_f64()?))
            };
            if let (Some(print), Some(whole)) = (m.get("for").and_then(Value::as_str), num(m, "whole")) {
                self.measured = Some((whole, pair("worst")));
                self.measured_for = Some(print.to_owned());
                self.measured_jump = m.get("jump").and_then(Value::as_bool).unwrap_or(false);
                self.measured_step = m.get("step").and_then(Value::as_bool).unwrap_or(false);
                self.measured_wave = pair("wave");
                self.measured_wave_span = m.get("waveSpan").and_then(Value::as_array).and_then(|a| {
                    Some((
                        usize::try_from(a.first()?.as_u64()?).ok()?,
                        usize::try_from(a.get(1)?.as_u64()?).ok()?,
                    ))
                });
                self.measured_wave_why = match m.get("why").and_then(Value::as_str) {
                    Some("drawn") => WaveWhy::Drawn,
                    Some("nodefl") => WaveWhy::NoDeflicker,
                    _ => WaveWhy::Look,
                };
                self.measured_len = m.get("len").and_then(Value::as_u64).map_or(0, |n| n as usize);
                self.measured_left_out = m.get("leftOut").and_then(Value::as_u64).map_or(0, |n| n as usize);
            }
        }
        let flag = |v: &Value, k: &str| v.get(k).and_then(Value::as_bool);
        if let Some(r) = v.get("reached").and_then(Value::as_u64) {
            self.reached = (r as usize).clamp(1, STEPS);
        }
        self.untouched = lapsewerk_studio::cache::untouched_in(v);
        // Shared with the command line: what the edited keyframes held when the others followed.
        self.edited_seen = v
            .get("editedSeen")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Some(f) = v.get("finish") {
            let d = &mut self.finish;
            d.deflicker = flag(f, "deflicker").unwrap_or(d.deflicker);
            d.thorough = flag(f, "thorough").unwrap_or(d.thorough);
            d.smoothing = num(f, "smoothing")
                .filter(|v| v.is_finite())
                .map_or(8.0, |v| v.clamp(2.0, 30.0) as f32);
            d.keep_light = num(f, "keepLight").map(|x| x.clamp(0.0, 1.0) as f32);
            d.hold_blacks = flag(f, "holdBlacks").unwrap_or(true);
            d.brightness = f
                .get("brightness")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|p| {
                            let p = p.as_array()?;
                            let f = p.first()?.as_u64()? as usize;
                            let s = p.get(1)?.as_f64().filter(|s| s.is_finite())?.clamp(-6.0, 6.0);
                            Some((f, s))
                        })
                        .take(64)
                        .collect()
                })
                .unwrap_or_default();
            d.stabilize = flag(f, "stabilize").unwrap_or(false);
            d.subpixel = flag(f, "subpixel").unwrap_or(false);
            d.dust = flag(f, "dust").unwrap_or(false);
            d.smooth_motion = flag(f, "smoothMotion").unwrap_or(false);
            d.tween = f
                .get("tween")
                .and_then(Value::as_u64)
                .map_or(0, |n| (n as usize).min(3));
            d.denoise = flag(f, "denoise").unwrap_or(false);
            d.light_pollution = flag(f, "lightPollution").unwrap_or(false);
            d.reduce_stars = num(f, "reduceStars").map(|x| x.clamp(0.0, 1.0) as f32);
            d.trails = f
                .get("trails")
                .and_then(Value::as_u64)
                .map(|t| t.min(10_000) as usize);
            d.sky = num(f, "sky").map_or(d.sky, |x| x.clamp(0.1, 1.0) as f32);
            d.reference = num(f, "reference").map(|x| x.clamp(0.1, 1.0) as f32);
            d.reference_low = flag(f, "referenceLow").unwrap_or(false);
            d.regional = flag(f, "regional").unwrap_or(false);
            d.edge = f
                .get("edge")
                .and_then(Value::as_u64)
                .map_or(d.edge, |e| (e as usize).clamp(256, 2048));
            d.replace_outliers = flag(f, "replaceOutliers").unwrap_or(true);
            d.fill_gaps = flag(f, "fillGaps").unwrap_or(true);
            d.replace_marked = f
                .get("replaceMarked")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_u64)
                        .map(|v| v as usize)
                        .take(10_000)
                        .collect()
                })
                .unwrap_or_default();
        }
        if let Some(c) = v.get("camera") {
            let unit = |k: &str, d: f32| {
                num(c, k)
                    .filter(|v| v.is_finite())
                    .map_or(d, |v| v.clamp(0.0, 1.0) as f32)
            };
            let name = c.get("move").and_then(Value::as_str).unwrap_or("");
            self.camera = Camera {
                kind: CameraMove::ALL
                    .into_iter()
                    .find(|m| m.name() == name)
                    .unwrap_or(CameraMove::Still),
                amount: unit("amount", 0.3).max(0.05),
                x: unit("x", 0.5),
                y: unit("y", 0.5),
            };
        }
        if let Some(e) = v.get("export") {
            self.preset_chosen = e.get("preset").is_some();
            self.preset = e
                .get("preset")
                .and_then(Value::as_u64)
                .map_or(DEFAULT_PRESET, |p| (p as usize).min(PRESETS.len() - 1));
            self.fps = e
                .get("fps")
                .and_then(Value::as_u64)
                .map_or(25, |f| f as u32)
                .clamp(1, 120);
            self.hdr = flag(e, "hdr").unwrap_or(false);
            self.hlg = flag(e, "hlg").unwrap_or(false);
            self.sequence = flag(e, "sequence").unwrap_or(self.sequence);
            self.frame_format = e
                .get("frames")
                .and_then(Value::as_str)
                .and_then(FrameFormat::parse)
                .unwrap_or_default();
            self.ntsc = flag(e, "ntsc").unwrap_or(false);
            self.size = e.get("size").and_then(Value::as_array).and_then(|a| {
                let w = a.first()?.as_u64()? as usize;
                let h = a.get(1)?.as_u64()? as usize;
                SIZES.iter().map(|(_, s)| *s).find(|s| *s == (w, h))
            });
            self.codec = e
                .get("codec")
                .and_then(Value::as_str)
                .and_then(|n| CODECS.into_iter().find(|c| codec_name(*c) == n));
            self.rec709 = flag(e, "rec709");
            self.timecode = e
                .get("timecode")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            self.frame_name = e
                .get("frameName")
                .and_then(Value::as_str)
                // Kept as written (shown under "Advanced" as one frames can't take, and the export
                // stops on it), not cleared silently into exporting under another name.
                .map(|n| n.trim().chars().take(200).collect())
                .unwrap_or_default();
            // Beside the photos it is kept as a name, so it moves with them. A path elsewhere is
            // kept unless the photos moved or were copied (it pointed at the old folder: one of
            // the same name, or one that is gone), so a copy never exports into the original.
            if let Some(o) = e.get("out").and_then(Value::as_str).filter(|o| !o.is_empty()) {
                let o = PathBuf::from(o);
                // A bare name (older projects kept the export beside the photos) moves into the
                // export folder.
                let o = if o.components().count() == 1 && o.is_relative() {
                    self.folder.join(EXPORT_DIR).join(o)
                } else if o.is_relative() {
                    self.folder.join(o)
                } else {
                    o
                };
                let parent = o.parent().map(Path::to_path_buf);
                // The photo folder it belongs to: the export folder's parent for ours.
                let home = parent.as_deref().map(|d| {
                    if d.file_name().is_some_and(|n| n == EXPORT_DIR) {
                        d.parent().unwrap_or(d)
                    } else {
                        d
                    }
                });
                let old_home =
                    home.is_some_and(|d| d != self.folder && d.file_name() == self.folder.file_name());
                let ours = parent.as_deref() == Some(self.folder.join(EXPORT_DIR).as_path());
                // Where the project was when this was saved (newer project files): a path inside
                // it moves with the photos; one outside it is kept even when it is not there now
                // (a delivery drive not plugged in: the export page says so, nothing is made in
                // its place).
                let saved_home = e.get("folderPath").and_then(Value::as_str).map(PathBuf::from);
                let outside = saved_home
                    .as_ref()
                    .is_some_and(|h| !o.starts_with(h) && !o.starts_with(&self.folder));
                if outside {
                    self.out = o;
                } else if (ours || parent.as_deref().is_some_and(Path::is_dir)) && !old_home {
                    // A copied or renamed photo folder: the export named after the old folder
                    // takes the new name (a name chosen by hand stays).
                    let saved_name = e.get("folderName").and_then(Value::as_str);
                    let o = match (saved_name, o.file_stem().and_then(|s| s.to_str())) {
                        (Some(old), Some(stem)) if old != self.name && stem == old => {
                            let ext = o.extension().map(|x| format!(".{}", x.to_string_lossy()));
                            o.with_file_name(format!("{}{}", self.name, ext.unwrap_or_default()))
                        }
                        _ => o,
                    };
                    self.out = o;
                }
            }
        }
    }
}

/// Which task is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskKind {
    Open,
    Analyse,
    Correct,
    Export,
    /// A small, quick test video, opened when done.
    Proof,
    /// A camera profile from the user's photos.
    Calibrate,
    /// A delivery checked against its checksums.
    Verify,
    /// The corrections worked out once more and the finished look measured ("Measure result").
    MeasureResult,
}

/// A message at the bottom: text, is it an error, since when.
pub struct Message {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

pub struct App {
    pub lang: Lang,
    /// Exports waiting to run one after another (the command line, in the background).
    pub queue: crate::queue::Queue,
    /// The command line beside the app, which the queue runs.
    pub cli: Option<PathBuf>,
    /// Closing was asked while the queue renders: the question is shown.
    pub ask_close: bool,
    /// Close although the queue renders (answered).
    pub close_anyway: bool,
    pub recent: Recent,
    pub project: Option<ProjectState>,
    pub previews: Previews,
    pub task: Option<(TaskKind, Task)>,
    pub message: Option<Message>,
    pub thumbs: crate::start::Thumbs,
    /// Headless run (screenshots): work on to this step without anyone clicking.
    pub autopilot: Option<usize>,
    autopilot_edited: bool,
    /// Headless run: make the video at step 6.
    pub autopilot_export: bool,
    /// Snapshot: "Check delivery" in step 6.
    pub autopilot_verify: bool,
    /// Snapshot: "Measure result" in step 5.
    pub autopilot_measure: bool,
    /// Put the export in the queue at step 6 (screenshots, tests).
    pub autopilot_queue: bool,
    /// Make a test video once at step 5.
    pub autopilot_proof: bool,
    /// Open the copy dialog once at step 3.
    pub autopilot_copy: bool,
    /// Paint a brush mask at step 3 (screenshots).
    pub autopilot_brush: bool,
    /// Step 3 shows an edit in progress (screenshots); off, a fresh project stays untouched.
    pub autopilot_demo: bool,
    /// "Advanced" open from the start (screenshots).
    pub open_advanced: bool,
    /// The introduction's slide while it is open.
    pub intro: Option<usize>,
    /// The help panel is open.
    pub help: bool,
    /// The introduction was offered this run.
    intro_offered: bool,
    /// The brush, and the stroke being painted.
    pub brush: crate::masks_ui::BrushTool,
    pub painting: Vec<(f64, f64)>,
    /// The program that opens a keyframe in LightCraft (`LAPSEWERK_LIGHTCRAFT`), else the app.
    pub lightcraft: Option<std::ffi::OsString>,
    /// LightCraft is there to open keyframes in (else the button is not shown).
    pub lightcraft_found: bool,
    /// Headless run: the preview mode of step 5.
    pub autopilot_mode: Option<PreviewMode>,
    pub ctx: egui::Context,
}

impl App {
    #[must_use]
    pub fn new(ctx: &egui::Context) -> Self {
        Self::with_recent(ctx, Recent::load())
    }

    /// The app with this list of recent projects (tests: one kept apart from the user's).
    #[must_use]
    pub fn with_recent(ctx: &egui::Context, recent: Recent) -> Self {
        crate::theme::install(ctx);
        let lang = recent
            .lang
            .as_deref()
            .and_then(Lang::from_code)
            .unwrap_or_else(Lang::from_system);
        // The exports not done when the app last closed: back, paused or stopped.
        let queue = crate::queue::Queue::load_at(recent.dir());
        let damaged = queue.damaged.clone();
        let mut app = Self {
            lang,
            recent,
            project: None,
            previews: Previews::new(ctx),
            task: None,
            message: None,
            thumbs: crate::start::Thumbs::default(),
            autopilot: None,
            autopilot_edited: false,
            autopilot_export: false,
            autopilot_verify: false,
            autopilot_measure: false,
            autopilot_queue: false,
            autopilot_proof: false,
            autopilot_copy: false,
            autopilot_brush: false,
            autopilot_demo: false,
            open_advanced: false,
            queue,
            cli: crate::queue::cli_path(),
            ask_close: false,
            close_anyway: false,
            intro: None,
            help: false,
            intro_offered: false,
            brush: crate::masks_ui::BrushTool::default(),
            painting: Vec::new(),
            lightcraft: std::env::var_os("LAPSEWERK_LIGHTCRAFT").filter(|v| !v.is_empty()),
            lightcraft_found: lightcraft_installed(),
            autopilot_mode: None,
            ctx: ctx.clone(),
        };
        // The queue from last time could not be read: said, with where it was put.
        if let Some(aside) = damaged {
            let text = app
                .lang
                .t()
                .queue_damaged
                .replace("{file}", &aside.display().to_string());
            app.say(text, true);
        }
        app
    }

    pub fn set_lang(&mut self, lang: Lang) {
        self.lang = lang;
        self.recent.lang = Some(lang.code().to_owned());
        self.recent.save();
    }

    pub fn say(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        // An error from below (the system, LightCraft, the command line) in German where known.
        let text = if error {
            crate::text::reason(self.lang, &text)
        } else {
            text
        };
        self.message = Some(Message {
            text,
            error,
            at: Instant::now(),
        });
    }

    /// Makes the folder `out` goes into when it is inside the photo folder (the default export
    /// folder); one elsewhere must be there already: a missing drive is reported, not replaced by
    /// an empty folder at its mount point.
    fn make_target_folder(folder: &Path, out: &Path, t: &crate::text::Texts) -> Result<(), String> {
        let Some(d) = out.parent().filter(|d| !d.as_os_str().is_empty()) else {
            return Ok(());
        };
        if d.is_dir() {
            return Ok(());
        }
        if !d.starts_with(folder) {
            return Err(t.target_missing.replace("{folder}", &d.display().to_string()));
        }
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))
    }

    /// Puts the open project's export in the queue, with its settings as they are now.
    pub fn queue_export(&mut self) {
        let Some(p) = &self.project else { return };
        if !p.timecode_ok() {
            let t = self.lang.t();
            self.say(t.timecode_bad.to_owned(), true);
            return;
        }
        if p.sequence && !p.frame_name.trim().is_empty() && p.frame_name_given().is_none() {
            self.say(self.lang.t().frame_name_refused.to_owned(), true);
            return;
        }
        // Another name than the frames there: refused now, not after a night in the queue.
        if let Some((there, here)) = p.name_clash() {
            let t = self.lang.t();
            self.say(
                t.target_name_clash
                    .replace("{there}", &there)
                    .replace("{here}", &here),
                true,
            );
            return;
        }
        let out = p.target();
        if self.queue.has_target(&out) {
            let t = self.lang.t();
            self.say(t.queue_twice, true);
            return;
        }
        let photos: Vec<PathBuf> = p
            .seq
            .as_ref()
            .map(|s| lock(s).frames().iter().map(|f| f.path.clone()).collect())
            .unwrap_or_default();
        if let Err(e) = lapsewerk_studio::check_target(&photos, &out, p.sequence) {
            self.say(crate::text::reason(self.lang, &e), true);
            return;
        }
        if let Err(e) = Self::make_target_folder(&p.folder, &out, self.lang.t()) {
            self.say(e, true);
            return;
        }
        let Some(p) = &self.project else { return };
        let mut args = p.cli_args();
        // Said on the button: a stopped render with these settings is gone on with (a stopped
        // night's render is not lost); a finished one, one with other settings or in another
        // format, and an existing movie, are replaced.
        if out.exists() {
            args.push(if p.goes_on() { "--resume" } else { "--overwrite" }.into());
        }
        let (name, folder) = (p.name.clone(), p.folder.clone());
        self.save_project_settings();
        self.queue.add(name.clone(), folder, out, args);
        let t = self.lang.t();
        self.say(t.queue_added.replace("{name}", &name), false);
    }

    /// Nothing running and nothing waiting (headless screenshots wait for this).
    #[must_use]
    pub fn idle(&self) -> bool {
        self.task.is_none() && !self.previews.busy() && !self.thumbs.busy()
    }

    /// Opens the folder as the current project: its frames listed now, opened on a task.
    pub fn open_folder(&mut self, folder: &Path) {
        // Absolute (a folder given relative to where the app started): the export target and the
        // project file's paths then hold from anywhere, the command line's --from-project too.
        let absolute = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
        // A photo chosen or dropped instead of its folder: the folder it is in.
        let absolute = if absolute.is_file() {
            absolute.parent().map_or(absolute.clone(), Path::to_path_buf)
        } else {
            absolute
        };
        let folder = absolute.as_path();
        let t = self.lang.t();
        let frames = match lapsewerk_studio::frame_files(folder) {
            Ok(f) => f,
            Err(_) if folder.is_dir() => {
                // The folder's name (a whole path runs off the window).
                let name = folder.file_name().map_or_else(
                    || folder.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                self.say(t.no_photos.replace("{folder}", &name), true);
                return;
            }
            Err(e) => {
                self.say(format!("{}: {e}", t.error), true);
                return;
            }
        };
        if frames.files.len() < 2 {
            self.say(t.one_photo, true);
            return;
        }
        let name = folder.file_name().map_or_else(
            || folder.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let mut p = ProjectState {
            folder: folder.to_path_buf(),
            out: folder.join(EXPORT_DIR).join(format!("{name}.mov")),
            name,
            files: frames.files,
            beside_raws: frames.beside_raws,
            seq: None,
            plan: None,
            timing: None,
            camera_data: 0,
            skipped_gaps: Vec::new(),
            skipped_names: Vec::new(),
            untouched: Vec::new(),
            edited_seen: String::new(),
            sidecar_keyframes: Vec::new(),
            unreadable: Vec::new(),
            unknown: Vec::new(),
            other_editor: Vec::new(),
            project_note: None,
            there_seen: std::sync::Mutex::new(None),
            source_seen: std::sync::Mutex::new(None),
            space_refused: false,
            space_anyway: false,
            analysis: None,
            quality: None,
            stale: false,
            corrected: None,
            corrected_with: None,
            video_version: 0,
            step: 1,
            reached: 1,
            cursor: 0,
            edit: None,
            copy: None,
            picking_wb: false,
            external: None,
            watched: std::collections::HashMap::new(),
            watched_at: Instant::now(),
            edit_version: 0,
            finish: Finish::default(),
            mode: PreviewMode::Play,
            playing: None,
            play_from: 0,
            split: 0.5,
            preset: DEFAULT_PRESET,
            camera: Camera::default(),
            preset_chosen: false,
            ntsc: false,
            size: None,
            codec: None,
            rec709: None,
            fps: 25,
            hdr: false,
            hlg: false,
            sequence: !cfg!(target_os = "macos"),
            frame_format: FrameFormat::Tiff,
            timecode: String::new(),
            frame_name: String::new(),
            exported: None,
            delivered: None,
            verified: None,
            verified_dir: None,
            proof: None,
            measured: None,
            measured_left_out: 0,
            measured_jump: false,
            measured_step: false,
            untouched_kept: Vec::new(),
            shown_video: (0, 0),
            developed: None,
            sky_frame: None,
            measured_wave: None,
            measured_wave_span: None,
            measured_wave_why: WaveWhy::Look,
            measured_for: None,
            sky_seen: None,
            sky_moving: false,
            measured_len: 0,
            flicker_estimate: None,
            ask_overwrite: false,
            resume_next: false,
        };
        // A damaged project file is kept beside (renamed), never written over; one from a newer
        // Lapsewerk is left as it is (and not saved over).
        let mut damaged = None;
        match ProjectFile::load(folder) {
            Ok(Some(file)) => {
                if let Some(app) = &file.app {
                    p.apply_app_settings(app);
                }
            }
            Ok(None) => {}
            Err(e) if e.to_string().contains("this version reads") => {
                damaged = Some(self.lang.t().project_newer.to_owned());
            }
            Err(_) => {
                let from = folder.join(lapsewerk_develop::project::FILE_NAME);
                // Never over an earlier damaged copy (it may hold more).
                let name = lapsewerk_develop::project::FILE_NAME;
                let to = (1..100)
                    .map(|k| {
                        folder.join(if k == 1 {
                            format!("{name}.damaged")
                        } else {
                            format!("{name}.damaged-{k}")
                        })
                    })
                    .find(|p| !p.exists())
                    .unwrap_or_else(|| folder.join(format!("{name}.damaged-last")));
                damaged = Some(if std::fs::rename(&from, &to).is_ok() {
                    self.lang.t().project_damaged.to_owned()
                } else {
                    self.lang.t().project_newer.to_owned()
                });
            }
        }
        // Coming back to a project: the analysis is measured again, so start at most at step 2.
        p.reached = p.reached.min(2);
        self.recent.touch(folder, p.frames(), p.reached);
        self.recent.save();
        self.previews.set_source(None);
        self.task = None;
        let files = p.files.clone();
        let folder_owned = folder.to_path_buf();
        self.project = Some(p);
        self.message = None;
        if let Some(text) = damaged {
            if let Some(p) = &mut self.project {
                p.project_note = Some(text.clone());
            }
            self.say(text, true);
        }
        let task = Task::spawn(&self.ctx, move |rep| {
            let opened = lapsewerk_studio::open(&files, &OpenSettings::default(), rep);
            match opened {
                Ok((seq, plan)) => {
                    let timing = lapsewerk_core::sequence::timing(seq.frames());
                    // The project file: what the import learned, the app's settings kept.
                    // Only over a file read (or none): one that can't be read stays as it is.
                    if let Ok(old) = ProjectFile::load(&folder_owned)
                        && let Ok(mut file) = ProjectFile::of(seq.frames(), &seq.keyframe_frames())
                    {
                        file.app = old.and_then(|old| old.app);
                        let _ = file.save(&folder_owned);
                    }
                    Outcome::Opened(Box::new(crate::work::Opened { seq, plan, timing }))
                }
                Err(e) => Outcome::Failed(e),
            }
        });
        self.task = Some((TaskKind::Open, task));
    }

    /// Saves the app's settings into the project file (beside the photos).
    pub fn save_project_settings(&self) {
        let Some(p) = &self.project else { return };
        if let Ok(Some(mut file)) = ProjectFile::load(&p.folder) {
            file.app = Some(p.app_settings());
            let _ = file.save(&p.folder);
        }
    }

    pub fn close_project(&mut self) {
        self.save_project_settings();
        self.task = None;
        self.previews.set_source(None);
        self.project = None;
    }

    /// Goes to step `step` (if reached), saving the settings.
    pub fn go(&mut self, step: usize) {
        // Leaving the keyframe editor with changes: they are saved, not lost.
        if step != 3
            && self
                .project
                .as_ref()
                .is_some_and(|p| p.step == 3 && p.edit.as_ref().is_some_and(Edit::dirty))
        {
            self.save_edit();
        }
        let Some(p) = &mut self.project else { return };
        let step = step.clamp(1, STEPS);
        if step > p.reached {
            return;
        }
        p.step = step;
        p.playing = None;
        let (folder, frames, reached) = (p.folder.clone(), p.frames(), p.reached);
        self.recent.touch(&folder, frames, reached);
        self.recent.save();
        self.save_project_settings();
        self.on_enter();
    }

    /// Unlocks and goes to the next step.
    pub fn next(&mut self) {
        let Some(p) = &mut self.project else { return };
        let next = (p.step + 1).min(STEPS);
        p.reached = p.reached.max(next);
        self.go(next);
    }

    /// Work a step starts by itself.
    fn on_enter(&mut self) {
        let Some(p) = &self.project else { return };
        if self.task.is_some() {
            return;
        }
        let needs_analysis = p.analysis.is_none() || p.stale;
        match p.step {
            2 | 3 if needs_analysis && p.seq.is_some() => self.start_analysis(),
            5 if p.seq.is_some()
                && (needs_analysis
                    || p.corrected.is_none()
                    || p.corrected_with.as_ref() != Some(&p.finish)) =>
            {
                self.start_correct();
            }
            _ => {}
        }
    }

    pub fn start_analysis(&mut self) {
        let Some(p) = &mut self.project else { return };
        let (Some(seq), Some(plan)) = (p.seq.clone(), p.plan.clone()) else {
            return;
        };
        p.corrected = None;
        let opts = p.measure_settings();
        let folder = p.folder.clone();
        let task = Task::spawn(&self.ctx, move |rep| {
            let mut seq = lock(&seq);
            match lapsewerk_studio::cache::analyse_cached(&mut seq, &plan, &opts, rep, &folder) {
                Ok(a) => Outcome::Analysed(Box::new(a)),
                Err(e) => Outcome::Failed(e),
            }
        });
        self.task = Some((TaskKind::Analyse, task));
    }

    /// Corrections for the preview (analysing first when the keyframes changed).
    pub fn start_correct(&mut self) {
        let Some(p) = &mut self.project else { return };
        let (Some(seq), Some(plan)) = (p.seq.clone(), p.plan.clone()) else {
            return;
        };
        let analysis = if p.stale { None } else { p.analysis.clone() };
        let opts = p.measure_settings();
        let args = p.render_args(true);
        p.corrected_with = Some(p.finish.clone());
        let folder = p.folder.clone();
        let task = Task::spawn(&self.ctx, move |rep| {
            let mut seq = lock(&seq);
            let a = match analysis {
                Some(a) => a,
                None => match lapsewerk_studio::cache::analyse_cached(&mut seq, &plan, &opts, rep, &folder) {
                    Ok(a) => Arc::new(a),
                    Err(e) => return Outcome::Failed(e),
                },
            };
            match lapsewerk_studio::cache::correct_cached(&mut seq, &plan, &a, &opts, &args, rep, &folder) {
                Ok(c) => Outcome::Corrected(Box::new(c)),
                Err(e) => Outcome::Failed(e),
            }
        });
        self.task = Some((TaskKind::Correct, task));
    }

    /// "Measure result": the corrections worked out once more with the finished look measured
    /// (as `--verify` does), to say how much flicker is left and where.
    pub fn start_measure_result(&mut self) {
        let Some(p) = &mut self.project else { return };
        let (Some(seq), Some(plan), Some(a)) = (p.seq.clone(), p.plan.clone(), p.analysis.clone()) else {
            return;
        };
        p.measured = None;
        let opts = p.measure_settings();
        let mut args = p.render_args(true);
        args.verify = true;
        let task = Task::spawn(&self.ctx, move |rep| {
            let mut seq = lock(&seq);
            match lapsewerk_studio::correct(&mut seq, &plan, &a, &opts, &args, rep) {
                Ok(c) => Outcome::Corrected(Box::new(c)),
                Err(e) => Outcome::Failed(e),
            }
        });
        self.task = Some((TaskKind::MeasureResult, task));
    }

    /// Renders the movie, or (`proof`) a small, quick test video beside the photos that opens when
    /// done. The preview's corrections are used when they were made with these settings.
    pub fn start_export(&mut self, proof: bool) {
        let correcting = matches!(self.task, Some((TaskKind::Correct, _)));
        let Some(p) = &mut self.project else { return };
        let (Some(seq), Some(plan)) = (p.seq.clone(), p.plan.clone()) else {
            return;
        };
        if !proof && !p.timecode_ok() {
            let t = self.lang.t();
            let why = if p.timecode.contains(';') {
                t.timecode_drop
            } else {
                t.timecode_bad
            };
            self.say(why.to_owned(), true);
            return;
        }
        // A name typed that frames can't take: said, not exported under another name.
        if !proof && p.sequence && !p.frame_name.trim().is_empty() && p.frame_name_given().is_none() {
            self.say(self.lang.t().frame_name_refused.to_owned(), true);
            return;
        }
        let mut args = p.render_args(false);
        if !proof && self.queue.has_target(&args.out) {
            let t = self.lang.t();
            self.say(t.target_queued.to_owned(), true);
            return;
        }
        if !proof && std::mem::take(&mut p.resume_next) {
            args.resume = true;
            args.overwrite = false;
        }
        if proof {
            // A movie, or (frame sequences) a folder of frames to look through.
            let name = format!("{} – {}", p.name, self.lang.t().proof_name);
            args.out = if args.sequence {
                p.folder.join(EXPORT_DIR).join(name)
            } else {
                p.folder.join(EXPORT_DIR).join(format!("{name}.mov"))
            };
            let upright = args.video.height > args.video.width;
            (args.video.width, args.video.height) = if upright { (540, 960) } else { (960, 540) };
            args.video.codec = Codec::Hevc;
            args.video.transfer = Transfer::Srgb;
            args.dust = false;
            args.overwrite = true;
        }
        // Never among the photos (their folder, one of their names).
        let photos: Vec<PathBuf> = lock(&seq).frames().iter().map(|f| f.path.clone()).collect();
        if let Err(e) = lapsewerk_studio::check_target(&photos, &args.out, args.sequence) {
            self.say(crate::text::reason(self.lang, &e), true);
            return;
        }
        // The default place (a folder beside the photos) may not exist yet; a folder elsewhere
        // that is missing (a drive not plugged in) is not made in its place.
        let made = args.out.parent().filter(|d| !d.is_dir()).map(Path::to_path_buf);
        if let Err(e) = Self::make_target_folder(&p.folder, &args.out, self.lang.t()) {
            self.say(e, true);
            return;
        }
        // Room for it (the test video is small): the size estimated against the free space.
        // "Start anyway" (the estimate is an upper bound: TIFF frames are packed) skips it once.
        let anyway = std::mem::take(&mut p.space_anyway);
        p.space_refused = false;
        if !proof && !anyway {
            let need = p.space_needed();
            if let Err(short) = lapsewerk_video::space::check(&args.out, need) {
                p.space_refused = true;
                // Nothing started: the folder just made for it goes again (only while empty).
                if let Some(d) = &made {
                    let _ = std::fs::remove_dir(d);
                }
                let t = self.lang.t();
                let tip = if args.video.width.max(args.video.height) > 1920 {
                    t.space_tip_4k
                } else {
                    ""
                };
                self.say(
                    t.space_short
                        .replace("{need}", &crate::text::bytes(self.lang, short.needed))
                        .replace("{free}", &crate::text::bytes(self.lang, short.free))
                        .replace("{tip}", tip),
                    true,
                );
                return;
            }
        }
        let checked = if args.sequence {
            lapsewerk_video::frames::preflight_named(
                &args.out,
                args.frame_format,
                args.overwrite,
                args.resume,
                matches!(args.video.transfer, Transfer::Pq { .. }),
                p.frame_name_given(),
            )
            .and_then(|()| {
                if args.resume {
                    lapsewerk_video::frames::resume_preflight(
                        &args.out,
                        args.frame_format,
                        &lapsewerk_studio::export::early_settings_words(&args),
                    )
                } else {
                    Ok(())
                }
            })
        } else {
            lapsewerk_video::preflight(&args.out, args.video, true)
        };
        if let Err(e) = checked {
            let text = match e {
                lapsewerk_video::Error::Helper(_) => self.lang.t().needs_mac.to_owned(),
                e => crate::text::reason(self.lang, &e.to_string()),
            };
            self.say(text, true);
            return;
        }
        let analysis = if p.stale { None } else { p.analysis.clone() };
        let ready = (!p.stale && !correcting && p.corrected_with.as_ref() == Some(&p.finish))
            .then(|| p.corrected.clone())
            .flatten();
        let opts = p.measure_settings();
        if !proof {
            p.exported = None;
        }
        let folder = p.folder.clone();
        self.save_project_settings();
        let held_out = (args.sequence && !proof).then(|| args.out.clone());
        let texts = self.lang.t();
        let task = Task::spawn(&self.ctx, move |rep| {
            // The folder held from the start (measuring comes first): another render there is
            // refused at once, and sees this one.
            let _held = match held_out.as_deref().map(lapsewerk_video::frames::reserve) {
                // Held by another render: said in the app's words (who, since, last active).
                Some(Err(e)) => {
                    let holder = held_out
                        .as_deref()
                        .and_then(lapsewerk_video::frames::lock_holder_at);
                    return Outcome::Failed(match holder {
                        Some(h) => texts
                            .target_busy
                            .replace("{who}", &crate::pages::holder_text(texts, Some(&h)))
                            .replace(" ({n} Bilder bisher)", "")
                            .replace(" ({n} frames so far)", ""),
                        None => e.to_string(),
                    });
                }
                Some(Ok(r)) => Some(r),
                None => None,
            };
            let mut seq = lock(&seq);
            let a = match analysis {
                Some(a) => a,
                None => match lapsewerk_studio::cache::analyse_cached(&mut seq, &plan, &opts, rep, &folder) {
                    Ok(a) => Arc::new(a),
                    Err(e) => return Outcome::Failed(e),
                },
            };
            let reused = match ready {
                Some(c) => match lapsewerk_studio::reuse(&mut seq, &plan, &c, &args, rep) {
                    Ok(c) => c,
                    Err(e) => return Outcome::Failed(e),
                },
                None => None,
            };
            let corrected = match reused {
                Some(c) => Ok(c),
                None => {
                    lapsewerk_studio::cache::correct_cached(&mut seq, &plan, &a, &opts, &args, rep, &folder)
                }
            };
            match corrected.and_then(|c| lapsewerk_studio::render(&mut seq, &plan, &a, &c, rep)) {
                Ok(path) => Outcome::Exported(path),
                Err(e) => Outcome::Failed(e),
            }
        });
        self.task = Some((if proof { TaskKind::Proof } else { TaskKind::Export }, task));
    }

    /// Checks the frame sequence in the export folder against its SHA256SUMS.txt (as
    /// `lapsewerk verify`), off the UI thread.
    pub fn start_verify(&mut self) {
        let Some(p) = &mut self.project else { return };
        p.verified = None;
        let dir = p.target();
        p.verified_dir = Some(dir.clone());
        let task = Task::spawn(
            &self.ctx,
            move |_rep| match lapsewerk_video::frames::verify_checksums(&dir) {
                Ok((ok, bad)) => Outcome::Verified(ok, bad),
                Err(e) => Outcome::Failed(e),
            },
        );
        self.task = Some((TaskKind::Verify, task));
    }

    /// Writes the suggested keyframes (sidecars beside frames that have none), then plans anew.
    pub fn create_suggested(&mut self) {
        let Some(p) = &self.project else { return };
        let (Some(seq), Some(plan), Some(a)) = (p.seq.clone(), p.plan.clone(), p.analysis.clone()) else {
            return;
        };
        let suggested = p.suggestions(&a);
        let result = lock(&seq).write_keyframes(&plan, &suggested);
        match result {
            Ok(written) => {
                if let Some(p) = &mut self.project {
                    for w in &written {
                        if let lapsewerk_develop::KeyframeWrite::Written { frame, .. } = w
                            && !p.untouched.iter().any(|(f, _)| f == frame)
                        {
                            let fp = p
                                .seq
                                .as_ref()
                                .map(|s| sidecar_print(&lock(s), *frame))
                                .unwrap_or_default();
                            p.untouched.push((*frame, fp));
                        }
                    }
                }
                let failed = written
                    .iter()
                    .filter_map(|w| match w {
                        lapsewerk_develop::KeyframeWrite::Failed { reason, .. } => Some(reason.clone()),
                        _ => None,
                    })
                    .next();
                if let Some(reason) = failed {
                    self.say(reason, true);
                }
                self.replan();
                // The new keyframes hold what the plan gave there: the analysis still holds.
                if let Some(p) = &self.project
                    && let Some(after) = &p.plan
                {
                    let opts = p.measure_settings();
                    lapsewerk_studio::cache::carry_over(&lock(&seq), &plan, after, &opts, &p.folder);
                }
            }
            Err(e) => self.say(e.to_string(), true),
        }
    }

    /// Auto transition again for `frame`: it follows the edited keyframes (blended from them and
    /// saved), until it is edited again. Leaves the edit.
    pub fn make_automatic(&mut self, frame: usize) {
        let Some(p) = &mut self.project else { return };
        p.edit = None;
        if !p.untouched.iter().any(|(f, _)| *f == frame) {
            // Taken as it is now; the transition then writes it.
            p.untouched.push((frame, String::new()));
        }
        p.edited_seen.clear();
        self.replan();
        self.save_project_settings();
    }

    /// Starts editing `frame` as a keyframe.
    pub fn edit_frame(&mut self, frame: usize) {
        let Some(p) = &mut self.project else { return };
        let Some(plan) = &p.plan else { return };
        // From the base look while nothing is edited: what the preview showed is where editing starts.
        // The crop as it is (not made taller for the video): it is what gets saved.
        let settings = lapsewerk_studio::with_base_look(plan, p.base_look()).settings(frame, 0.0);
        p.edit_version += 1;
        p.edit = Some(Edit::new(frame, settings, p.edit_version));
        p.cursor = frame;
    }

    /// Changes a control of the keyframe being edited.
    pub fn set_control(&mut self, id: &str, value: f64) {
        let Some(p) = &mut self.project else { return };
        let Some(e) = &mut p.edit else { return };
        let before = e.settings.clone();
        let changed = if id.starts_with(lapsewerk_develop::masks::PREFIX) {
            lapsewerk_develop::masks::set(&mut e.settings, id, value)
        } else {
            lapsewerk_develop::controls::set(&mut e.settings, id, value)
        };
        // A slider that only rounds what it shows is no change.
        if changed && e.settings != before {
            let after = std::mem::replace(&mut e.settings, before);
            e.before_change(id);
            e.settings = after;
            self.edited();
        }
    }

    /// Sets several controls of the keyframe being edited at once: one step to undo.
    pub fn set_controls(&mut self, values: &[(&str, f64)], step: &str) {
        let Some(p) = &mut self.project else { return };
        let Some(e) = &mut p.edit else { return };
        let mut next = e.settings.clone();
        for (id, v) in values {
            lapsewerk_develop::controls::set(&mut next, id, *v);
        }
        if next == e.settings {
            return;
        }
        e.before_change(step);
        e.last = None;
        e.settings = next;
        self.edited();
    }

    /// LightCraft's Auto for the keyframe being edited (tone and colour, not white balance).
    pub fn auto_tone(&mut self) {
        let Some(p) = &self.project else { return };
        let (Some(seq), Some(e)) = (p.seq.clone(), &p.edit) else {
            return;
        };
        let (frame, settings) = (e.frame, e.settings.clone());
        let result = lock(&seq).auto_tone(frame, &settings);
        match result {
            Ok(values) => self.set_controls(&values, "auto"),
            Err(e) => self.say(e.to_string(), true),
        }
    }

    /// White balance of the keyframe being edited: the spot at (`x`, `y`) (0..1) made neutral grey,
    /// or (no spot) the frame's own (Auto).
    pub fn white_balance(&mut self, spot: Option<(f64, f64)>) {
        let Some(p) = &mut self.project else { return };
        p.picking_wb = false;
        let (Some(seq), Some(e)) = (p.seq.clone(), &p.edit) else {
            return;
        };
        let (frame, settings) = (e.frame, e.settings.clone());
        let result = lock(&seq).white_balance(frame, &settings, spot);
        match result {
            Ok((temp, tint)) => self.set_controls(&[("wb.temp", temp), ("wb.tint", tint)], "wb"),
            Err(e) => self.say(e.to_string(), true),
        }
    }

    /// Adds a mask of `kind` to the keyframe being edited and selects it.
    pub fn add_mask(&mut self, kind: lapsewerk_develop::masks::Kind) {
        let t = self.lang.t();
        let Some(p) = &mut self.project else { return };
        let Some(e) = &mut p.edit else { return };
        e.before_change("mask+");
        let n = e.settings.masks.len() + 1;
        let name = format!("{} {n}", crate::masks_ui::kind_name(kind, t));
        e.settings
            .masks
            .push(lapsewerk_develop::masks::new_mask(kind, &name));
        e.mask = Some(e.settings.masks.len() - 1);
        self.edited();
    }

    /// Carries out what was done in the masks panel or on the picture.
    pub fn mask_actions(&mut self, actions: Vec<crate::masks_ui::Action>) {
        use crate::masks_ui::Action;
        for a in actions {
            match a {
                Action::Add(kind) => self.add_mask(kind),
                Action::Remove(i) => self.remove_mask(i),
                Action::Invert(i) => self.invert_mask(i),
                Action::Select(i) => {
                    if let Some(e) = self.project.as_mut().and_then(|p| p.edit.as_mut()) {
                        e.mask = i;
                    }
                }
                Action::Set(id, v) => self.set_control(&id, v),
                Action::Tool(tool) => self.brush = tool,
                Action::Stroke(i, points) => {
                    let tool = self.brush;
                    self.change_mask(i, |m| {
                        lapsewerk_develop::masks::add_stroke(m, &points, tool.size, tool.feather, tool.erase)
                    });
                }
                Action::ClearStrokes(i) => self.change_mask(i, lapsewerk_develop::masks::clear_strokes),
            }
        }
    }

    /// Changes mask `i` of the keyframe being edited with `f` (whether it changed), one step to undo.
    fn change_mask(&mut self, i: usize, f: impl FnOnce(&mut lapsewerk_develop::Mask) -> bool) {
        let Some(p) = &mut self.project else { return };
        let Some(e) = &mut p.edit else { return };
        let before = e.settings.clone();
        let Some(m) = e.settings.masks.get_mut(i) else {
            return;
        };
        if !f(m) {
            return;
        }
        let after = std::mem::replace(&mut e.settings, before);
        e.before_change("stroke");
        // Every stroke is a step of its own.
        e.last = None;
        e.settings = after;
        self.edited();
    }

    /// Removes mask `i` of the keyframe being edited.
    pub fn remove_mask(&mut self, i: usize) {
        let Some(p) = &mut self.project else { return };
        let Some(e) = &mut p.edit else { return };
        if i >= e.settings.masks.len() {
            return;
        }
        e.before_change("mask-");
        e.settings.masks.remove(i);
        e.mask = None;
        self.edited();
    }

    /// Turns mask `i` inside out.
    pub fn invert_mask(&mut self, i: usize) {
        let Some(p) = &mut self.project else { return };
        let Some(e) = &mut p.edit else { return };
        let Some(m) = e.settings.masks.get(i) else { return };
        let invert = !m.invert;
        e.before_change("mask~");
        if let Some(m) = e.settings.masks.get_mut(i) {
            m.invert = invert;
        }
        self.edited();
    }

    /// Undoes (`back`) or redoes a change of the keyframe being edited.
    pub fn undo(&mut self, back: bool) {
        let Some(p) = &mut self.project else { return };
        let Some(e) = &mut p.edit else { return };
        if e.step(back) {
            self.edited();
        }
    }

    /// The keyframe being edited changed: a new preview.
    fn edited(&mut self) {
        let Some(p) = &mut self.project else { return };
        let Some(e) = &mut p.edit else { return };
        p.edit_version += 1;
        e.version = p.edit_version;
        let current = e.version;
        self.previews
            .drop_kind(|k| !matches!(k, Kind::Edit(v) if v + 4 < current));
    }

    pub fn save_edit(&mut self) {
        let Some(p) = &mut self.project else { return };
        let (Some(seq), Some(e)) = (p.seq.clone(), &p.edit) else {
            return;
        };
        let frame = e.frame;
        let result = lock(&seq).save_keyframe(frame, e.settings.clone());
        match result {
            Ok(_) => {
                if let Some(e) = &mut p.edit {
                    e.original = e.settings.clone();
                }
                // This one is edited now; those never edited follow the edited ones (on replan).
                p.untouched.retain(|(f, _)| *f != frame);
                self.replan();
                self.save_project_settings();
            }
            Err(e) => self.say(e.to_string(), true),
        }
    }

    /// Opens the frame looked at (a keyframe, saved first) in LightCraft, and watches its sidecar:
    /// when LightCraft saves it, the keyframe is read again.
    pub fn open_in_lightcraft(&mut self) {
        // The keyframe as it is now goes into its sidecar first, so LightCraft starts from it.
        let dirty = self
            .project
            .as_ref()
            .and_then(|p| p.edit.as_ref())
            .is_some_and(Edit::dirty);
        if dirty {
            self.save_edit();
        }
        let Some(p) = &mut self.project else { return };
        let Some(seq) = p.seq.clone() else { return };
        let frame = p.edit.as_ref().map_or(p.cursor, |e| e.frame);
        let made = {
            let mut seq = lock(&seq);
            match seq.sidecar(frame) {
                Some(path) => Ok((path, false)),
                None => {
                    let settings = p
                        .plan
                        .as_ref()
                        .map(|plan| plan.settings(frame, 0.0))
                        .unwrap_or_default();
                    seq.save_keyframe(frame, settings).map(|path| (path, true))
                }
            }
        };
        let (sidecar, new) = match made {
            Ok(m) => m,
            Err(e) => {
                let t = self.lang.t();
                self.say(format!("{}: {e}", t.error), true);
                return;
            }
        };
        let Some(photo) = p.files.get(frame).cloned() else {
            return;
        };
        let modified = std::fs::metadata(&sidecar).and_then(|m| m.modified()).ok();
        p.external = Some(External {
            frame,
            sidecar,
            modified,
            checked: Instant::now(),
        });
        if new {
            self.replan();
        }
        let t = self.lang.t();
        match launch_lightcraft(self.lightcraft.as_deref(), &photo) {
            Ok(()) => self.say(t.lightcraft_opened, false),
            Err(e) => self.say(format!("{}: {e}", t.lightcraft_missing), true),
        }
    }

    /// The keyframe opened in LightCraft: read again when its sidecar changed (checked once a second).
    fn watch_external(&mut self) {
        let Some(p) = &mut self.project else { return };
        let Some(x) = &mut p.external else { return };
        if x.checked.elapsed().as_secs_f32() < 1.0 {
            return;
        }
        x.checked = Instant::now();
        let now = std::fs::metadata(&x.sidecar).and_then(|m| m.modified()).ok();
        if now.is_none() || now == x.modified {
            return;
        }
        x.modified = now;
        let frame = x.frame;
        let Some(seq) = p.seq.clone() else { return };
        let result = lock(&seq).reload_keyframe(frame);
        // Edited in LightCraft: no longer following the others. Only a rating or a label saved
        // (its develop settings as before, by their fingerprint) is no edit, as on opening.
        let print = lapsewerk_studio::cache::sidecar_print(&lock(&seq), frame);
        p.untouched.retain(|(f, before)| *f != frame || *before == print);
        let t = self.lang.t();
        match result {
            Ok(_) => {
                self.replan();
                self.save_project_settings();
                // An open editor on that keyframe shows what LightCraft saved.
                let editing = self
                    .project
                    .as_ref()
                    .and_then(|p| p.edit.as_ref())
                    .is_some_and(|e| e.frame == frame);
                if editing {
                    self.edit_frame(frame);
                }
                self.say(
                    t.lightcraft_reloaded.replace("{n}", &(frame + 1).to_string()),
                    false,
                );
            }
            Err(e) => self.say(format!("{}: {e}", t.error), true),
        }
        self.ctx.request_repaint();
    }

    /// Takes in keyframes saved outside (LightCraft saving the metadata of several at once): each
    /// keyframe's sidecar whose time changed is read again; the ones whose settings changed are
    /// no longer automatic, planned anew and named. The app's own saves change nothing there.
    fn watch_keyframes(&mut self) {
        let Some(p) = &mut self.project else { return };
        if p.watched_at.elapsed().as_secs_f32() < 2.0 || self.task.is_some() {
            return;
        }
        p.watched_at = Instant::now();
        let Some(seq) = p.seq.clone() else { return };
        let sidecars: Vec<(usize, PathBuf)> = {
            let s = lock(&seq);
            s.keyframe_frames()
                .into_iter()
                .filter_map(|f| s.sidecar(f).map(|path| (f, path)))
                .collect()
        };
        let mut changed = Vec::new();
        for (f, path) in sidecars {
            let time = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            // First seen (just opened, or after a task kept this from looking): read again too, so a
            // save while measuring is not taken for the time it was seen at; equal settings say nothing.
            if p.watched.insert(f, time).is_none_or(|before| before != time) {
                changed.push(f);
            }
        }
        let mut edited = Vec::new();
        let mut broken = Vec::new();
        for f in changed {
            let mut s = lock(&seq);
            let before = format!("{:?}", s.settings(f).ok());
            // Saved damaged (cut off, not a sidecar any more): said at once, not passed over; the
            // look as before stays until it is saved whole again.
            // Read again, and readable as a sidecar (one cut short or not text reads "fine" but
            // holds no settings: said, as on opening).
            if s.reload_keyframe(f).is_err() || s.unreadable_sidecars().contains(&f) {
                broken.push(f);
                continue;
            }
            if format!("{:?}", s.settings(f).ok()) != before {
                let print = lapsewerk_studio::cache::sidecar_print(&s, f);
                p.untouched.retain(|(u, was)| *u != f || *was == print);
                edited.push(f);
            }
        }
        if !broken.is_empty() {
            let t = self.lang.t();
            let list: Vec<String> = broken.iter().map(|f| (f + 1).to_string()).collect();
            if let Some(p) = &mut self.project {
                for f in &broken {
                    if !p.unreadable.contains(f) {
                        p.unreadable.push(*f);
                    }
                }
            }
            self.say(t.sidecar_broken_live.replace("{frames}", &list.join(", ")), true);
        }
        if edited.is_empty() {
            return;
        }
        self.replan();
        // Which keyframes are automatic now, kept at once (a crash or the command line meanwhile
        // must not take the followed ones for edited).
        self.save_project_settings();
        let editing = self
            .project
            .as_ref()
            .and_then(|p| p.edit.as_ref())
            .map(|e| e.frame)
            .filter(|f| edited.contains(f));
        if let Some(f) = editing {
            self.edit_frame(f);
        }
        let t = self.lang.t();
        let list: Vec<String> = edited.iter().map(|f| (f + 1).to_string()).collect();
        self.say(t.lightcraft_reloaded.replace("{n}", &list.join(", ")), false);
        self.ctx.request_repaint();
    }

    /// Makes a colour profile of the camera from the raw photos in `folders`.
    pub fn start_calibrate(&mut self, folders: Vec<PathBuf>) {
        let task = Task::spawn(&self.ctx, move |rep| {
            match lapsewerk_studio::calibrate(&folders, None, rep) {
                Ok(c) => Outcome::Calibrated(c),
                Err(e) => Outcome::Failed(e),
            }
        });
        self.task = Some((TaskKind::Calibrate, task));
    }

    /// Opens the dialog copying the edited keyframe's settings to the other saved keyframes.
    pub fn open_copy(&mut self) {
        let Some(p) = &mut self.project else { return };
        let Some(e) = &p.edit else { return };
        let frame = e.frame;
        let masks = e.settings.masks.iter().map(|m| (m.name.clone(), true)).collect();
        // Ticked: the automatic ones (they follow anyway); one edited by hand (a night look) only
        // when ticked on purpose.
        let targets = p
            .keyframes()
            .into_iter()
            .filter(|&f| f != frame)
            .map(|f| (f, p.untouched.iter().any(|(u, _)| *u == f)))
            .collect();
        p.copy = Some(CopyDialog {
            parts: crate::sliders::copy_parts().iter().map(|part| part.on).collect(),
            targets,
            masks,
        });
    }

    /// Copies the ticked parts of the edited keyframe to the ticked keyframes, saving them.
    pub fn copy_to_keyframes(&mut self) {
        let Some(p) = &mut self.project else { return };
        let (Some(seq), Some(e), Some(dialog)) = (p.seq.clone(), &p.edit, p.copy.take()) else {
            return;
        };
        let parts = crate::sliders::copy_parts();
        let chosen: Vec<&crate::sliders::Part> = parts
            .iter()
            .zip(&dialog.parts)
            .filter(|(_, on)| **on)
            .map(|(part, _)| part)
            .collect();
        let ids: Vec<&str> = chosen.iter().flat_map(|part| part.ids.iter().copied()).collect();
        // Masks one by one (those ticked); the masks part is their heading.
        let masks_on = chosen.iter().any(|part| part.masks);
        let mask_picks: Vec<usize> = dialog
            .masks
            .iter()
            .enumerate()
            .filter(|(_, (_, on))| *on && masks_on)
            .map(|(i, _)| i)
            .collect();
        let source = e.settings.clone();
        let mut done = 0;
        let mut failed = None;
        {
            let mut seq = lock(&seq);
            for (f, _) in dialog.targets.iter().filter(|(_, on)| *on) {
                let result = seq.settings(*f).and_then(|mut d| {
                    lapsewerk_develop::copy_settings(&source, &mut d, &ids, false);
                    lapsewerk_develop::copy_masks(&source, &mut d, &mask_picks);
                    seq.save_keyframe(*f, d)
                });
                match result {
                    Ok(_) => done += 1,
                    Err(e) => failed = Some(e.to_string()),
                }
            }
        }
        // Settings given to them by hand: edited now, no longer following the others.
        if let Some(p) = &mut self.project {
            p.untouched
                .retain(|(f, _)| !dialog.targets.iter().any(|(t, on)| *on && t == f));
        }
        let t = self.lang.t();
        match failed {
            Some(e) => self.say(format!("{}: {e}", t.error), true),
            None => self.say(t.copied.replace("{n}", &done.to_string()), false),
        }
        if done > 0 {
            self.replan();
        }
    }

    /// The keyframes changed: a new plan; the measurements are stale.
    /// Auto transition: the untouched keyframes (made, never edited) follow the edited ones. One
    /// whose sidecar changed since (edited elsewhere: LightCraft, another raw editor, by hand) is edited now
    /// and never written over; the others are set again when an edited keyframe changed.
    fn sync_untouched(&mut self) {
        let Some(p) = &mut self.project else { return };
        let Some(seq) = p.seq.clone() else { return };
        // Along the scene's light: the analysis's, else the one kept beside the photos (as the
        // command line does), so app and command line blend alike.
        let n = crate::work::lock(&seq).len();
        let light = p
            .analysis
            .as_ref()
            .map(|a| a.scene.clone())
            .or_else(|| lapsewerk_studio::cache::kept_scene(&p.folder, n));
        let result = lapsewerk_studio::cache::follow_edited(
            &mut lock(&seq),
            &mut p.untouched,
            &p.edited_seen,
            light.as_deref(),
        );
        // Said in step 3 (worked out here, on a change, not each time it is drawn).
        p.untouched_kept = lapsewerk_studio::cache::untouched_kept(&lock(&seq), &p.untouched);
        match result {
            Ok(Some(seen)) => p.edited_seen = seen,
            Ok(None) => {}
            Err(e) => self.say(e, true),
        }
    }

    fn replan(&mut self) {
        self.sync_untouched();
        let Some(p) = &mut self.project else { return };
        let Some(seq) = p.seq.clone() else { return };
        let (plan, unreadable) = {
            let s = lock(&seq);
            let mut odd = s.unreadable_sidecars();
            p.sidecar_keyframes = s.keyframe_frames();
            p.other_editor = s.other_editor_beside();
            p.unknown = s.unknown_settings();
            odd.retain(|f| !p.other_editor.contains(f));
            (s.plan(lapsewerk_core::keyframes::Interpolation::Smooth), odd)
        };
        p.unreadable = unreadable;
        match plan {
            Ok(plan) => {
                let plan = Arc::new(plan);
                p.plan = Some(Arc::clone(&plan));
                p.stale = true;
                p.corrected = None;
                p.developed = lapsewerk_studio::developed_size(&lock(&seq), &plan, 0);
                p.shown_video = p.preview_video();
                let plan = shown_plan(&plan, p.base_look(), &seq, p.shown_video);
                self.previews.set_source(Some(preview::Source { seq, plan }));
            }
            Err(e) => self.say(e.to_string(), true),
        }
    }

    /// Frames the previews anew when the video's shape changed (another format chosen, the
    /// frames found upright): a wide crop is shown as the export frames it.
    fn sync_framing(&mut self) {
        let Some(p) = &mut self.project else { return };
        let (Some(seq), Some(plan)) = (p.seq.clone(), p.plan.clone()) else {
            return;
        };
        // Not waiting for a sequence in use: the last size known stays until it is free.
        if let Ok(s) = seq.try_lock() {
            p.developed = lapsewerk_studio::developed_size(&s, &plan, 0);
        }
        let video = p.preview_video();
        let shape = |v: (usize, usize)| v.0 as f64 / v.1.max(1) as f64;
        if p.shown_video == (0, 0) || (shape(video) / shape(p.shown_video) - 1.0).abs() < 1e-3 {
            return;
        }
        p.shown_video = video;
        // Only a crop changes: without one the previews stay as they are.
        let cropped = (0..p.frames())
            .step_by((p.frames() / 16).max(1))
            .any(|i| !plan.settings(i, 0.0).crop.geometry.is_identity());
        if cropped {
            let plan = shown_plan(&plan, p.base_look(), &seq, video);
            self.previews.set_source(Some(preview::Source { seq, plan }));
        }
    }

    /// Takes in a finished task.
    pub fn poll(&mut self) {
        self.sync_framing();
        self.watch_external();
        self.watch_keyframes();
        self.previews.poll(&self.ctx);
        self.thumbs.poll(&self.ctx, &self.recent);
        let Some((kind, task)) = &mut self.task else {
            return;
        };
        let kind = *kind;
        let Some((outcome, notes)) = task.take_outcome() else {
            return;
        };
        // What the work reported, for whoever runs the app from a terminal.
        if std::env::var_os("LAPSEWERK_LOG").is_some() {
            for n in &notes {
                eprintln!("{n}");
            }
        }
        self.task = None;
        let lang = self.lang;
        let t = lang.t();
        // A camera profile needs no project.
        let outcome = match outcome {
            Outcome::Calibrated(c) => {
                let text = if c.written.is_empty() {
                    t.calibrate_none
                        .replace("{n}", &lapsewerk_develop::calibrate::MIN_PHOTOS.to_string())
                } else {
                    let names: Vec<String> = c.written.iter().map(|(m, n, _)| format!("{m} ({n})")).collect();
                    t.calibrate_done.replace("{models}", &names.join(", "))
                };
                self.say(text, c.written.is_empty());
                return;
            }
            Outcome::Failed(e) if kind == TaskKind::Calibrate => {
                self.say(format!("{}: {e}", t.error), true);
                return;
            }
            Outcome::Cancelled if kind == TaskKind::Calibrate => {
                self.say(t.cancelled, false);
                return;
            }
            o => o,
        };
        let Some(p) = &mut self.project else { return };
        // Settings chosen for the user (day to night recommended): in the project file at once,
        // so a render from the command line or the queue has them too.
        let mut save = false;
        match outcome {
            Outcome::Opened(o) => {
                let o = *o;
                // Unreadable files were left out: the frames are those opened.
                p.skipped_names = o
                    .seq
                    .skipped()
                    .iter()
                    .map(|p| {
                        p.file_name()
                            .map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
                    })
                    .collect();
                if !p.skipped_names.is_empty() {
                    p.files = o.seq.frames().iter().map(|f| f.path.clone()).collect();
                    // Those inside the series become blends; those at its ends make the video
                    // shorter (no neighbours to blend): each said for what it is.
                    let inside: usize = o.seq.skipped_gaps().iter().map(|(_, m)| m).sum();
                    let edge = p.skipped_names.len().saturating_sub(inside);
                    let mut text = t
                        .skipped_files
                        .replace("{names}", &crate::text::names_list(lang, &p.skipped_names));
                    if inside > 0 {
                        text.push(' ');
                        text.push_str(t.skipped_mid);
                    }
                    match edge {
                        0 => {}
                        1 => {
                            text.push(' ');
                            text.push_str(t.skipped_edge_one);
                        }
                        n => {
                            text.push(' ');
                            text.push_str(&t.skipped_edge_many.replace("{n}", &n.to_string()));
                        }
                    }
                    // (the field, not `say`: the project is borrowed here). Not red: nothing is
                    // wrong now, the hints say it too.
                    self.message = Some(Message {
                        text,
                        error: false,
                        at: Instant::now(),
                    });
                }
                p.camera_data = o.seq.frames().iter().filter(|f| f.exposure.is_some()).count();
                p.skipped_gaps = o.seq.skipped_gaps();
                p.timing = o.timing;
                // A new project of photos smaller than 4K: Full HD, not a 4K video scaled up.
                if !p.preset_chosen && o.seq.source_size(0).is_some_and(|(w, h)| w.max(h) < 3840) {
                    p.preset = 2;
                }
                // Keyframes saved by another raw editor too, and sidecars nothing can read: said in step 3
                // from the first look on (not only after a keyframe changes).
                p.other_editor = o.seq.other_editor_beside();
                p.sidecar_keyframes = o.seq.keyframe_frames();
                p.unknown = o.seq.unknown_settings();
                let mut odd = o.seq.unreadable_sidecars();
                odd.retain(|f| !p.other_editor.contains(f));
                p.unreadable = odd;
                p.developed = lapsewerk_studio::developed_size(&o.seq, &o.plan, 0);
                let seq = Arc::new(Mutex::new(o.seq));
                let plan = Arc::new(o.plan);
                p.seq = Some(Arc::clone(&seq));
                p.plan = Some(Arc::clone(&plan));
                p.shown_video = p.preview_video();
                let plan = shown_plan(&plan, p.base_look(), &seq, p.shown_video);
                self.previews.set_source(Some(preview::Source { seq, plan }));
                // Keyframes marked automatic in their own sidecars (photos and XMP passed on
                // without lapsewerk.json): automatic here too.
                if let Some(p) = &mut self.project
                    && let Some(seq) = &p.seq
                {
                    lapsewerk_studio::cache::with_marked(&lock(seq), &mut p.untouched);
                }
                // Keyframes edited elsewhere since: no longer automatic, and the automatic ones
                // follow them.
                if self.project.as_ref().is_some_and(|p| !p.untouched.is_empty()) {
                    self.replan();
                    self.save_project_settings();
                }
                self.on_enter();
            }
            Outcome::Analysed(a) => {
                // A day-to-night sequence: keep some of the change of light (recommended), unless the
                // options were changed already.
                if a.day_to_night() && p.finish == Finish::default() {
                    p.finish.keep_light = Some(KEEP_LIGHT_DEFAULT);
                    save = true;
                }
                let (frames, skipped): (Vec<lapsewerk_core::sequence::Frame>, Vec<(usize, usize)>) = p
                    .seq
                    .as_ref()
                    .map(|s| {
                        let s = lock(s);
                        (s.frames().to_vec(), s.skipped_gaps())
                    })
                    .unwrap_or_default();
                p.quality = Some(lapsewerk_studio::quality_with(&a, &frames, &skipped));
                p.analysis = Some(Arc::from(a));
                p.flicker_estimate = None;
                p.stale = false;
                if p.step == 5 {
                    self.start_correct();
                }
            }
            Outcome::Corrected(c) if kind == TaskKind::MeasureResult => {
                // Said, not taken in: the preview's corrections stay as they are.
                let mut stops = c.verified_seen.clone().unwrap_or_default();
                // Photos the export replaces (a flash) and the jump across a gap it fills are not
                // what the video shows: left out of the figure (said beside it).
                let repair = p.render_args(true).repair;
                let mut left_out = 0;
                for i in repair
                    .replace
                    .iter()
                    .copied()
                    .chain(repair.fill.iter().map(|(b, _)| *b))
                {
                    // Left out by the measuring already; counted to be said.
                    if let Some(s) = stops.get_mut(i) {
                        *s = None;
                        left_out += 1;
                    }
                }
                let whole = lapsewerk_studio::measure::jitter(&stops);
                let worst = (0..stops.len().saturating_sub(24))
                    .step_by(5)
                    .filter_map(|a| {
                        stops
                            .get(a..a + 25)
                            .map(|w| (a, lapsewerk_studio::measure::jitter(w)))
                    })
                    .max_by(|p, q| p.1.total_cmp(&q.1))
                    .filter(|(_, j)| *j > 2.0 * whole && *j > 0.004);
                // No stretch stands out: the photo that stands out most from its neighbours (as
                // `--verify` finds its largest jumps; the plain step from one photo to the next
                // named a day-to-night ramp, and a stretch named there ran past a short series'
                // end).
                let jump = if worst.is_none() && whole > 0.01 {
                    stops
                        .windows(3)
                        .enumerate()
                        .filter_map(|(i, w)| match w {
                            [Some(a), Some(b), Some(c)] => {
                                Some((i + 1, (a - 2.0 * b + c).abs(), b - a, c - b))
                            }
                            _ => None,
                        })
                        .max_by(|p, q| p.1.total_cmp(&q.1))
                } else {
                    None
                };
                // A step (light that stays: the moon rising) shows as two equal peaks either side
                // of it: said between the two photos (the first is `at`), not at one; a photo that
                // stands out on its own (both ways, back again) is said at that photo.
                let step = jump.and_then(|(i, _, before, after)| {
                    if before.signum() == after.signum() && before.abs() > 0.0 && after.abs() > 0.0 {
                        return None;
                    }
                    if after.abs() > 2.0 * before.abs() {
                        Some(i)
                    } else if before.abs() > 2.0 * after.abs() {
                        Some(i - 1)
                    } else {
                        None
                    }
                });
                let jump = jump.map(|(i, j, _, _)| (step.unwrap_or(i), j));
                p.measured = Some((whole, worst.or(jump)));
                p.measured_for = p.corrected.as_ref().map(|c| offsets_print(&c.offsets));
                p.measured_jump = jump.is_some();
                p.measured_step = step.is_some();
                p.measured_wave = lapsewerk_studio::measure::slow_wave(&stops)
                    .filter(|(_, a)| *a > lapsewerk_studio::SLOW_WAVE_SAID);
                p.measured_wave_span = p
                    .measured_wave
                    .map(|(at, _)| lapsewerk_studio::measure::wave_span(&stops, at));
                p.measured_wave_why = match p.measured_wave {
                    Some((at, _)) => {
                        let drawn: Vec<Option<f64>> =
                            lapsewerk_core::daynight::shaped(&p.brightness_points(), stops.len())
                                .into_iter()
                                .map(Some)
                                .collect();
                        let hand = (at.saturating_sub(5)..=at + 5)
                            .filter_map(|i| lapsewerk_studio::measure::wave_at(&drawn, i))
                            .fold(0.0f64, f64::max);
                        if !p.brightness_points().is_empty() && hand > lapsewerk_studio::SLOW_WAVE_SAID {
                            WaveWhy::Drawn
                        } else if !p.finish.deflicker {
                            WaveWhy::NoDeflicker
                        } else {
                            WaveWhy::Look
                        }
                    }
                    None => WaveWhy::Look,
                };
                p.measured_len = stops.len();
                p.measured_left_out = left_out;
                // Kept with the project, to show again on reopening.
                save = true;
            }
            Outcome::Corrected(c) => {
                // "Measure result" measured these very corrections (reopened, nothing changed):
                // kept; any change and it is gone.
                let print = offsets_print(&c.offsets);
                if p.measured_for.as_deref() != Some(print.as_str()) {
                    p.measured = None;
                    p.measured_wave = None;
                }
                p.corrected = Some(Arc::from(c));
                p.stale = false;
                p.video_version += 1;
                let v = p.video_version;
                self.previews
                    .drop_kind(|k| !matches!(k, Kind::Video(old) if old < v));
            }
            // Taken in above.
            Outcome::Calibrated(_) => {}
            Outcome::Exported(path) if kind == TaskKind::Proof => {
                let frames = path.is_dir();
                if frames {
                    Self::reveal(&path);
                } else {
                    Self::play(&path);
                }
                p.proof = Some(path.clone());
                self.say(if frames { t.proof_done_frames } else { t.proof_done }, false);
            }
            Outcome::Verified(ok, bad) => {
                let fine = bad.is_empty();
                p.verified = Some((ok, bad));
                let names = p.verified_damaged();
                let damaged = names.len();
                let text = if fine {
                    t.verify_ok.replace("{n}", &ok.to_string())
                } else if damaged > 0
                    && p.sequence
                    && p.can_resume()
                    && p.render_there().is_none_or(|r| r.same != Some(false))
                {
                    // Said with what to do: going on makes just those again (a stopped export: and
                    // the rest after them).
                    let stopped = p.render_there().is_some_and(|r| !r.ended);
                    if stopped {
                        t.footer_damaged_stopped
                    } else {
                        t.footer_damaged
                    }
                    .replace("{bad}", &p.damage_words(t, &names))
                } else {
                    let bad = p.verified.as_ref().map_or(&[][..], |v| v.1.as_slice());
                    crate::pages::verify_bad_text(
                        t,
                        bad,
                        p.render_there().is_some_and(|r| r.same == Some(false)),
                    )
                };
                self.say(text, !fine);
            }
            Outcome::Exported(path) => {
                let frames = path.is_dir();
                // A queued export of the same target stopped or waiting from last time is done
                // now too: not left as "stopped" beside the finished delivery.
                self.queue.finished_elsewhere(&path);
                p.delivered = (!frames).then(|| p.movie_delivery());
                p.exported = Some(path);
                p.verified = None;
                self.say(
                    if frames {
                        t.export_done_frames
                    } else {
                        t.export_done
                    },
                    false,
                );
            }
            Outcome::Cancelled => {
                if kind == TaskKind::Correct {
                    p.corrected_with = None;
                }
                self.say(t.cancelled, false);
            }
            Outcome::Failed(e) => {
                if kind == TaskKind::Correct {
                    p.corrected_with = None;
                }
                self.say(format!("{}: {e}", t.error), true);
            }
        }
        if save {
            self.save_project_settings();
        }
    }

    /// Opens the system's file viewer at `path`.
    /// Opens a movie in the system's player.
    pub fn play(path: &Path) {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(opener).arg(path).spawn();
    }

    pub fn reveal(path: &Path) {
        let _ = if cfg!(target_os = "macos") {
            std::process::Command::new("open").arg("-R").arg(path).spawn()
        } else if cfg!(windows) {
            std::process::Command::new("explorer")
                .arg("/select,")
                .arg(path)
                .spawn()
        } else {
            std::process::Command::new("xdg-open")
                .arg(path.parent().unwrap_or(path))
                .spawn()
        };
    }

    /// The UI, one frame.
    /// The introduction is done (or skipped): not shown by itself again.
    pub fn close_intro(&mut self) {
        self.intro = None;
        if !self.recent.intro_seen {
            self.recent.intro_seen = true;
            self.recent.save();
        }
    }

    pub fn frame(&mut self, ui: &mut egui::Ui) {
        // The first start of all (not a headless run): the introduction.
        if !self.recent.intro_seen && self.intro.is_none() && self.autopilot.is_none() && !self.intro_offered
        {
            self.intro_offered = true;
            self.intro = Some(0);
        }
        if ui.ctx().input(|i| i.key_pressed(egui::Key::F1)) {
            self.help = !self.help;
        }
        crate::help::intro(self, ui.ctx());
        self.poll();
        if let Some((name, ok)) = self.queue.poll(self.cli.as_deref()) {
            let t = self.lang.t();
            let mut text = if ok { t.queue_done } else { t.queue_failed }.replace("{name}", &name);
            // Why, in the status bar too (not only in the queue's card).
            if let Some(crate::queue::State::Failed(e)) = self
                .queue
                .jobs
                .iter()
                .rev()
                .find(|j| j.name == name)
                .map(|j| &j.state)
            {
                text.push_str(" – ");
                text.push_str(&crate::text::reason(self.lang, e));
            }
            // The open project's export done in the queue: step 6 shows it done, as a direct one.
            let out = self
                .queue
                .jobs
                .iter()
                .rev()
                .find(|j| j.name == name)
                .map(|j| j.out.clone());
            if ok
                && let (Some(out), Some(p)) = (out, &mut self.project)
                && p.target() == out
            {
                p.exported = Some(out);
                // What was checked before the queue made it again no longer holds.
                p.verified = None;
            }
            self.say(text, !ok);
        }
        // Kept over a restart (written only when a job came, went or changed its state).
        self.queue.save_at(self.recent.dir());
        if self.queue.busy() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(300));
        }
        if let Some(p) = &self.project {
            let (cursor, n) = (p.cursor, p.frames());
            self.previews.trim(cursor, n);
        }
        self.drop_folders(ui.ctx());
        self.drive();
        if self.project.is_some() {
            crate::pages::show(self, ui);
        } else {
            crate::start::show(self, ui);
        }
    }

    /// A folder dropped on the window opens as a project.
    fn drop_folders(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if let Some(path) = dropped.into_iter().next() {
            let folder = if path.is_dir() {
                path
            } else {
                path.parent().map(Path::to_path_buf).unwrap_or(path)
            };
            self.open_folder(&folder);
        }
    }

    /// Has a headless run reached its step (and the step's work started)?
    #[must_use]
    pub fn autopilot_done(&self) -> bool {
        let Some(target) = self.autopilot else { return true };
        self.project
            .as_ref()
            .is_none_or(|p| p.seq.is_some() && p.step >= target)
    }

    /// Headless runs: go on to the wanted step as soon as the work of the last one is done.
    fn drive(&mut self) {
        let Some(target) = self.autopilot else { return };
        if self.task.is_some() {
            return;
        }
        let Some(p) = &self.project else { return };
        if p.seq.is_none() {
            return;
        }
        if p.step < target {
            if p.step == 3 && p.edit.is_none() {
                let first = p
                    .analysis
                    .as_ref()
                    .and_then(|a| a.suggested_keyframes().get(1).copied());
                if let Some(f) = first {
                    self.edit_frame(f);
                }
            }
            self.next();
        } else if p.step == 3
            && self.autopilot_demo
            && p.edit.as_ref().is_none_or(|e| !e.dirty())
            && !self.autopilot_edited
        {
            // Show editing: the second suggested keyframe, a little brighter.
            let second = p
                .analysis
                .as_ref()
                .and_then(|a| a.suggested_keyframes().get(1).copied());
            if let Some(f) = second {
                self.autopilot_edited = true;
                self.edit_frame(f);
                self.set_control("light.exposure", 0.6);
                self.set_control("light.contrast", 20.0);
                self.add_mask(lapsewerk_develop::masks::Kind::Linear);
            }
        } else if p.step == 3 && p.edit.is_some() && self.autopilot_brush {
            self.autopilot_brush = false;
            self.add_mask(lapsewerk_develop::masks::Kind::Brush);
            let i = self
                .project
                .as_ref()
                .and_then(|p| p.edit.as_ref())
                .and_then(|e| e.mask);
            if let Some(i) = i {
                let stroke: Vec<(f64, f64)> = (0..30)
                    .map(|k| (0.2 + 0.02 * f64::from(k), 0.7 - 0.005 * f64::from(k)))
                    .collect();
                self.mask_actions(vec![crate::masks_ui::Action::Stroke(i, stroke)]);
            }
        } else if p.step == 3 && p.edit.is_some() && self.autopilot_copy {
            self.autopilot_copy = false;
            self.open_copy();
        } else if p.step == 6 && self.autopilot_export {
            self.autopilot_export = false;
            self.start_export(false);
        } else if p.step == 6 && self.autopilot_verify && self.task.is_none() {
            self.autopilot_verify = false;
            self.start_verify();
        } else if p.step == 6 && self.autopilot_queue {
            self.autopilot_queue = false;
            self.queue_export();
        } else if p.step == 5 && p.corrected.is_some() && self.autopilot_measure && self.task.is_none() {
            self.autopilot_measure = false;
            self.start_measure_result();
        } else if p.step == 5 && p.corrected.is_some() && self.autopilot_proof {
            self.autopilot_proof = false;
            self.start_export(true);
        } else if p.step == 5 && self.autopilot_mode.is_some() {
            let mode = self.autopilot_mode.take();
            if let (Some(mode), Some(p)) = (mode, &mut self.project) {
                p.mode = mode;
                p.cursor = p.frames() / 2;
            }
        } else if p.step == 5
            && p.corrected.is_some()
            && self.previews.bulk.is_none()
            && p.mode == PreviewMode::Play
        {
            let offsets = p.offsets();
            let (v, n) = (p.video_version, p.frames());
            self.previews.want_play(Kind::Video(v), &offsets, n, p.cursor);
        }
    }
}

/// `v` as written for the command line (four decimal places) and read back there.
#[must_use]
pub fn as_written(v: f64) -> f64 {
    format!("{v:.4}").parse().unwrap_or(v)
}

/// A fingerprint of `frame`'s sidecar as it is on disk (empty without one).
fn sidecar_print(seq: &lapsewerk_develop::Sequence, frame: usize) -> String {
    lapsewerk_studio::cache::sidecar_print(seq, frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_steps_back_one_drag_at_a_time() {
        let mut e = Edit::new(3, DevelopSettings::default(), 0);
        let set = |e: &mut Edit, id: &str, v: f64| {
            let mut s = e.settings.clone();
            assert!(lapsewerk_develop::controls::set(&mut s, id, v));
            e.before_change(id);
            e.settings = s;
        };
        let exposure =
            |e: &Edit| lapsewerk_develop::controls::get(&e.settings, "light.exposure").unwrap_or(f64::NAN);
        // One drag (quick changes of one control), then another control.
        for v in [0.1, 0.2, 0.3] {
            set(&mut e, "light.exposure", v);
        }
        set(&mut e, "light.contrast", 20.0);
        assert_eq!(e.undo.len(), 2);
        assert!(e.step(true));
        assert_eq!(exposure(&e), 0.3);
        assert!(e.step(true));
        assert_eq!(exposure(&e), 0.0);
        assert!(!e.dirty());
        assert!(!e.step(true), "nothing left to undo");
        assert!(e.step(false));
        assert_eq!(exposure(&e), 0.3);
        // A new change drops what was undone.
        set(&mut e, "light.exposure", 1.0);
        assert!(e.redo.is_empty());
    }

    /// A folder of `n` unique copies of the test JPEG.
    fn frames(name: &str, n: usize) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lapsewerk-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..n {
            let mut bytes = include_bytes!("../../../crates/develop/tests/data/frame.jpg").to_vec();
            bytes.extend_from_slice(format!("frame {i}").as_bytes());
            std::fs::write(dir.join(format!("f{i:03}.jpg")), bytes).unwrap();
        }
        dir
    }

    /// Runs the app's frames headless until it is idle (or 120 s).
    fn settle(app: &mut App, ctx: &egui::Context) {
        let started = Instant::now();
        let mut quiet = 0;
        while quiet < 3 && started.elapsed().as_secs() < 120 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1440.0, 900.0),
                )),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| app.frame(ui));
            // Nothing draws here: the texture updates are not needed.
            out.textures_delta.clear();
            quiet = if app.idle() && app.autopilot_done() {
                quiet + 1
            } else {
                0
            };
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn brush_strokes_paint_and_undo_one_at_a_time() {
        use crate::masks_ui::Action;
        use lapsewerk_develop::masks::{self, Kind};
        let config = std::env::temp_dir().join(format!("lapsewerk-app-config-brush-{}", std::process::id()));
        let dir = frames("brush", 3);
        let ctx = egui::Context::default();
        let mut app = App::with_recent(&ctx, Recent::load_at(Some(config.clone())));
        app.autopilot = Some(3);
        app.open_folder(&dir);
        settle(&mut app, &ctx);
        app.add_mask(Kind::Brush);
        let i = app
            .project
            .as_ref()
            .and_then(|p| p.edit.as_ref())
            .and_then(|e| e.mask)
            .unwrap();
        let strokes = |app: &App| {
            app.project
                .as_ref()
                .and_then(|p| p.edit.as_ref())
                .and_then(|e| e.settings.masks.get(i))
                .map_or(0, |m| masks::strokes(m).len())
        };
        app.mask_actions(vec![Action::Tool(crate::masks_ui::BrushTool {
            size: 0.1,
            feather: 20.0,
            erase: false,
        })]);
        app.mask_actions(vec![Action::Stroke(i, vec![(0.2, 0.2), (0.4, 0.3)])]);
        app.mask_actions(vec![Action::Stroke(i, vec![(0.6, 0.6)])]);
        assert_eq!(strokes(&app), 2);
        let m = app
            .project
            .as_ref()
            .and_then(|p| p.edit.as_ref())
            .and_then(|e| e.settings.masks.get(i))
            .unwrap();
        assert_eq!(masks::strokes(m)[0].1, 0.1, "painted with the brush's size");
        app.undo(true);
        assert_eq!(strokes(&app), 1, "one stroke a step");
        app.mask_actions(vec![Action::ClearStrokes(i)]);
        assert_eq!(strokes(&app), 0);
        app.undo(true);
        assert_eq!(strokes(&app), 1);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn auto_and_the_white_balance_picker_are_one_step_each() {
        let config = std::env::temp_dir().join(format!("lapsewerk-app-config-auto-{}", std::process::id()));
        let dir = frames("auto", 3);
        let ctx = egui::Context::default();
        let mut app = App::with_recent(&ctx, Recent::load_at(Some(config.clone())));
        app.autopilot = Some(3);
        app.open_folder(&dir);
        settle(&mut app, &ctx);
        let steps = |app: &App| {
            app.project
                .as_ref()
                .and_then(|p| p.edit.as_ref())
                .map_or(0, |e| e.undo.len())
        };
        let before = steps(&app);
        // A white balance far off first, so the picker surely changes it.
        app.set_controls(&[("wb.temp", 3000.0), ("wb.tint", 60.0)], "test");
        assert_eq!(steps(&app), before + 1);
        app.white_balance(Some((0.5, 0.5)));
        assert_eq!(steps(&app), before + 2, "both values in one step");
        let e = app.project.as_ref().and_then(|p| p.edit.as_ref()).unwrap();
        let temp = lapsewerk_develop::controls::get(&e.settings, "wb.temp").unwrap();
        assert!(
            (temp - 3000.0).abs() > 1.0,
            "the picker set the temperature ({temp})"
        );
        assert!(
            !app.project.as_ref().unwrap().picking_wb,
            "picking ends with the pick"
        );
        app.auto_tone();
        assert!(steps(&app) <= before + 3);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn the_command_line_gets_the_transfer_the_app_renders_with() {
        let config = std::env::temp_dir().join(format!("lapsewerk-app-config-tr-{}", std::process::id()));
        let dir = frames("transfer", 3);
        let ctx = egui::Context::default();
        let mut app = App::with_recent(&ctx, Recent::load_at(Some(config.clone())));
        app.autopilot = Some(1);
        app.open_folder(&dir);
        settle(&mut app, &ctx);
        let p = app.project.as_mut().unwrap();
        for (preset, sequence, rec709) in [
            (2, true, None),
            (0, true, None),
            (0, false, Some(false)),
            (2, false, Some(true)),
        ] {
            p.preset = preset;
            p.sequence = sequence;
            p.rec709 = rec709;
            let want = match p.render_args(false).video.transfer {
                Transfer::Rec709 => "rec709",
                Transfer::Srgb => "srgb",
                _ => "hdr",
            };
            let args = p.cli_args();
            let at = args
                .iter()
                .position(|a| a == "--transfer")
                .expect("--transfer given");
            assert_eq!(
                args[at + 1],
                want,
                "preset {preset}, sequence {sequence}, rec709 {rec709:?}"
            );
        }
        // HLG chosen in the app: rendered and handed on as HLG, with its whole headroom.
        p.hdr = true;
        p.hlg = true;
        assert!(matches!(
            p.render_args(false).video.transfer,
            Transfer::Hlg { .. }
        ));
        let args = p.cli_args();
        assert!(
            args.windows(2).any(|w| w[0] == "--hdr" && w[1] == "hlg"),
            "{args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--hdr-peak" || a == "--transfer"),
            "{args:?}"
        );
        p.hlg = false;
        assert!(matches!(p.render_args(false).video.transfer, Transfer::Pq { .. }));
        assert!(p.cli_args().windows(2).any(|w| w[0] == "--hdr" && w[1] == "pq"));
        p.hdr = false;
        // The timecode goes where it is carried (a movie, DPX, EXR) and is checked against the rate.
        p.fps = 25;
        p.ntsc = false;
        p.timecode = "01:00:00:00".into();
        p.sequence = false;
        assert!(p.timecode_ok());
        assert!(
            p.cli_args()
                .windows(2)
                .any(|w| w[0] == "--timecode" && w[1] == "01:00:00:00")
        );
        assert_eq!(p.render_args(false).video.timecode, Some(90_000));
        // What a resume is checked for before measuring: settings alone, as the frames' file says them.
        let early = lapsewerk_studio::export::early_settings_words(&p.render_args(false));
        for key in ["size: ", "frame rate: 25/1", "timecode: 01:00:00:00", "curve: "] {
            assert!(early.contains(key), "{early}");
        }
        for later in ["frames: ", "deflicker: ", "repairs: ", "keyframe "] {
            assert!(!early.lines().any(|l| l.starts_with(later)), "{early}");
        }
        p.sequence = true;
        p.frame_format = FrameFormat::Tiff;
        assert!(
            !p.cli_args().iter().any(|a| a == "--timecode"),
            "TIFF has no timecode"
        );
        p.frame_format = FrameFormat::Dpx;
        assert!(p.cli_args().iter().any(|a| a == "--timecode"));
        for bad in ["01:00:00:25", "01:00:00;00", "1:00"] {
            p.timecode = bad.into();
            assert!(!p.timecode_ok(), "{bad}");
        }
        p.timecode.clear();
        assert!(p.timecode_ok());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn a_copied_project_exports_into_the_copy() {
        let config = std::env::temp_dir().join(format!("lapsewerk-app-config-copy-{}", std::process::id()));
        let dir = frames("copyproject", 3);
        let ctx = egui::Context::default();
        let mut app = App::with_recent(&ctx, Recent::load_at(Some(config.clone())));
        app.autopilot = Some(1);
        app.open_folder(&dir);
        settle(&mut app, &ctx);
        app.save_project_settings();
        // The same folder name elsewhere: a copy (another disk, a backup).
        let copy = std::env::temp_dir()
            .join(format!("lapsewerk-copy-{}", std::process::id()))
            .join(dir.file_name().unwrap());
        let _ = std::fs::remove_dir_all(copy.parent().unwrap());
        std::fs::create_dir_all(&copy).unwrap();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            std::fs::copy(e.path(), copy.join(e.file_name())).unwrap();
        }
        app.close_project();
        app.open_folder(&copy);
        settle(&mut app, &ctx);
        let out = app.project.as_ref().map(|p| p.out.clone()).unwrap();
        assert!(out.starts_with(&copy), "{} should be in the copy", out.display());
        // A copy under another name: the export takes the new name.
        let renamed = copy.parent().unwrap().join("Sky");
        std::fs::create_dir_all(&renamed).unwrap();
        for e in std::fs::read_dir(&copy).unwrap().flatten() {
            if e.path().is_file() {
                std::fs::copy(e.path(), renamed.join(e.file_name())).unwrap();
            }
        }
        app.close_project();
        app.open_folder(&renamed);
        settle(&mut app, &ctx);
        let out = app.project.as_ref().map(|p| p.target()).unwrap();
        // (a folder of frames here, a movie on a Mac: the name either way)
        assert_eq!(out.file_stem().unwrap(), "Sky", "{}", out.display());
        assert!(out.starts_with(&renamed), "{}", out.display());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(copy.parent().unwrap());
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn a_keyframe_saved_in_lightcraft_is_taken_over() {
        let config = std::env::temp_dir().join(format!("lapsewerk-app-config-lc-{}", std::process::id()));
        let dir = frames("lightcraft", 4);
        let ctx = egui::Context::default();
        let mut app = App::with_recent(&ctx, Recent::load_at(Some(config.clone())));
        // A stand-in for LightCraft that starts and ends.
        app.lightcraft = Some("true".into());
        app.autopilot = Some(3);
        app.open_folder(&dir);
        settle(&mut app, &ctx);
        let frame = app
            .project
            .as_ref()
            .and_then(|p| p.edit.as_ref())
            .map(|e| e.frame)
            .unwrap();
        app.open_in_lightcraft();
        let sidecar = app
            .project
            .as_ref()
            .and_then(|p| p.external.as_ref())
            .map(|x| x.sidecar.clone())
            .unwrap();
        assert!(sidecar.exists(), "the keyframe was saved before opening");
        // "LightCraft" saves the keyframe: clarity 42, written by another session.
        {
            let paths: Vec<PathBuf> = (0..4).map(|i| dir.join(format!("f{i:03}.jpg"))).collect();
            let mut other = lapsewerk_develop::Sequence::open(&paths).unwrap();
            let mut d = other.settings(frame).unwrap();
            lapsewerk_develop::controls::set(&mut d, "effects.clarity", 42.0);
            std::thread::sleep(std::time::Duration::from_millis(1100));
            other.save_keyframe(frame, d).unwrap();
        }
        if let Some(x) = app.project.as_mut().and_then(|p| p.external.as_mut()) {
            x.checked = Instant::now() - std::time::Duration::from_secs(2);
        }
        app.poll();
        let e = app.project.as_ref().and_then(|p| p.edit.as_ref()).unwrap();
        assert_eq!(
            lapsewerk_develop::controls::get(&e.settings, "effects.clarity"),
            Some(42.0)
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn keyframes_saved_outside_together_are_all_taken_over() {
        let config = std::env::temp_dir().join(format!("lapsewerk-app-config-all-{}", std::process::id()));
        let dir = frames("allkeyframes", 4);
        let paths: Vec<PathBuf> = (0..4).map(|i| dir.join(format!("f{i:03}.jpg"))).collect();
        {
            let mut seq = lapsewerk_develop::Sequence::open(&paths).unwrap();
            for f in [0, 3] {
                let d = seq.settings(f).unwrap();
                seq.save_keyframe(f, d).unwrap();
            }
        }
        let ctx = egui::Context::default();
        let mut app = App::with_recent(&ctx, Recent::load_at(Some(config.clone())));
        app.open_folder(&dir);
        settle(&mut app, &ctx);
        let look = |app: &mut App| {
            if let Some(p) = app.project.as_mut() {
                p.watched_at = Instant::now() - std::time::Duration::from_secs(3);
            }
            app.poll();
        };
        // The sidecars' times noted.
        look(&mut app);
        // Both saved in LightCraft at once ("save metadata" on a selection).
        {
            let mut other = lapsewerk_develop::Sequence::open(&paths).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1100));
            for f in [0, 3] {
                let mut d = other.settings(f).unwrap();
                lapsewerk_develop::controls::set(&mut d, "effects.clarity", 42.0);
                other.save_keyframe(f, d).unwrap();
            }
        }
        look(&mut app);
        let seq = app.project.as_ref().and_then(|p| p.seq.clone()).unwrap();
        for f in [0, 3] {
            let d = lock(&seq).settings(f).unwrap();
            assert_eq!(
                lapsewerk_develop::controls::get(&d, "effects.clarity"),
                Some(42.0),
                "keyframe {f}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn a_keyframe_saved_outside_before_the_first_look_is_taken_over() {
        // Saved while the app measured (it does not look then): the first look reads it anyway.
        let config = std::env::temp_dir().join(format!("lapsewerk-app-config-early-{}", std::process::id()));
        let dir = frames("earlysave", 4);
        let paths: Vec<PathBuf> = (0..4).map(|i| dir.join(format!("f{i:03}.jpg"))).collect();
        {
            let mut seq = lapsewerk_develop::Sequence::open(&paths).unwrap();
            for f in [0, 3] {
                let d = seq.settings(f).unwrap();
                seq.save_keyframe(f, d).unwrap();
            }
        }
        let ctx = egui::Context::default();
        let mut app = App::with_recent(&ctx, Recent::load_at(Some(config.clone())));
        app.open_folder(&dir);
        settle(&mut app, &ctx);
        {
            let mut other = lapsewerk_develop::Sequence::open(&paths).unwrap();
            let mut d = other.settings(3).unwrap();
            lapsewerk_develop::controls::set(&mut d, "effects.clarity", 42.0);
            other.save_keyframe(3, d).unwrap();
        }
        if let Some(p) = app.project.as_mut() {
            p.watched.clear();
            p.watched_at = Instant::now() - std::time::Duration::from_secs(3);
        }
        app.poll();
        let seq = app.project.as_ref().and_then(|p| p.seq.clone()).unwrap();
        let d = lock(&seq).settings(3).unwrap();
        assert_eq!(
            lapsewerk_develop::controls::get(&d, "effects.clarity"),
            Some(42.0)
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn a_project_goes_from_the_photos_to_the_preview() {
        let config = std::env::temp_dir().join(format!("lapsewerk-app-config-{}", std::process::id()));
        let dir = frames("flow", 6);
        let ctx = egui::Context::default();
        let mut app = App::with_recent(&ctx, Recent::load_at(Some(config.clone())));
        app.autopilot = Some(5);
        app.open_folder(&dir);
        settle(&mut app, &ctx);
        let p = app.project.as_ref().unwrap();
        assert_eq!(p.step, 5, "reached the preview");
        assert_eq!(p.frames(), 6);
        assert!(p.analysis.is_some() && p.corrected.is_some());
        assert_eq!(p.offsets().len(), 6);
        // The project remembers itself: in the list, and its settings beside the photos.
        assert_eq!(
            app.recent.entries.first().map(|e| e.folder.clone()),
            Some(dir.clone())
        );
        // "Measure result": a figure for the finished look; the preview's corrections stay.
        let before = app.project.as_ref().map(|p| p.video_version);
        app.start_measure_result();
        settle(&mut app, &ctx);
        let p = app.project.as_ref().unwrap();
        assert!(p.measured.is_some_and(|(v, _)| v.is_finite()), "{:?}", p.measured);
        assert_eq!(Some(p.video_version), before);
        app.save_project_settings();
        let file = ProjectFile::load(&dir).unwrap().unwrap();
        assert_eq!(
            file.app.and_then(|a| a.get("reached").and_then(Value::as_u64)),
            Some(5)
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&config);
    }
}
