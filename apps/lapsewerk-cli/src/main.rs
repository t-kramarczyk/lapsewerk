//! Lapsewerk on the command line.
//!
//! - `lapsewerk import <folder>`: reads the frames' capture times and camera settings and writes the
//!   project file (`lapsewerk.json`) beside them.
//! - `lapsewerk analyze <folder>`: timing, exposure steps and flicker of a sequence, as a summary and
//!   a CSV.
//! - `lapsewerk render <folder> --out FILE.mov`: the same analysis, then every frame developed with its
//!   keyframes, the camera's exposure steps undone and the flicker taken out, written as a movie.

// Messages never stop a render: when stderr or stdout is a pipe that closed (`| head`, a `tee`
// that died), std's `eprintln!`/`println!` panic; these drop the text instead. Declared before the
// modules, so they replace the standard macros in the whole binary.
macro_rules! eprintln {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($t)*);
    }};
}
macro_rules! println {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($t)*);
    }};
}

mod bench;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::Instant;

use lapsewerk_core::sequence;
use lapsewerk_develop::{DevelopPlan, Sequence};
use lapsewerk_studio::{
    Analysis, MeasureSettings, OpenSettings, Pass, Remaining, RenderArgs, Reporter, Step, apply, jitter,
};
use lapsewerk_video::{Codec, Transfer, VideoSettings};

/// Prints notes to stdout and progress to stderr, as the command line always has.
#[derive(Default)]
struct Console {
    clocks: Mutex<HashMap<Step, Remaining>>,
    /// The last step finished that was said ("checked 30/30" once, though measured in two goes).
    said_done: Mutex<Option<(Step, usize)>>,
}

impl Reporter for Console {
    fn progress(&self, step: Step, done: usize, total: usize) {
        let Ok(mut clocks) = self.clocks.lock() else {
            return;
        };
        // For a program running this one (the app's render queue): every step, as it happens.
        if std::env::var_os("LAPSEWERK_PROGRESS_LINES").is_some() {
            eprintln!("progress {} {done} {total}", step.label());
        }
        if done == 0 {
            clocks.insert(step, Remaining::start());
            return;
        }
        match step {
            Step::Render if done.is_multiple_of(25) || done == total => {
                let clock = clocks.entry(step).or_insert_with(Remaining::start);
                let rate = done as f64 / clock.elapsed().as_secs_f64().max(1e-9);
                let left = clock.left(done, total).map_or(0.0, |d| d.as_secs_f64());
                eprintln!("lapsewerk: rendered {done}/{total} ({rate:.1} frames/s, {left:.0} s left)");
            }
            Step::Measure | Step::Check | Step::Verify if done.is_multiple_of(50) || done == total => {
                if done == total
                    && let Ok(mut said) = self.said_done.lock()
                {
                    if *said == Some((step, total)) {
                        return;
                    }
                    *said = Some((step, total));
                }
                eprintln!("lapsewerk: {} {done}/{total}", step.label());
            }
            _ => {}
        }
    }

    fn note(&self, line: &str) {
        println!("{line}");
    }
}

const USAGE: &str = "\
usage:
  lapsewerk import  <folder>   (frames, timing, exposure steps → <folder>/lapsewerk.json)
  lapsewerk analyze <folder> [--csv FILE] [--write-keyframes] [OPTIONS]
  lapsewerk bench   <folder> [--frames N] [--size WxH]   (where render time goes)
  lapsewerk calibrate <folder>… [--out DIR]   (a colour profile of your camera from many of your raw
                    photos with their camera JPEG, at least 5 of a model, the more varied the better;
                    written where LightCraft and Lapsewerk read it)
  lapsewerk verify  <frames folder>   (every frame against its SHA256SUMS.txt; exit 1 when one differs)
  lapsewerk render  <folder> --from-project [OPTIONS]   (exactly as set in the app; options given win)
  lapsewerk render  <folder> --out FILE.mov [--size WxH] [--position X,Y] [--move-from X,Y,ZOOM] [--move-to X,Y,ZOOM] [--fps 25] [--codec prores422hq|prores422|prores4444|hevc]
                    [--bitrate BPS] [--transfer srgb|rec709] [--hdr pq|hlg] [--hdr-peak EV] [--deflicker N] [--passes 1|2] [--verify] [--regional 6x4] [--denoise R] [--blend N] [--tween N] [--stabilize] [--dust] [--light-pollution F] [--reduce-stars F] [--star-trails N] [--sky X,Y,W,H] [--replace-outliers] [--fill-gaps] [--keep-light F] [--auto-brightness] [--base-look] [--render-jobs N] [--overwrite] [--sequence] [--frames tiff|dpx|exr] [--name NAME] [--timecode HH:MM:SS:FF] [--movie] [--resume] [--fit-orientation] [--subpixel] [--replace-frames N,N] [OPTIONS]
analyze:
  --write-keyframes    make the suggested keyframes: an XMP sidecar, to edit in LightCraft, beside
                       each suggested frame that has none (existing sidecars are never changed)
options:
  --edge PX            size of the measuring renders (768)
  --jobs N             measuring threads
  --look FRAME         develop every frame with one look: this frame's camera look, else another's, else none (default 0)
  --per-frame-look     each frame with its own look
  --keep-hot-pixels    leave hot pixels as they are (by default they are found and repaired)
  --reference X,Y,W,H  measure brightness for deflicker only in this part of the frame, as fractions
                       of the developed frame from the left and top: 0,0,1,0.4 is the upper 40 %
                       (the sky, without people, traffic or water moving below it)
render:
  --from-project       the settings the app saved for this folder (lapsewerk.json); options after
                       it change single ones
  --out PATH           the movie (.mov), or with --sequence the folder for the frames
  --size WxH           video size; default 3840x2160, or 2160x3840 when the frames are upright
  --fps N              frames a second (25; 23.976, 29.97, 59.94 or 30000/1001 for NTSC rates)
  --codec C            prores422hq (default), prores422, prores4444 or hevc (movies, Mac only)
  --bitrate BPS        HEVC bit rate (default about 0.2 bits a pixel)
  --transfer T         srgb, or rec709 (BT.1886, gamma 2.4: the default for ProRes and for the
                       command line's default codec); in viewers without colour management Rec. 709
                       frames look lighter, which is right on a video monitor
  --passes 1|2         2 (default): a check pass measures the frames as rendered and corrects what
                       is left
  --verify             measure the finished corrections once more and report the flicker left
  --render-jobs N      frames rendered at a time (2; more is faster and takes more memory)
  --overwrite          replace the movie or the frames there
  --brightness F:S,…   shape the video's brightness by hand: at frame F (from 1) S stops brighter
                       (or darker), smooth in between, held before the first and after the last
                       point (e.g. 1:0,300:+0.8,600:+0.3); added after deflicker and day to night
  --auto-brightness    lift a sequence shown too dark to a readable brightness, once for all
                       frames and never darker: a night as shot, or a day whose brightest frames
                       show under the day level (a starry night: its sky to about 30 of 255);
                       nothing happens when --brightness is drawn
  --base-look          a look for keyframes nobody edited: shadows opened, highlights held,
                       some contrast and vibrance (the app uses it until a keyframe is edited);
                       left out, and said, when a keyframe is edited (another raw editor, LightCraft, app)
  --no-hold-blacks     let blacks rise with a frame shown brighter than shot (by default the
                       brightening of day to night and the camera's steps leaves black black)
  --no-space-check     render even when the destination looks too full (checked before measuring)
  --position X,Y       where the video sits in a frame that is cropped to fit, 0..1 from the
                       left and top (default 0.5,0.5: centred; 0.5,1: the bottom)
  --move-from X,Y,ZOOM a camera move: the view on the first frame, position 0..1 as --position,
                       zoom 1..2 (2: half as wide), eased to --move-to on the last frame; one of
                       the two alone moves from or to --position unzoomed
  --deflicker N        deflicker smoothing in frames (default 8; larger is steadier but follows
                       quick real changes such as cloud shadows less; 0: no deflicker)
  --tween N            N frames in between every two, along the motion (optical flow): smoother
                       clouds and traffic, or a slower video (1-3; the movie gets N+1 times longer)
  --subpixel           with --stabilize: shift by the exact amount (resampled) instead of whole
                       pixels: steadier, slightly softer
  --fit-orientation    with --size: turn a landscape size upright for upright frames (as the app's
                       presets); an upright size stays as it is
  --replace-frames L   frames (from 1) replaced by their neighbours, as marked by hand in the app
  --sequence           write the video as a folder of 16-bit TIFF frames (--out is the folder) instead
                       of a movie: works on every system, for Resolve, Premiere, After Effects
  --frames tiff|dpx|exr with --sequence: 16-bit TIFF (default), 10-bit DPX, or OpenEXR (half float,
                       linear light: SDR white 1.0, HDR highlights above it; for grading)
  --name NAME          with --sequence: frames named NAME_000001.tif … instead of after the folder
                       (letters A-Z, digits, '-', '_', '.'); kept for --resume and verify
  --timecode HH:MM:SS:FF  the timecode of the first frame (e.g. 01:00:00:00 for a broadcast
                       master): a timecode track in movies, in each DPX / EXR frame's header;
                       HH:MM:SS;FF is drop-frame (29.97, 59.94)
  --movie              a movie file after all (when the project's settings say --sequence)
  --resume             with --sequence: keep the frames a stopped render left and go on after them
                       (with repairs too; not with denoise, blend, star work or --tween; the
                       settings must be those of the frames there)
  --regional CxR       regional deflicker on a grid of C × R tiles (needs --passes 2)
  --hdr pq|hlg         HDR video (Rec. 2020): frames rendered with LightCraft's HDR headroom, SDR
                       white at 203 cd/m². pq: HDR10 (YouTube, Apple TV); hlg: broadcast, also looks
                       right on SDR screens. A movie needs ProRes or HEVC (10 bit); frames
                       (--sequence) work in every format, DPX and EXR name it in their headers
  --hdr-peak EV        headroom above SDR white in stops (pq: default 3, at most 5; hlg: default and
                       at most 2.3, its 1000 cd/m²)
  --keep-light F       day to night: keep this fraction (0-1) of the scene's real change of light
                       (0: as bright throughout, 1: night as dark as it was; 0.15 is the app's)
  --stabilize          remove camera shake (renders 4 % larger and crops against the shake)
  --dust               remove sensor dust: soft dark spots in the same place in every frame, found
                       where the scene is smooth (sky, mist, still water)
  --blend N            motion blur: each frame blended with its neighbours, N frames in all (2-8)
  --light-pollution F  night sky: take this fraction (0-1) of the light pollution out: a smooth glow
                       over the sky (a town's dome, the moon) fitted per frame, averaged over 13
                       frames; its bright side is brought down to the sky's dark side
  --sky X,Y,W,H        the sky, fractions of the video frame: where --light-pollution is fitted
                       (default 0,0,1,0.6: the upper 60 %) and where --star-trails form (with the
                       sky found in the first frame: within both); keep the foreground out of it
  --reduce-stars F     dim the stars by this fraction (0-1; 0.5 calms a busy sky)
  --star-trails N      star trails, fading over N frames (0: endless trails)
  --replace-outliers   replace frames that stand out from their neighbours (a flash, headlights)
                       by the neighbours blended
  --fill-gaps          put blended frames into pauses of the capture (a card change), so the
                       video does not jump there
  --denoise R          temporal noise reduction over R frames on each side (1-4; 2 is a good start)
  --denoise-tolerance T  how much change counts as noise, in multiples of it (default 1; 2 averages more)";

struct Args {
    /// Check that the export fits on its disk before measuring (`--no-space-check`: don't).
    space_check: bool,
    folder: PathBuf,
    csv: Option<PathBuf>,
    edge: usize,
    jobs: usize,
    /// Frame whose camera look every frame is developed with; `None`: each frame's own.
    look: Option<usize>,
    hot_pixels: bool,
    /// Area the deflicker measures in (`--reference`); `None`: the whole frame.
    reference: Option<lapsewerk_core::deflicker::Rect>,
    /// Write sidecars for the suggested keyframes (`analyze` only).
    write_keyframes: bool,
    /// The frames' name (`--name`), when not the folder's.
    frame_name: Option<String>,
    render: Option<RenderArgs>,
}

/// `--option=value` as `--option value`, as most programs take it (`--name=SHOT010`); a lone
/// `--` or a value without an option stays as it is.
fn split_equals(args: Vec<String>) -> Vec<String> {
    args.into_iter()
        .flat_map(|a| match a.strip_prefix("--").and_then(|r| r.split_once('=')) {
            Some((opt, value)) if !opt.is_empty() => vec![format!("--{opt}"), value.to_owned()],
            _ => vec![a],
        })
        .collect()
}

fn main() -> ExitCode {
    // Interrupted (Ctrl-C, `kill`): the process ends without running destructors, so the movie being
    // written is stopped and its hidden partial file deleted here.
    if let Err(e) = ctrlc::set_handler(|| {
        let movies = lapsewerk_video::abort_all();
        // Its folders free again at once (another computer need not wait for them to go stale).
        let folders = lapsewerk_video::frames::release_all_locks();
        eprintln!(
            "\nlapsewerk: interrupted{}",
            if movies > 0 { "; partial movie deleted" } else { "" }
        );
        // A frame sequence stopped: the frames written stay, and going on makes only the rest.
        for d in folders {
            let stem = d
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            use lapsewerk_video::frames::{FrameFormat, finished_frames_as};
            if [FrameFormat::Tiff, FrameFormat::Dpx, FrameFormat::Exr]
                .into_iter()
                .any(|f| finished_frames_as(&d, &stem, f) > 0)
            {
                eprintln!(
                    "lapsewerk: the frames written in {} stay: the same command with --resume goes on from there",
                    d.display()
                );
            } else {
                // Stopped while measuring: nothing written; an empty folder made for it goes again.
                let _ = std::fs::remove_dir(&d);
            }
        }
        std::process::exit(130);
    }) {
        eprintln!(
            "lapsewerk: no Ctrl-C handler ({e}); an interrupted render may leave a hidden .partial.mov"
        );
    }
    let args: Vec<String> = match from_project(split_equals(std::env::args().skip(1).collect())) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("lapsewerk: {e}");
            return ExitCode::FAILURE;
        }
    };
    if args
        .first()
        .is_some_and(|a| a == "--version" || a == "-V" || a == "version")
    {
        println!("lapsewerk {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    // Help asked anywhere (`render --help` too): the usage, not an unknown option.
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let rest = args.get(1..).unwrap_or_default();
    let result = match args.first().map(String::as_str) {
        Some("analyze") => parse(rest, false).and_then(|a| run(&a)),
        Some("render") => parse(rest, true).and_then(|a| run(&a)),
        Some("bench") => bench::run(rest),
        Some("import") => import(rest),
        Some("calibrate") => calibrate(rest),
        Some("verify") => verify(rest),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        // Nothing asked: the usage, and a failure for a script that forgot its command.
        None => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
        Some(other) => Err(format!("unknown command `{other}`\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lapsewerk: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `render FOLDER --from-project [OPTIONS]`: the options the app saved for the folder
/// (`lapsewerk.json`, `app.render`), then those given (which win).
fn from_project(args: Vec<String>) -> Result<Vec<String>, String> {
    if !args.iter().any(|a| a == "--from-project") {
        return Ok(args);
    }
    if args.first().map(String::as_str) != Some("render") {
        return Err("--from-project goes with `lapsewerk render FOLDER`".into());
    }
    // The folder: the first word that is no option (its place kept: a value further on spelled
    // like it, such as `--name A` for a folder A, is a value, not the folder again).
    let folder_at = args
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, a)| !a.starts_with("--"))
        .map(|(i, _)| i)
        .ok_or("--from-project needs the folder")?;
    let folder = args
        .get(folder_at)
        .cloned()
        .ok_or("--from-project needs the folder")?;
    let project = lapsewerk_develop::project::Project::load(std::path::Path::new(&folder))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{folder} has no lapsewerk.json (open it in the app first)"))?;
    let saved: Vec<String> = project
        .app
        .as_ref()
        .and_then(|a| a.get("render"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .ok_or("the project has no render settings yet: export once in the app, or save it there")?;
    // A project copied or moved since: its output follows it (a path inside the folder it was
    // saved in, or into a folder of the same name elsewhere, was the original's).
    let here = std::path::Path::new(&folder);
    let was = project
        .app
        .as_ref()
        .and_then(|a| a.get("export"))
        .and_then(|e| e.get("folderPath"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    let here_abs = here.canonicalize().unwrap_or_else(|_| here.to_path_buf());
    let mut saved = saved;
    for k in 1..saved.len() {
        if saved.get(k - 1).map(String::as_str) != Some("--out") {
            continue;
        }
        if let Some(v) = saved.get_mut(k) {
            let p = std::path::PathBuf::from(&*v);
            if let Some(inside) = was
                .as_ref()
                .filter(|w| **w != here_abs)
                .and_then(|w| p.strip_prefix(w).ok())
            {
                *v = here.join(inside).to_string_lossy().into_owned();
                continue;
            }
            let elsewhere = p
                .parent()
                .is_some_and(|d| d != here && d.file_name().is_some() && d.file_name() == here.file_name());
            if elsewhere && let Some(name) = p.file_name() {
                *v = here.join(name).to_string_lossy().into_owned();
            }
        }
    }
    // Options given win: the project's same option goes, and those it contradicts.
    let given: Vec<&str> = args
        .iter()
        .skip(1)
        .filter(|a| a.starts_with("--") && *a != "--from-project")
        .map(String::as_str)
        .collect();
    let gives_tiff = args.windows(2).any(|w| {
        w.first().is_some_and(|a| a == "--frames")
            && w.get(1).is_some_and(|v| {
                lapsewerk_video::frames::FrameFormat::parse(v)
                    == Some(lapsewerk_video::frames::FrameFormat::Tiff)
            })
    });
    // Why the project's option goes: the same one given, or one it contradicts.
    let contradicted = |flag: &str| -> Option<&'static str> {
        if given.contains(&flag) {
            return Some("given on the command line");
        }
        match flag {
            "--transfer" if given.contains(&"--hdr") => Some("--hdr given (HDR has its own curve)"),
            "--hdr" | "--hdr-peak" if given.contains(&"--transfer") => Some("--transfer given"),
            "--timecode" if gives_tiff => Some("TIFF frames carry no timecode"),
            "--sequence" | "--frames" | "--resume" if given.contains(&"--movie") => Some("--movie given"),
            "--codec" | "--bitrate" if given.contains(&"--sequence") => Some("--sequence given"),
            _ => None,
        }
    };
    let mut kept = Vec::with_capacity(saved.len());
    let mut k = 0;
    while let Some(a) = saved.get(k) {
        let takes_value = saved.get(k + 1).is_some_and(|v| !v.starts_with("--"));
        let width = if a.starts_with("--") && takes_value { 2 } else { 1 };
        if let Some(why) = a.starts_with("--").then(|| contradicted(a)).flatten() {
            eprintln!(
                "lapsewerk: the project's {} left out: {why}",
                saved.get(k..k + width).map(|s| s.join(" ")).unwrap_or_default()
            );
        } else {
            kept.extend(saved.get(k..k + width).unwrap_or_default().iter().cloned());
        }
        k += width;
    }
    let mut out = vec!["render".to_owned(), folder.clone()];
    out.extend(kept);
    out.extend(
        args.into_iter()
            .enumerate()
            .skip(1)
            .filter(|(i, a)| a != "--from-project" && *i != folder_at)
            .map(|(_, a)| a),
    );
    // INFO.txt's settings line: this command, as a shell takes it back, the output absolute and
    // without what never changes a pixel (threads).
    let quote = |a: &str| {
        if !a.is_empty()
            && a.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_./:=,+-@%".contains(c))
        {
            a.to_owned()
        } else {
            format!("'{}'", a.replace('\'', "'\\''"))
        }
    };
    // As a shell takes it back (copied from a log, it runs as it ran).
    eprintln!(
        "lapsewerk: {}",
        out.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
    );
    let mut spelled = Vec::new();
    let mut k = 0;
    while let Some(a) = out.get(k) {
        match a.as_str() {
            "--render-jobs" | "--jobs" => k += 2,
            // How this run treated what was there, not how the frames look: run again, the
            // settings make the same frames into an empty folder.
            "--resume" | "--overwrite" => k += 1,
            "--out" => {
                if let Some(v) = out.get(k + 1) {
                    let abs = plain_path(Path::new(v)).display().to_string();
                    spelled.push("--out".to_owned());
                    spelled.push(quote(&abs));
                }
                k += 2;
            }
            _ => {
                let a = if k == 1 {
                    plain_path(Path::new(a)).display().to_string()
                } else {
                    a.clone()
                };
                spelled.push(quote(&a));
                k += 1;
            }
        }
    }
    lapsewerk_studio::export::set_settings_line(spelled.join(" "));
    Ok(out)
}

fn parse(args: &[String], render: bool) -> Result<Args, String> {
    let mut folder = None;
    let mut csv = None;
    let mut edge = 768;
    let mut look = Some(0);
    let mut hot_pixels = true;
    let mut write_keyframes = false;
    let mut reference = None;
    let cores = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
    // A measuring job holds one decoded raw at a time; four keep 16 GB machines comfortable.
    let mut jobs = (cores / 2).clamp(1, 4);
    let mut out = None;
    let mut size = None;
    let mut position = (0.5, 0.5);
    let (mut move_from, mut move_to) = (None, None);
    let mut fps = (25, 1);
    let mut codec = Codec::default();
    let mut bitrate = None;
    let mut transfer = Transfer::default();
    let mut passes = 2;
    // A full-size render needs about 2.5 GB; two at a time fit 16 GB beside the encoder.
    let mut render_jobs = 2;
    let mut overwrite = false;
    let mut sequence = false;
    let mut fit_orientation = false;
    let mut subpixel = false;
    let mut space_check = true;
    let mut hold_blacks = true;
    let mut auto_brightness = false;
    let mut base_look = false;
    let mut brightness: Vec<(usize, f64)> = Vec::new();
    let mut replace_frames: Vec<usize> = Vec::new();
    let mut resume = false;
    let mut frame_format = lapsewerk_video::frames::FrameFormat::Tiff;
    let mut timecode_text: Option<String> = None;
    let mut frame_name: Option<String> = None;
    let mut verify = false;
    let mut denoise_radius = 0usize;
    let mut regional = None;
    let mut blend = 1usize;
    let mut stabilize = false;
    let mut dust = false;
    let mut keep_light = None;
    let mut tolerance = 1.0f32;
    let mut hdr: Option<lapsewerk_core::hdr::Signal> = None;
    let mut hdr_peak: Option<f64> = None;
    let mut transfer_given = false;
    let mut light_pollution: Option<f32> = None;
    let mut sky: Option<lapsewerk_core::deflicker::Rect> = None;
    let mut reduce_stars: Option<f32> = None;
    let mut star_trails: Option<usize> = None;
    let mut replace_outliers = false;
    let mut fill_gaps = false;
    let mut tween = 0usize;
    let mut deflicker = Some(lapsewerk_core::deflicker::DeflickerParams::default().smoothing_frames);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        let render_only = |name: &str| {
            if render {
                Ok(())
            } else {
                Err(format!("{name} belongs to `lapsewerk render`"))
            }
        };
        match a.as_str() {
            "--csv" => csv = Some(PathBuf::from(value("--csv")?)),
            "--edge" => edge = value("--edge")?.parse().map_err(|_| "--edge expects pixels")?,
            "--look" => {
                look = Some(
                    value("--look")?
                        .parse()
                        .map_err(|_| "--look expects a frame number")?,
                );
            }
            "--per-frame-look" => look = None,
            "--keep-hot-pixels" => hot_pixels = false,
            "--reference" => {
                let v: Vec<f32> = value("--reference")?
                    .split(',')
                    .map(|p| p.trim().parse::<f32>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| "--reference expects X,Y,W,H, such as 0,0,1,0.4")?;
                reference = match v.as_slice() {
                    [x, y, w, h] => lapsewerk_core::deflicker::Rect::new(*x, *y, *w, *h),
                    _ => None,
                };
                if reference.is_none() {
                    return Err(format!(
                        "--reference expects X,Y,W,H inside the frame (0 to 1, X+W and Y+H at most 1, \
                         W and H at least {}), such as 0,0,1,0.4",
                        lapsewerk_core::deflicker::Rect::MIN_SIDE
                    ));
                }
            }
            "--write-keyframes" => {
                if render {
                    return Err("--write-keyframes belongs to `lapsewerk analyze`".into());
                }
                write_keyframes = true;
            }
            "--jobs" => jobs = value("--jobs")?.parse().map_err(|_| "--jobs expects a number")?,
            "--out" => {
                render_only(a)?;
                out = Some(PathBuf::from(value("--out")?));
            }
            "--size" => {
                render_only(a)?;
                let v = value("--size")?;
                size = Some(
                    v.split_once(['x', 'X', '×'])
                        .and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
                        .ok_or("--size expects WIDTHxHEIGHT, such as 3840x2160")?,
                );
                // 16K a side, 8K × 8K at most: a frame and its work fit in memory.
                if let Some((w, h)) = size
                    && (w > 16_384 || h > 16_384 || w * h > 8_192 * 8_192 || w > 8 * h || h > 8 * w)
                {
                    return Err(format!(
                        "--size {w}x{h} is beyond what Lapsewerk renders (16384 a side, 67 megapixels, sides at most 8:1)"
                    ));
                }
            }
            "--position" => {
                render_only(a)?;
                let v = value("--position")?;
                position = v
                    .split_once(',')
                    .and_then(|(x, y)| Some((x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?)))
                    .filter(|(x, y)| (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y))
                    .ok_or("--position expects X,Y between 0 and 1, such as 0.5,1")?;
            }
            "--tween" => {
                render_only(a)?;
                tween = value("--tween")?
                    .parse()
                    .ok()
                    .filter(|n| (1..=3).contains(n))
                    .ok_or("--tween expects 1 to 3")?;
            }
            "--deflicker" => {
                render_only(a)?;
                let n: f64 = value("--deflicker")?
                    .parse()
                    .ok()
                    .filter(|n: &f64| (0.0..=60.0).contains(n))
                    .ok_or("--deflicker expects frames from 0 (off) to 60")?;
                deflicker = (n > 0.0).then_some(n.max(1.0));
            }
            "--move-from" | "--move-to" => {
                render_only(a)?;
                let v = value(a)?;
                let n: Vec<f64> = v.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                let view = match n.as_slice() {
                    [x, y, zoom]
                        if (0.0..=1.0).contains(x)
                            && (0.0..=1.0).contains(y)
                            && (1.0..=lapsewerk_core::framing::MAX_ZOOM).contains(zoom) =>
                    {
                        lapsewerk_core::framing::View {
                            x: *x,
                            y: *y,
                            zoom: *zoom,
                        }
                    }
                    _ => {
                        return Err(format!(
                            "{a} expects X,Y,ZOOM: position 0..1, zoom 1..2, such as 0.5,0.5,1.3"
                        ));
                    }
                };
                if a == "--move-from" {
                    move_from = Some(view);
                } else {
                    move_to = Some(view);
                }
            }
            "--fps" => {
                render_only(a)?;
                fps = lapsewerk_video::parse_fps(value("--fps")?)
                    .ok_or("--fps expects 25, 29.97 or 30000/1001")?;
                let rate = f64::from(fps.0) / f64::from(fps.1.max(1));
                if !(1.0..=240.0).contains(&rate) {
                    return Err(format!("--fps {rate} is outside 1 to 240 frames a second"));
                }
            }
            "--codec" => {
                render_only(a)?;
                codec = Codec::parse(value("--codec")?)
                    .ok_or("--codec: prores422hq, prores422, prores4444, hevc")?;
            }
            "--bitrate" => {
                render_only(a)?;
                bitrate = Some(
                    value("--bitrate")?
                        .parse()
                        .map_err(|_| "--bitrate expects bits per second")?,
                );
            }
            "--transfer" => {
                render_only(a)?;
                transfer_given = true;
                transfer = match value("--transfer")?.as_str() {
                    "srgb" => Transfer::Srgb,
                    "709" | "rec709" | "bt1886" => Transfer::Rec709,
                    _ => {
                        return Err("--transfer is srgb (web, Apple) or rec709 (broadcast, gamma 2.4)".into());
                    }
                };
            }
            "--hdr" => {
                render_only(a)?;
                hdr = Some(match value("--hdr")?.as_str() {
                    "pq" | "PQ" | "hdr10" => lapsewerk_core::hdr::Signal::Pq,
                    "hlg" | "HLG" => lapsewerk_core::hdr::Signal::Hlg,
                    _ => return Err("--hdr is pq or hlg".into()),
                });
            }
            "--hdr-peak" => {
                render_only(a)?;
                hdr_peak = Some(
                    value("--hdr-peak")?
                        .parse::<f64>()
                        .ok()
                        .filter(|v| v.is_finite() && *v > 0.0)
                        .ok_or("--hdr-peak expects stops above SDR white, such as 3")?,
                );
            }
            "--light-pollution" => {
                render_only(a)?;
                light_pollution = Some(
                    value("--light-pollution")?
                        .parse::<f32>()
                        .ok()
                        .filter(|v| (0.0..=1.0).contains(v))
                        .ok_or("--light-pollution expects 0 to 1, such as 1")?,
                );
            }
            "--sky" => {
                render_only(a)?;
                let v: Vec<f32> = value("--sky")?
                    .split(',')
                    .map(|p| p.trim().parse::<f32>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| "--sky expects X,Y,W,H, such as 0,0,1,0.6")?;
                sky = match v.as_slice() {
                    [x, y, w, h] => lapsewerk_core::deflicker::Rect::new(*x, *y, *w, *h),
                    _ => None,
                };
                if sky.is_none() {
                    return Err("--sky expects X,Y,W,H inside the frame, such as 0,0,1,0.6".into());
                }
            }
            "--reduce-stars" => {
                render_only(a)?;
                reduce_stars = Some(
                    value("--reduce-stars")?
                        .parse::<f32>()
                        .ok()
                        .filter(|v| (0.0..=1.0).contains(v))
                        .ok_or("--reduce-stars expects 0 to 1, such as 0.5")?,
                );
            }
            "--star-trails" => {
                render_only(a)?;
                star_trails = Some(
                    value("--star-trails")?
                        .parse::<usize>()
                        .ok()
                        .filter(|n| *n <= 100_000)
                        .ok_or("--star-trails expects a number of frames (0: endless)")?,
                );
            }
            "--replace-outliers" => {
                render_only(a)?;
                replace_outliers = true;
            }
            "--fill-gaps" => {
                render_only(a)?;
                fill_gaps = true;
            }
            "--passes" => {
                render_only(a)?;
                passes = match value("--passes")?.as_str() {
                    "1" => 1,
                    "2" => 2,
                    _ => return Err("--passes is 1 or 2".into()),
                };
            }
            "--render-jobs" => {
                render_only(a)?;
                render_jobs = value("--render-jobs")?
                    .parse()
                    .map_err(|_| "--render-jobs expects a number")?;
                if !(1..=8).contains(&render_jobs) {
                    return Err(format!("--render-jobs {render_jobs} is outside 1 to 8"));
                }
            }
            "--overwrite" => {
                render_only(a)?;
                overwrite = true;
            }
            "--brightness" => {
                render_only(a)?;
                let v = value("--brightness")?;
                brightness = v
                    .split(',')
                    .map(|p| {
                        let (f, s) = p.split_once(':')?;
                        let f: usize = f.trim().parse().ok().filter(|f| *f >= 1)?;
                        let s: f64 = s.trim().parse().ok().filter(|s: &f64| s.is_finite() && s.abs() <= 6.0)?;
                        Some((f - 1, s))
                    })
                    .collect::<Option<Vec<_>>>()
                    .ok_or("--brightness expects frame:stops points, frames from 1 and stops between -6 and +6, such as 1:0,120:+0.7,240:-0.3")?;
            }
            "--no-hold-blacks" => {
                render_only(a)?;
                hold_blacks = false;
            }
            "--auto-brightness" => {
                render_only(a)?;
                auto_brightness = true;
            }
            "--base-look" => {
                render_only(a)?;
                base_look = true;
            }
            "--no-space-check" => {
                render_only(a)?;
                space_check = false;
            }
            "--subpixel" => {
                render_only(a)?;
                subpixel = true;
            }
            "--fit-orientation" => {
                render_only(a)?;
                fit_orientation = true;
            }
            "--replace-frames" => {
                render_only(a)?;
                replace_frames = value("--replace-frames")?
                    .split(',')
                    .filter(|p| !p.trim().is_empty())
                    .map(|p| p.trim().parse::<usize>().ok().filter(|n| *n >= 1).map(|n| n - 1))
                    .collect::<Option<Vec<usize>>>()
                    .ok_or("--replace-frames expects frame numbers from 1, such as 13,20")?;
            }
            "--sequence" => {
                render_only(a)?;
                sequence = true;
            }
            "--name" => {
                render_only(a)?;
                frame_name = Some(
                    lapsewerk_video::frames::clean_name(value("--name")?)
                        .map_err(|e| format!("--name: {e}"))?,
                );
            }
            "--timecode" => {
                render_only(a)?;
                // Empty: none (a project's timecode taken back on the command line).
                timecode_text = Some(value("--timecode")?.to_owned()).filter(|t| !t.trim().is_empty());
            }
            "--frames" => {
                render_only(a)?;
                let v = value("--frames")?;
                frame_format = lapsewerk_video::frames::FrameFormat::parse(v)
                    .ok_or_else(|| format!("--frames {v}: tiff, dpx or exr"))?;
            }
            "--movie" => {
                render_only(a)?;
                sequence = false;
            }
            "--resume" => {
                render_only(a)?;
                resume = true;
            }
            "--verify" => {
                render_only(a)?;
                verify = true;
            }
            "--regional" => {
                render_only(a)?;
                let v = value("--regional")?;
                regional = Some(
                    v.split_once(['x', 'X', '×'])
                        .and_then(|(c, r)| {
                            lapsewerk_core::regional::Grid::new(
                                c.trim().parse().ok()?,
                                r.trim().parse().ok()?,
                            )
                        })
                        .ok_or("--regional expects COLSxROWS between 1 and 32, such as 6x4")?,
                );
            }
            "--keep-light" => {
                render_only(a)?;
                keep_light = Some(
                    value("--keep-light")?
                        .parse::<f64>()
                        .ok()
                        .filter(|k| (0.0..=1.0).contains(k))
                        .ok_or("--keep-light expects 0 to 1, such as 0.15")?,
                );
            }
            "--stabilize" => {
                render_only(a)?;
                stabilize = true;
            }
            "--dust" => {
                render_only(a)?;
                dust = true;
            }
            "--blend" => {
                render_only(a)?;
                blend = value("--blend")?
                    .parse()
                    .ok()
                    .filter(|b| (1..=8).contains(b))
                    .ok_or("--blend expects 1 to 8 frames")?;
            }
            "--denoise" => {
                render_only(a)?;
                denoise_radius = value("--denoise")?
                    .parse()
                    .ok()
                    .filter(|r| (0..=4).contains(r))
                    .ok_or("--denoise expects 0 to 4")?;
            }
            "--denoise-tolerance" => {
                render_only(a)?;
                tolerance = value("--denoise-tolerance")?
                    .parse()
                    .ok()
                    .filter(|t: &f32| (0.25..=4.0).contains(t))
                    .ok_or("--denoise-tolerance expects 0.25 to 4")?;
            }
            s if s.starts_with('-') => return Err(format!("unknown option `{s}`\n{USAGE}")),
            s if folder.is_none() => folder = Some(PathBuf::from(s)),
            s => return Err(format!("unexpected argument `{s}`")),
        }
    }
    let folder = folder.ok_or(USAGE)?;
    if !(64..=8192).contains(&edge) {
        return Err("--edge must be between 64 and 8192".into());
    }
    let render = if render {
        let out = out.ok_or(if sequence {
            "render needs --out FOLDER (the folder the frames go into)"
        } else {
            "render needs --out FILE.mov (or --sequence --out FOLDER for frames)"
        })?;
        // As a whole path, "." and ".." resolved: the frames are named after the folder (`--out .`
        // inside it named them frame_…, a second sequence beside the first).
        let out = plain_path(&out);
        if let Some(signal) = hdr {
            if transfer_given {
                return Err("--hdr sets the transfer itself; leave out --transfer".into());
            }
            let most = signal.max_headroom_ev().min(5.0);
            let ev = hdr_peak.unwrap_or(match signal {
                lapsewerk_core::hdr::Signal::Pq => 3.0,
                lapsewerk_core::hdr::Signal::Hlg => most,
            });
            if ev > most + 1e-9 {
                return Err(format!("--hdr-peak: at most {most:.1} stops for this signal"));
            }
            let peak = 2f64.powf(ev) as f32;
            transfer = match signal {
                lapsewerk_core::hdr::Signal::Pq => Transfer::Pq { peak },
                lapsewerk_core::hdr::Signal::Hlg => Transfer::Hlg { peak },
            };
        } else if hdr_peak.is_some() {
            return Err("--hdr-peak belongs to --hdr".into());
        } else if !transfer_given && codec != Codec::Hevc {
            // A ProRes master is for editing and broadcast: Rec. 709 (BT.1886), as the app does.
            transfer = Transfer::Rec709;
        }
        if sky.is_some() && light_pollution.is_none() && star_trails.is_none() {
            return Err("--sky belongs to --light-pollution or --star-trails".into());
        }
        if regional.is_some() && reference.is_some() {
            return Err(
                "--regional and --reference do not combine: regional deflicker measures the whole frame"
                    .into(),
            );
        }
        if regional.is_some() && passes < 2 {
            return Err("--regional needs the check pass (--passes 2)".into());
        }
        if frame_name.is_some() && !sequence {
            return Err("--name names the frames of --sequence (a movie is named by --out)".into());
        }
        let timecode = match &timecode_text {
            Some(_) if sequence && frame_format == lapsewerk_video::frames::FrameFormat::Tiff => {
                return Err("--timecode goes with a movie, or with DPX or EXR frames (--frames dpx|exr): TIFF has no timecode".into());
            }
            Some(t) => Some(lapsewerk_video::parse_timecode_df(t, fps.0, fps.1).ok_or_else(|| {
                // From the project (the app's timecode), not typed here: said, with the way out.
                let from_project = std::env::args().any(|a| a == "--from-project")
                    && !std::env::args().any(|a| a == "--timecode" || a.starts_with("--timecode="));
                let origin = if from_project {
                    " (the timecode comes from the project: give --timecode HH:MM:SS:FF for this rate, or leave --fps as the project has it)"
                } else {
                    ""
                };
                if t.contains(';') {
                    format!(
                        "--timecode {t}: drop-frame (HH:MM:SS;FF) is for 29.97 and 59.94 fps, and frame numbers 0 and 1 (59.94: 0 to 3) do not exist at a minute's start but every tenth{origin}"
                    )
                } else {
                    format!("--timecode {t}: HH:MM:SS:FF within a day, frames below the rate{origin}")
                }
            })?),
            None => None,
        };
        if sequence {
            if out.extension().is_some_and(|e| e.eq_ignore_ascii_case("mov")) {
                return Err("with --sequence, --out is a folder for the frames (not a .mov)".into());
            }
        } else if frame_format != lapsewerk_video::frames::FrameFormat::Tiff {
            return Err("--frames goes with --sequence".into());
        } else if resume {
            return Err("--resume goes with --sequence (a movie cannot be continued)".into());
        } else if !out.extension().is_some_and(|e| e.eq_ignore_ascii_case("mov")) {
            return Err("--out must end in .mov (or use --sequence for a folder of TIFF frames)".into());
        }
        Some(RenderArgs {
            out,
            auto_size: size.is_none() || fit_orientation,
            position,
            deflicker,
            tween,
            framing: (move_from.is_some() || move_to.is_some()).then(|| {
                let still = lapsewerk_core::framing::View {
                    x: position.0,
                    y: position.1,
                    zoom: 1.0,
                };
                lapsewerk_core::framing::Move {
                    from: move_from.unwrap_or(still),
                    to: move_to.unwrap_or(still),
                }
            }),
            video: VideoSettings {
                width: size.map_or(3840, |s| s.0),
                height: size.map_or(2160, |s| s.1),
                fps_num: fps.0,
                fps_den: fps.1,
                codec,
                bitrate,
                transfer,
                timecode: timecode.map(|(c, _)| c),
                drop_frame: timecode.is_some_and(|(_, d)| d),
            },
            passes,
            verify,
            regional,
            keep_light,
            hold_blacks,
            auto_brightness,
            base_look,
            brightness,
            stabilize,
            dust,
            blend,
            denoise: (denoise_radius > 0).then_some(lapsewerk_core::temporal::DenoiseParams {
                radius: denoise_radius,
                tolerance,
            }),
            astro: lapsewerk_studio::stage::AstroParams {
                light_pollution: light_pollution.and_then(|amount| {
                    Some((
                        sky.or_else(|| lapsewerk_core::deflicker::Rect::new(0.0, 0.0, 1.0, 0.6))?,
                        amount,
                    ))
                }),
                reduce_stars,
                trails: star_trails,
                trails_area: sky,
            },
            // Filled in from the analysis (`run`).
            repair: lapsewerk_studio::stage::RepairParams {
                replace: replace_frames,
                fill: Vec::new(),
            },
            replace_outliers,
            fill_gaps,
            jobs: render_jobs.clamp(1, 8),
            overwrite,
            sequence,
            frame_format,
            resume,
            subpixel,
        })
    } else {
        None
    };
    Ok(Args {
        space_check,
        folder,
        csv,
        edge,
        jobs: jobs.clamp(1, 16),
        look,
        hot_pixels,
        reference,
        write_keyframes,
        frame_name,
        render,
    })
}

/// `lapsewerk import <folder>`: the project file from the frames' metadata (no rendering).
fn calibrate(args: &[String]) -> Result<(), String> {
    let mut folders = Vec::new();
    let mut out = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = Some(PathBuf::from(it.next().ok_or("--out needs a folder")?)),
            o if o.starts_with("--") => return Err(format!("unknown option `{o}`\n{USAGE}")),
            f => folders.push(PathBuf::from(f)),
        }
    }
    if folders.is_empty() {
        return Err(format!("calibrate expects folders of raw photos\n{USAGE}"));
    }
    let c = lapsewerk_studio::calibrate(&folders, out.as_deref(), &Console::default())?;
    if c.written.is_empty() {
        return Err(format!(
            "no profile written: at least {} photos of one camera with a usable camera JPEG are needed ({} file(s) could not be used)",
            lapsewerk_develop::calibrate::MIN_PHOTOS,
            c.skipped
        ));
    }
    Ok(())
}

/// Checks a finished frame sequence against its checksums, naming each frame that differs.
fn verify(args: &[String]) -> Result<(), String> {
    let [dir] = args else {
        return Err("verify expects one folder of frames".into());
    };
    // Its checksum list there, but no INFO.txt in full (a full disk or a stop at the very end):
    // not ended either, whatever the frames say.
    let info_whole = lapsewerk_video::frames::read_text(
        std::path::Path::new(dir).join(lapsewerk_video::frames::INFO_FILE),
    )
    .is_ok_and(|t| t.contains("made on"));
    let stopped = lapsewerk_video::frames::stopped_render(std::path::Path::new(dir)) || !info_whole;
    let (ok, bad) = lapsewerk_video::frames::verify_checksums(std::path::Path::new(dir))?;
    println!("verified:      {ok} frame(s) as written");
    let aside = lapsewerk_video::frames::set_aside_files(std::path::Path::new(dir));
    if !aside.is_empty() {
        println!(
            "set aside:     {} (files that were there under frames' names, e.g. edited ones; not part of the delivery)",
            aside.join(", ")
        );
    }
    if stopped {
        println!(
            "stopped:       a render that did not end: its frames checked against the checksums noted as they were written; render … --resume finishes it (and writes SHA256SUMS.txt)"
        );
    }
    let extra = |b: &&String| b.ends_with(lapsewerk_video::frames::EXTRA);
    // A hostile list of a million lines prints a thousand, and says how many more.
    const SHOWN: usize = 1000;
    for b in bad.iter().take(SHOWN) {
        if extra(&b) {
            println!("extra:         {b}");
        } else {
            println!("differs:       {b}");
        }
    }
    // After the list, as "… and N more" reads.
    if bad.len() > SHOWN {
        println!("differs:       … and {} more not shown", bad.len() - SHOWN);
    }
    let extras = bad.iter().filter(extra).count();
    // In one line, by kind and with ranges ("missing 19: 12-30"), not only added up.
    if !bad.is_empty() {
        println!("summary:       {}", verify_summary(&bad));
    }
    if bad.is_empty() && stopped {
        // As written so far, but no delivery yet: a script must not take it for one.
        Err("a render that did not end: its frames are as written so far, but it is no finished delivery yet (render … --resume finishes it)".into())
    } else if bad.is_empty() {
        Ok(())
    } else if extras == bad.len() {
        // The delivery itself is as written; only something beside it would go into an edit too.
        Err(format!(
            "every listed frame is as written, but {extras} picture(s) beside them are not in SHA256SUMS.txt: move them out before the folder goes into an edit (exit code 1 until then)"
        ))
    } else if stopped {
        Err(format!(
            "{} file(s) differ from the checksums noted as the frames were written (a render that did not end): render … --resume makes them again and then the rest",
            bad.len()
        ))
    } else {
        Err(format!(
            "{} file(s) differ from SHA256SUMS.txt: changed or missing frames are made again with render … --resume; pictures not in the list belong elsewhere (move them out)",
            bad.len()
        ))
    }
}

/// `verify`'s findings by kind, each with its frames as ranges: "cut short 1: 7; changed 1: 5;
/// missing 19: 12-30; not in the list 1".
fn verify_summary(bad: &[String]) -> String {
    let number = |b: &str| -> Option<usize> {
        let name = b.split(':').next()?;
        let stem = name.rsplit_once('.')?.0;
        let digits = stem.rsplit_once('_')?.1;
        (digits.len() == 6).then(|| digits.parse().ok()).flatten()
    };
    let ranges = |mut v: Vec<usize>| -> String {
        v.sort_unstable();
        v.dedup();
        let mut out: Vec<String> = Vec::new();
        let mut i = 0;
        while let Some(&a) = v.get(i) {
            let mut j = i;
            while let (Some(next), Some(now)) = (v.get(j + 1), v.get(j))
                && *next == now + 1
            {
                j += 1;
            }
            let b = v.get(j).copied().unwrap_or(a);
            out.push(if a == b { a.to_string() } else { format!("{a}-{b}") });
            i = j + 1;
            if out.len() >= 20 {
                out.push("…".to_owned());
                break;
            }
        }
        out.join(", ")
    };
    let mut parts = Vec::new();
    for (kind, test) in [
        (
            "cut short",
            Box::new(|b: &str| b.contains(": cut short")) as Box<dyn Fn(&str) -> bool>,
        ),
        ("changed", Box::new(|b: &str| b.ends_with(": changed"))),
        ("missing", Box::new(|b: &str| b.ends_with(": missing"))),
    ] {
        let hits: Vec<&String> = bad.iter().filter(|b| test(b)).collect();
        if !hits.is_empty() {
            let frames = ranges(hits.iter().filter_map(|b| number(b)).collect());
            parts.push(if frames.is_empty() {
                format!("{kind} {}", hits.len())
            } else {
                format!("{kind} {}: {frames}", hits.len())
            });
        }
    }
    let extra = bad
        .iter()
        .filter(|b| b.ends_with(lapsewerk_video::frames::EXTRA))
        .count();
    if extra > 0 {
        parts.push(format!("not in the list {extra}"));
    }
    let other = bad.len()
        - bad
            .iter()
            .filter(|b| {
                b.contains(": cut short")
                    || b.ends_with(": changed")
                    || b.ends_with(": missing")
                    || b.ends_with(lapsewerk_video::frames::EXTRA)
            })
            .count();
    if other > 0 {
        parts.push(format!("other {other}"));
    }
    parts.join("; ")
}

/// `p` from the root, with "." and ".." taken out (by the names, not following links).
fn plain_path(p: &Path) -> PathBuf {
    use std::path::Component;
    let whole = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut out = PathBuf::new();
    for c in whole.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn import(args: &[String]) -> Result<(), String> {
    use lapsewerk_develop::project::Project;
    let folder = match args {
        [f] if !f.starts_with('-') => PathBuf::from(f),
        _ => return Err(format!("import expects one folder\n{USAGE}")),
    };
    let files = frame_files(&folder)?;
    eprintln!("lapsewerk: reading {} frames …", files.len());
    let seq = Sequence::open(&files).map_err(|e| e.to_string())?;
    if !seq.skipped().is_empty() {
        let names: Vec<String> = seq
            .skipped()
            .iter()
            .map(|p| {
                p.file_name()
                    .map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
            })
            .collect();
        println!(
            "skipped:       {} unreadable file(s), left out (damaged or cut short): {}",
            names.len(),
            names.join(", ")
        );
    }
    // An unreadable old project is replaced; only one written by a newer Lapsewerk is kept.
    let previous = match Project::load(&folder) {
        Ok(p) => p,
        Err(e @ lapsewerk_develop::project::ProjectError::Format { .. })
            if e.to_string().contains("this version reads") =>
        {
            return Err(format!("{e}: written by a newer Lapsewerk, left as it is"));
        }
        Err(e) => {
            eprintln!("lapsewerk: replacing the old project ({e})");
            None
        }
    };
    let seq_has_exif = seq.frames().iter().any(|f| f.exposure.is_some());
    let mut project = Project::of(seq.frames(), &seq.keyframe_frames()).map_err(|e| e.to_string())?;
    // The app's settings for the sequence stay.
    project.app = previous.as_ref().and_then(|p| p.app.clone());
    let n = project.frames.len();
    println!("frames:        {n}");
    match project.interval_ms {
        Some(ms) => println!("interval:      {:.1} s", ms as f64 / 1000.0),
        None => println!("interval:      unknown (no or identical capture times)"),
    }
    // The frame before each pause, counted from 1 (the index of the frame after it, from 0).
    println!("gaps after:    frame(s) {:?} (from 1)", project.gaps_before);
    println!("out of order:  {:?}", project.out_of_order);
    let exif = project.frames.iter().filter(|f| f.exposure.is_some()).count();
    println!("camera data:   {exif} of {n} frames");
    if seq_has_exif {
        println!("exposure steps: {} (EXIF)", project.exposure_steps.len());
    } else {
        println!(
            "exposure steps: no exposure data in the photos (EXIF): analyze looks for them in the pictures"
        );
    }
    let keys: Vec<usize> = (0..n)
        .filter(|&i| project.frames.get(i).is_some_and(|f| f.keyframe))
        .collect();
    println!(
        "keyframes:     frame(s) {:?} (from 1)",
        keys.iter().map(|k| k + 1).collect::<Vec<_>>()
    );
    if let Some(old) = previous {
        let c = old.changes(&files);
        if !c.is_empty() {
            println!(
                "since the last import: {} added, {} removed",
                c.added.len(),
                c.removed.len()
            );
        }
    }
    let path = project.save(&folder).map_err(|e| e.to_string())?;
    println!("project:       {}", path.display());
    Ok(())
}

/// Says when the folder's frames differ from its project (frames added or removed since the import).
fn check_project(folder: &Path, files: &[PathBuf]) {
    let list = |v: &[String]| {
        let shown: Vec<&str> = v.iter().take(5).map(String::as_str).collect();
        let more = v.len().saturating_sub(shown.len());
        if more > 0 {
            format!("{} and {more} more", shown.join(", "))
        } else {
            shown.join(", ")
        }
    };
    match lapsewerk_develop::project::Project::load(folder) {
        Ok(Some(p)) => {
            let c = p.changes(files);
            if !c.added.is_empty() {
                eprintln!(
                    "lapsewerk: not in the project (added since the import): {}",
                    list(&c.added)
                );
            }
            if !c.removed.is_empty() {
                eprintln!(
                    "lapsewerk: in the project but gone from the folder: {}",
                    list(&c.removed)
                );
            }
            if !c.is_empty() {
                eprintln!("lapsewerk: run `lapsewerk import` again to update the project");
            }
        }
        Ok(None) => {}
        Err(e) => eprintln!("lapsewerk: project file ignored: {e}"),
    }
}

/// The frames in `folder`, saying when JPEGs beside raws were left out.
pub fn frame_files(folder: &Path) -> Result<Vec<PathBuf>, String> {
    let f = lapsewerk_studio::frame_files(folder)?;
    if f.beside_raws > 0 {
        eprintln!(
            "lapsewerk: {} JPEG/TIFF/HEIC beside their raws left out (RAW+JPEG): frames come from the raws",
            f.beside_raws
        );
    }
    Ok(f.files)
}

fn run(args: &Args) -> Result<(), String> {
    let console = Console::default();
    let started = Instant::now();
    let files = frame_files(&args.folder)?;
    // Everything that can be checked without the frames is checked now, not after hours of measuring.
    // A destination outside the photo folder whose folder is not there (a delivery drive not
    // mounted): stopped, not made on the system disk in its place.
    if let Some(r) = args.render.as_ref() {
        let abs = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        let (out, folder) = (abs(&r.out), abs(&args.folder));
        let parent = out.parent().filter(|d| !d.as_os_str().is_empty());
        if let Some(d) = parent
            && !d.is_dir()
            && !d.starts_with(&folder)
        {
            return Err(format!(
                "{}: the folder for the export is not there (a drive not plugged in?); make it first, or choose another --out",
                d.display()
            ));
        }
    }
    // Room for the export: a night's render that fills the disk at frame 1500 is found out now.
    if let Some(r) = args.render.as_ref().filter(|_| args.space_check) {
        let frames = files.len() as u64 * (r.tween as u64 + 1);
        let there = if r.sequence && r.resume {
            let stem = lapsewerk_video::frames::frame_stem(&r.out);
            lapsewerk_video::frames::finished_frames_as(&r.out, &stem, r.frame_format) as u64
        } else {
            0
        };
        let format = r.sequence.then_some(r.frame_format);
        let needed = lapsewerk_video::space::estimate_bytes(&r.video, format, frames.saturating_sub(there));
        lapsewerk_video::space::check(&r.out, needed)
            .map_err(|e| format!("{}: {e} (--no-space-check renders anyway)", r.out.display()))?;
    }
    if let Some(r) = &args.render {
        lapsewerk_studio::check_target(&files, &r.out, r.sequence)?;
    }
    if let Some(r) = args.render.as_ref().filter(|r| r.sequence) {
        lapsewerk_video::frames::preflight_named(
            &r.out,
            r.frame_format,
            r.overwrite,
            r.resume,
            matches!(r.video.transfer, lapsewerk_video::Transfer::Pq { .. }),
            args.frame_name.as_deref(),
        )
        .and_then(|()| {
            if r.resume {
                lapsewerk_video::frames::resume_preflight(
                    &r.out,
                    r.frame_format,
                    &lapsewerk_studio::export::early_settings_words(r),
                )
            } else {
                Ok(())
            }
        })
        .map_err(|e| e.to_string())?;
    }
    // Held from now on (measuring takes minutes before the first frame is written): a second
    // render into it is refused at once.
    let _held = match args.render.as_ref().filter(|r| r.sequence) {
        Some(r) => {
            Some(lapsewerk_video::frames::reserve(&r.out).map_err(|e| format!("{}: {e}", r.out.display()))?)
        }
        None => None,
    };
    if let Some(r) = args.render.as_ref().filter(|r| !r.sequence) {
        lapsewerk_video::preflight(&r.out, r.video, r.overwrite).map_err(|e| e.to_string())?;
        for p in lapsewerk_video::stale_partials(&r.out) {
            eprintln!(
                "lapsewerk: {} is left from an earlier render that was killed; delete it unless another render to {} is running",
                p.display(),
                r.out.display()
            );
        }
    }
    if let Some(csv) = &args.csv {
        check_csv_target(csv, &files, args.render.as_ref().map(|r| r.out.as_path()))?;
    }
    eprintln!("lapsewerk: opening {} frames …", files.len());
    let open = OpenSettings {
        look: args.look,
        hot_pixels: args.hot_pixels,
    };
    let (mut seq, mut plan) = lapsewerk_studio::open(&files, &open, &console)?;
    // Auto transition, as the app does it: keyframes made and never edited follow the edited ones.
    let followed = lapsewerk_studio::cache::follow_edited_in(&mut seq, &args.folder)?;
    let kept = lapsewerk_studio::cache::untouched_kept_in(&seq, &args.folder);
    if !kept.is_empty() {
        println!(
            "auto transition: keyframe(s) {:?} (from 1) have read-only or linked XMP: kept as they are, they don't follow the edited ones (make the XMP writable to let them)",
            kept.iter().map(|f| f + 1).collect::<Vec<_>>()
        );
    }
    if !followed.is_empty() {
        println!(
            "auto transition: keyframe(s) {:?} (from 1, never edited): those between edited ones follow them, the others keep their look",
            followed.iter().map(|f| f + 1).collect::<Vec<_>>()
        );
        plan = seq
            .plan(lapsewerk_core::keyframes::Interpolation::Smooth)
            .map_err(|e| e.to_string())?;
    }
    // Against the project: the photos read (damaged ones left out are not "added").
    let readable: Vec<PathBuf> = files
        .iter()
        .filter(|f| !seq.skipped().contains(f))
        .cloned()
        .collect();
    check_project(&args.folder, &readable);
    // Which keyframes are automatic is kept in lapsewerk.json and marked in their sidecars: with
    // neither (keyframes made elsewhere) all count as edited, and none follows the others; said.
    let keyframes_there = seq.keyframe_frames().len();
    let marked = lapsewerk_develop::project::Project::load(&args.folder)
        .ok()
        .flatten()
        .and_then(|p| p.app)
        .is_some_and(|a| a.get("untouchedKeyframes").is_some())
        || seq
            .keyframe_frames()
            .into_iter()
            .any(|f| seq.automatic_mark(f).is_some());
    if keyframes_there > 1 && !marked {
        println!(
            "keyframes:     {keyframes_there} keyframes count as edited (no lapsewerk.json here says which were made automatically): none follows the others; the app's \"automatic again\" marks one so"
        );
    }
    let other_editor = seq.other_editor_beside();
    if !other_editor.is_empty() {
        println!(
            "keyframes:     frame(s) {:?} (from 1) were saved by another raw editor too: Lapsewerk develops them with its own settings in the file (lc:settings), so that editor's change does not show; edit them in LightCraft or the app, or remove lc:settings from the XMP to take that editor's values",
            other_editor.iter().map(|f| f + 1).collect::<Vec<_>>()
        );
    }
    for (f, names) in seq.unknown_settings() {
        println!(
            "keyframes:     frame {} holds settings this Lapsewerk does not know ({}; likely from a newer LightCraft): left out of the video until Lapsewerk is updated, and saving that keyframe in Lapsewerk removes them",
            f + 1,
            names.join(", ")
        );
    }
    let unreadable = seq.unreadable_sidecars();
    if !unreadable.is_empty() {
        println!(
            "keyframes:     the sidecar of frame(s) {:?} (from 1) holds no settings Lapsewerk can read (damaged, or another program's): developed with the defaults",
            unreadable.iter().map(|f| f + 1).collect::<Vec<_>>()
        );
    }
    let opened = started.elapsed();
    let n = seq.len();

    if let Some(t) = sequence::timing(seq.frames()) {
        println!("frames:        {n}");
        if t.interval_ms > 0 {
            println!("interval:      {:.1} s", t.interval_ms as f64 / 1000.0);
        } else {
            println!("interval:      unknown (identical capture times)");
        }
        if t.gaps_before.is_empty() {
            println!("gaps in time:  none");
        } else {
            println!(
                "gaps in time:  after frame(s) {:?} (from 1; damaged photos left out are gaps too, see quality)",
                t.gaps_before
            );
        }
        if t.out_of_order.is_empty() {
            println!("out of order:  none");
        } else {
            println!("out of order:  {:?}", t.out_of_order);
        }
    } else {
        println!("frames:        {n} (no capture times)");
    }
    println!(
        "keyframes:     frame(s) {:?} (from 1)",
        plan.keyframes().iter().map(|k| k + 1).collect::<Vec<_>>()
    );

    let measure = MeasureSettings {
        edge: args.edge,
        jobs: args.jobs,
        reference: args.reference,
        stabilize: args.render.as_ref().is_some_and(|r| r.stabilize),
    };
    // As measured before when the photos, keyframes and settings are unchanged (the app's analysis too).
    let measuring = std::time::Instant::now();
    let mut a = lapsewerk_studio::cache::analyse_cached(&mut seq, &plan, &measure, &console, &args.folder)?;
    // Without an analysis before, the automatic keyframes followed along time only: now the light is
    // known, they follow along it, as the next run (and the app) would; only the frames that
    // change are measured again.
    let followed_now = lapsewerk_studio::cache::follow_edited_in(&mut seq, &args.folder)?;
    if !followed_now.is_empty() {
        println!(
            "auto transition: keyframe(s) {:?} (from 1) follow along the light now measured",
            followed_now.iter().map(|f| f + 1).collect::<Vec<_>>()
        );
        plan = seq
            .plan(lapsewerk_core::keyframes::Interpolation::Smooth)
            .map_err(|e| e.to_string())?;
        a = lapsewerk_studio::cache::analyse_cached(&mut seq, &plan, &measure, &console, &args.folder)?;
    }
    // Kept from before, it took no measuring now.
    let kept = measuring.elapsed() < a.as_shot.time / 2;
    if let Err(e) = lapsewerk_studio::frame_size(&a.as_shot.sizes) {
        println!("shape:         {e}");
    }
    let q = lapsewerk_studio::quality_with(&a, seq.frames(), &seq.skipped_gaps());
    // The first frame after each gap: the jump across a gap is no flicker (the app says the same).
    let after_gaps: Vec<usize> = q.missing.iter().map(|(i, _)| *i).collect();
    report(&a, n, &after_gaps);
    println!(
        "quality:       {} odd frame(s) {:?}, highlights burnt out in {} frame(s) (up to {:.1} % of the picture), {} gap(s) {:?} (frames counted from 1; a gap as (after frame, frames missing))",
        q.outliers.len(),
        q.outliers.iter().map(|i| i + 1).collect::<Vec<_>>(),
        q.clipped.len(),
        q.clipped_most * 100.0,
        q.missing.len(),
        q.missing.iter().map(|(i, m)| (*i, *m)).collect::<Vec<_>>()
    );
    let (scene_range, video_range) = a.light_ranges();
    // The app keeps 15 % of a large change of light by itself; say so, so both agree.
    if a.day_to_night() && args.render.as_ref().is_none_or(|r| r.keep_light.is_none()) {
        // Which way: the scene's light at the end against the start.
        let first = a.scene.first().copied().unwrap_or(0.0);
        let last = a.scene.last().copied().unwrap_or(0.0);
        let way = if last > first {
            "night to day"
        } else {
            "day to night"
        };
        println!(
            "tip:           the light changes by {scene_range:.1} stops ({way}): --keep-light 0.15 keeps 15 % of it, as the app does by default"
        );
    }
    let suggested = a.suggested_keyframes();
    // As the app lists them: one right beside a saved keyframe adds nothing.
    let saved = seq.keyframe_frames();
    let near = (seq.len() / 50).max(2);
    let to_edit: Vec<usize> = suggested
        .iter()
        .filter(|s| !saved.iter().any(|k| k.abs_diff(**s) <= near && k != *s))
        .map(|k| k + 1)
        .collect();
    println!(
        "light:         the scene changes by {scene_range:.1} stops (as exposed, the camera's steps undone: {video_range:.1}; the video keeps the share --keep-light sets); keyframes to edit (frames from 1): {to_edit:?}"
    );
    if args.write_keyframes {
        let made = write_keyframes(&mut seq, &plan, &suggested)?;
        // The app lets these follow the ones you edit (auto transition) until you edit them.
        if let Err(e) = lapsewerk_studio::cache::note_untouched(&seq, &made, &args.folder) {
            eprintln!("lapsewerk: the new keyframes are not noted as automatic in lapsewerk.json: {e}");
        }
        // The app opens them without measuring again: the new keyframes hold what the plan gave there.
        if let Ok(after) = seq.plan(lapsewerk_core::keyframes::Interpolation::Smooth) {
            lapsewerk_studio::cache::carry_over(&seq, &plan, &after, &measure, &args.folder);
        }
        // And they follow the edited ones along the light just measured (not along time), as the
        // app and the next run would: written so now, and measured again where that changes a frame.
        let followed = lapsewerk_studio::cache::follow_edited_in(&mut seq, &args.folder)?;
        if !followed.is_empty() {
            println!(
                "auto transition: new keyframe(s) {:?} (from 1) follow the edited ones along the light",
                followed.iter().map(|f| f + 1).collect::<Vec<_>>()
            );
            let after = seq
                .plan(lapsewerk_core::keyframes::Interpolation::Smooth)
                .map_err(|e| e.to_string())?;
            lapsewerk_studio::cache::analyse_cached(&mut seq, &after, &measure, &console, &args.folder)?;
        }
    }
    if kept {
        println!(
            "time:          open {:.1} s, measure: none (as measured before, {:.1} s then)",
            opened.as_secs_f64(),
            a.as_shot.time.as_secs_f64()
        );
    } else {
        println!(
            "time:          open {:.1} s, measure {:.1} s ({:.0} ms/frame with {} measuring job(s), --jobs)",
            opened.as_secs_f64(),
            a.as_shot.time.as_secs_f64(),
            a.as_shot.time.as_secs_f64() * 1000.0 / n as f64,
            args.jobs
        );
    }

    if let Some(r) = &args.render {
        let mut r = r.clone();
        // The base look is for a sequence nobody edited, as in the app: an edited keyframe's look
        // is what it says.
        if r.base_look {
            let edited = lapsewerk_studio::cache::edited_keyframes_in(&seq, &args.folder);
            if !edited.is_empty() {
                println!(
                    "base look:     left out: keyframe(s) {:?} (from 1) are edited (here, in LightCraft or another raw editor), their look is used as it is",
                    edited.iter().map(|f| f + 1).collect::<Vec<_>>()
                );
                r.base_look = false;
            } else {
                // Said when it acts too: a delivery's log tells what made the look.
                let look: Vec<String> = lapsewerk_studio::BASE_LOOK
                    .iter()
                    .map(|(k, v)| format!("{k} {v:+}"))
                    .collect();
                println!("base look:     on every frame ({})", look.join(", "));
            }
        }
        if let Some(f) = r.repair.replace.iter().find(|f| **f >= seq.len()) {
            return Err(format!(
                "--replace-frames {}: the series has {} frames",
                f + 1,
                seq.len()
            ));
        }
        if r.replace_outliers {
            // Found ones, and those named (--replace-frames).
            let mut all: Vec<usize> = q.outliers.iter().chain(&r.repair.replace).copied().collect();
            all.sort_unstable();
            all.dedup();
            r.repair.replace = all;
        }
        if r.fill_gaps {
            r.repair.fill.clone_from(&q.missing);
        }
        let r = &r;
        let c = lapsewerk_studio::cache::correct_cached(
            &mut seq,
            &plan,
            &a,
            &measure,
            r,
            &console,
            &args.folder,
        )?;
        // The check pass's time beside the measuring's (on a long series it is most of it).
        if let Some((check, _)) = &c.second {
            println!(
                "time:          check pass {:.1} s ({:.0} ms/frame)",
                check.time.as_secs_f64(),
                check.time.as_secs_f64() * 1000.0 / seq.len().max(1) as f64
            );
        }
        // The analysis is kept even if the movie fails.
        if let Some(path) = &args.csv {
            write_csv(
                path,
                &seq,
                &a,
                c.second.as_ref(),
                c.verified.as_deref(),
                &c.offsets,
            )?;
            println!("csv:           {}", path.display());
        }
        lapsewerk_studio::render(&mut seq, &plan, &a, &c, &console)?;
        println!("total:         {:.1} s", started.elapsed().as_secs_f64());
    } else if let Some(path) = &args.csv {
        write_csv(path, &seq, &a, None, None, &a.offsets())?;
        println!("csv:           {}", path.display());
    }
    Ok(())
}

/// Refuses a CSV path that would overwrite a frame, the movie or a file that is not a CSV.
fn check_csv_target(csv: &Path, frames: &[PathBuf], movie: Option<&Path>) -> Result<(), String> {
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    let refuse = |why: &str| Err(format!("--csv {}: {why}", csv.display()));
    if frames.iter().any(|f| same(f, csv)) {
        return refuse("is one of the frames");
    }
    if movie.is_some_and(|m| same(m, csv)) {
        return refuse("is the movie's path");
    }
    if !csv.extension().is_some_and(|e| e.eq_ignore_ascii_case("csv")) {
        return refuse("must end in .csv");
    }
    if csv.is_dir() {
        return refuse("is a folder");
    }
    Ok(())
}

/// Writes `data` beside `path` and renames it into place: an interrupted write leaves the old file.
fn write_replacing(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = path
        .file_name()
        .map_or_else(|| "out".into(), |n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(data).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Writes sidecars for the `suggested` keyframes and reports each one.
/// Writes the suggested keyframes; the frames written.
fn write_keyframes(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    suggested: &[usize],
) -> Result<Vec<usize>, String> {
    use lapsewerk_develop::KeyframeWrite;
    let done = seq.write_keyframes(plan, suggested).map_err(|e| e.to_string())?;
    let name = |p: &Path| {
        p.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    };
    let mut failed = 0;
    for w in &done {
        match w {
            KeyframeWrite::Written { frame, sidecar } => {
                println!("  keyframe {:>5}: written {}", frame + 1, name(sidecar));
            }
            KeyframeWrite::Existing { frame, sidecar } => {
                println!(
                    "  keyframe {:>5}: has {} already, left as it is",
                    frame + 1,
                    name(sidecar)
                );
            }
            KeyframeWrite::Failed { frame, reason } => {
                failed += 1;
                println!("  keyframe {:>5}: not written: {reason}", frame + 1);
            }
        }
    }
    let written = done
        .iter()
        .filter(|w| matches!(w, KeyframeWrite::Written { .. }))
        .count();
    println!("new keyframes: {written} written; edit them in LightCraft, then render");
    if failed > 0 {
        return Err(format!("{failed} keyframe sidecar(s) could not be written"));
    }
    Ok(done
        .iter()
        .filter_map(|w| match w {
            KeyframeWrite::Written { frame, .. } => Some(*frame),
            _ => None,
        })
        .collect())
}

fn report(a: &Analysis, n: usize, after_gaps: &[usize]) {
    // Steps seen in the pictures (no EXIF: reported 0) are no sensor checks.
    let seen = |c: &&lapsewerk_core::exposure::StepCheck| c.reported == 0.0;
    let with_sensor = a
        .checks
        .iter()
        .filter(|c| c.measured.is_some() && !seen(c))
        .count();
    let confirmed = a
        .checks
        .iter()
        .filter(|c| c.measured.is_some() && !seen(c) && (c.used - c.reported).abs() < 1e-9)
        .count();
    if with_sensor == 0 {
        println!(
            "steps:         {} ({})",
            a.checks.len(),
            if a.checks.is_empty() {
                "the exposure stays the same: nothing to check"
            } else if a.checks.iter().all(|c| c.reported == 0.0) {
                "seen in the pictures, no EXIF"
            } else {
                "no raw sensor data to check them, e.g. JPEG: the EXIF steps are used"
            }
        );
    } else {
        println!(
            "steps:         {with_sensor} checked against the sensor data, {confirmed} as EXIF says; the others changed the picture by another amount, and that measured jump is used{}",
            match a.checks.len() - with_sensor {
                0 => String::new(),
                n => format!(" ({n} without sensor data use EXIF)"),
            }
        );
    }
    const LISTED: usize = 40;
    for c in a.checks.iter().take(LISTED) {
        if seen(&c) {
            println!(
                "  frame {:>5}: seen in the pictures, used {:+.2}",
                c.frame + 1,
                c.used
            );
        } else {
            println!(
                "  frame {:>5}: EXIF {:+.2}, sensor {}, used {:+.2}",
                c.frame + 1,
                c.reported,
                c.measured.map_or("–".into(), |m| format!("{m:+.2}")),
                c.used
            );
        }
    }
    println!(
        "measured:      {}/{n} frames ({} failed)",
        a.compensated.iter().flatten().count(),
        a.as_shot.failures.len()
    );
    if a.checks.len() > LISTED {
        println!("  … and {} more", a.checks.len() - LISTED);
    }
    for (i, e) in a.as_shot.failures.iter().take(5) {
        println!("  frame {}: {e}", i + 1);
    }
    // Odd frames (a flash) are replaced in the movie and would dominate the figure: left out,
    // as the app does.
    let odd = lapsewerk_core::quality::outliers(&a.compensated, lapsewerk_studio::OUTLIER_STOPS);
    let steady: Vec<Option<f64>> = a
        .compensated
        .iter()
        .enumerate()
        .map(|(i, v)| {
            if odd.contains(&i) || after_gaps.contains(&i) {
                None
            } else {
                *v
            }
        })
        .collect();
    let whole = jitter(&a.compensated);
    println!(
        "flicker:       {:.4} stops frame-to-frame (RMS of second differences{})",
        jitter(&steady),
        if odd.is_empty() {
            String::new()
        } else {
            format!("; without {} odd frame(s), {whole:.4} with them", odd.len())
        }
    );
    println!(
        "after deflicker (predicted from the measurements; the check pass corrects what the tone curve leaves, --verify measures it): {:.4} stops",
        jitter(&apply(&steady, &a.corrections))
    );
    let largest = a.corrections.iter().fold(0.0f64, |m, c| m.max(c.abs()));
    println!("largest correction: {largest:.3} stops");
}

fn write_csv(
    path: &Path,
    seq: &Sequence,
    a: &Analysis,
    second: Option<&(Pass, Vec<f64>)>,
    verified: Option<&[Option<f64>]>,
    offsets: &[f64],
) -> Result<(), String> {
    let opt = |v: Option<f64>| v.map(|v| format!("{v:.4}")).unwrap_or_default();
    let mut out = String::from(
        "frame,file,captured_ms,shutter_s,aperture,iso,exif_ramp_stops,raw_stops,ramp_stops,as_shot_stops,\
         compensated_stops,deflicker_stops,checked_stops,second_deflicker_stops,offset_stops,verified_stops,\
         scene_stops,video_trend_stops\n",
    );
    for (i, f) in seq.frames().iter().enumerate() {
        let e = f.exposure;
        let _ = writeln!(
            out,
            "{i},{},{},{},{},{},{:.4},{},{:.4},{},{},{:.4},{},{},{:.4},{},{:.4},{:.4}",
            csv_field(
                &f.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            f.captured_ms.map(|v| v.to_string()).unwrap_or_default(),
            e.map(|e| e.shutter_s.to_string()).unwrap_or_default(),
            e.map(|e| e.aperture.to_string()).unwrap_or_default(),
            e.map(|e| e.iso.to_string()).unwrap_or_default(),
            a.exif_ramp.get(i).copied().unwrap_or(0.0),
            opt(a.as_shot.raw.get(i).copied().flatten()),
            a.ramp.get(i).copied().unwrap_or(0.0),
            opt(a.as_shot.stops.get(i).copied().flatten()),
            opt(a.compensated.get(i).copied().flatten()),
            a.corrections.get(i).copied().unwrap_or(0.0),
            opt(second.and_then(|s| s.0.stops.get(i).copied().flatten())),
            opt(second.and_then(|s| s.1.get(i).copied())),
            offsets.get(i).copied().unwrap_or(0.0),
            opt(verified.and_then(|v| v.get(i).copied().flatten())),
            a.scene.get(i).copied().unwrap_or(f64::NAN),
            a.video_trend.get(i).copied().unwrap_or(f64::NAN),
        );
    }
    write_replacing(path, out.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

/// A CSV field, quoted when it holds a comma, quote or line break, and defused when a spreadsheet
/// would read it as a formula.
fn csv_field(s: &str) -> String {
    let defused = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{s}")
    } else {
        s.to_owned()
    };
    if defused.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", defused.replace('"', "\"\""))
    } else {
        defused
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_says_each_kind_with_its_frames() {
        let bad: Vec<String> = [
            "s_000005.dpx: changed".to_owned(),
            "s_000007.dpx: cut short (10 of about 20 bytes)".to_owned(),
        ]
        .into_iter()
        .chain((12..=30).map(|i| format!("s_{i:06}.dpx: missing")))
        .collect();
        assert_eq!(
            verify_summary(&bad),
            "cut short 1: 7; changed 1: 5; missing 19: 12-30"
        );
    }

    #[test]
    fn an_option_and_its_value_may_be_joined_by_an_equals_sign() {
        let args = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            split_equals(args(&[
                "render",
                "A",
                "--name=SHOT",
                "--size",
                "1x1",
                "--",
                "x=y"
            ])),
            args(&["render", "A", "--name", "SHOT", "--size", "1x1", "--", "x=y"])
        );
    }

    #[test]
    fn the_csv_never_replaces_a_frame_or_the_movie() {
        let frames = vec![PathBuf::from("/seq/a.arw")];
        assert!(check_csv_target(Path::new("/seq/a.arw"), &frames, None).is_err());
        assert!(
            check_csv_target(
                Path::new("/seq/film.mov"),
                &frames,
                Some(Path::new("/seq/film.mov"))
            )
            .is_err()
        );
        assert!(check_csv_target(Path::new("/seq/lapsewerk.json"), &frames, None).is_err());
        assert!(check_csv_target(Path::new("/seq/analyse.CSV"), &frames, None).is_ok());
    }

    #[test]
    fn csv_fields_are_quoted_and_defused() {
        assert_eq!(csv_field("a.arw"), "a.arw");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("=1+1"), "'=1+1");
        assert_eq!(csv_field("\t=1"), "'\t=1");
        assert_eq!(csv_field("\r=1"), "\"'\r=1\"");
    }
}
