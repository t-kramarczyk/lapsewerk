//! The video as a sequence of 16-bit TIFF files: on every system (no AVFoundation needed), for
//! editing software that takes image sequences (DaVinci Resolve, Premiere, After Effects), and
//! resumable: every frame is written on its own, atomically, so a render stopped at frame 2500 of
//! 3000 can go on from there.
//!
//! Files are `<dir>/<stem>_000001.tif` (counted from 1), the values as the movie would carry them
//! (the same transfer: sRGB, BT.1886 for Rec. 709, or the PQ / HLG signal). Also as DPX (10-bit,
//! the same values) or OpenEXR (half float, linear light: SDR white 1.0, HDR highlights above).

use std::path::{Path, PathBuf};

use lightcraft_codecs::encode::{EncodeImage, EncodeMeta, Samples, TiffCompression, encode_tiff};

use crate::{Error, VideoSettings};

/// The file format of a frame sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrameFormat {
    /// 16-bit TIFF (Deflate), with a colour profile.
    #[default]
    Tiff,
    /// 10-bit DPX, the transfer's values.
    Dpx,
    /// OpenEXR, half float, linear light (ZIP).
    Exr,
}

impl FrameFormat {
    /// The files' extension.
    #[must_use]
    pub fn ext(self) -> &'static str {
        match self {
            Self::Tiff => "tif",
            Self::Dpx => "dpx",
            Self::Exr => "exr",
        }
    }

    /// The format's name, as people say it: TIFF, DPX, EXR.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Tiff => "TIFF",
            Self::Dpx => "DPX",
            Self::Exr => "EXR",
        }
    }

    /// The format of a frame file's name, by its extension.
    #[must_use]
    pub fn of_name(file: &str) -> Option<Self> {
        Self::parse(file.rsplit_once('.')?.1)
    }

    /// The format of a name: `tiff`/`tif`, `dpx`, `exr`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "tif" | "tiff" => Some(Self::Tiff),
            "dpx" => Some(Self::Dpx),
            "exr" => Some(Self::Exr),
            _ => None,
        }
    }
}

/// Writes the frames of a video as numbered files (16-bit TIFF, DPX or OpenEXR).
pub struct FrameWriter {
    /// Frames of an earlier render removed to make this one (overwriting).
    removed: usize,
    /// The frame files of this name Lapsewerk is known to have written here (its checksum lists):
    /// only these are removed past the end or on a fresh start; a file someone put there named
    /// like a frame stays.
    ours: std::collections::BTreeMap<String, Vec<Noted>>,
    /// Files named like this render's frames that are not known to be Lapsewerk's: kept.
    kept_foreign: Vec<String>,
    /// Resuming: the first frame found damaged or cut short (made again from there).
    damaged: Option<usize>,
    /// Resuming: every frame found damaged from the first on.
    damaged_all: Vec<usize>,
    /// Resuming: the whole frames after the first damaged one, as written (kept when the render
    /// can keep them, rather than made again).
    keepable: Vec<usize>,
    dir: PathBuf,
    stem: String,
    format: FrameFormat,
    transfer: crate::Transfer,
    /// Frames a second, and the timecode of the first frame (DPX and OpenEXR carry them).
    fps: (u32, u32),
    timecode: Option<u32>,
    drop_frame: bool,
    width: usize,
    height: usize,
    table: Option<crate::Conversion>,
    /// PQ: the meter, and each frame's brightest pixel and average (cd/m²), those of an earlier
    /// run too (kept in [`LIGHT`]), so a resumed sequence reports the light of all its frames.
    meter: Option<(lapsewerk_core::hdr::LightMeter, FrameLight)>,
    /// The colour profile embedded (SDR: sRGB, or Rec. 709 with BT.1886's gamma 2.4).
    icc: Option<Vec<u8>>,
    /// The next frame's number (from 0).
    next: usize,
    written: u64,
}

/// The TIFF file of frame `index` (from 0) in `dir`.
#[must_use]
pub fn frame_path(dir: &Path, stem: &str, index: usize) -> PathBuf {
    frame_path_as(dir, stem, index, FrameFormat::Tiff)
}

/// The file of frame `index` (from 0) in `dir`, in `format`.
#[must_use]
pub fn frame_path_as(dir: &Path, stem: &str, index: usize, format: FrameFormat) -> PathBuf {
    dir.join(format!("{stem}_{:06}.{}", index + 1, format.ext()))
}

/// How many TIFF frames from the first on are already in `dir` (a stopped render's).
#[must_use]
pub fn finished_frames(dir: &Path, stem: &str) -> usize {
    finished_frames_as(dir, stem, FrameFormat::Tiff)
}

/// How many frames in `format` from the first on are already in `dir`.
#[must_use]
pub fn finished_frames_as(dir: &Path, stem: &str, format: FrameFormat) -> usize {
    (0..)
        .take_while(|&i| frame_path_as(dir, stem, i, format).is_file())
        .count()
}

/// A text file of the folder (a list, INFO.txt, a lock …) read only when it is a plain file of
/// sensible size: a pipe would never end, a link to /dev/zero would fill the memory.
pub fn read_text(path: impl AsRef<Path>) -> std::io::Result<String> {
    use std::io::Read;
    const MOST: u64 = 256 << 20;
    let path = path.as_ref();
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(std::io::Error::other("not a plain file"));
    }
    if meta.len() > MOST {
        return Err(std::io::Error::other("too large for a text file of the folder"));
    }
    let mut text = String::new();
    std::fs::File::open(path)?.take(MOST).read_to_string(&mut text)?;
    Ok(text)
}

/// The file in the folder that says what its frames were rendered with.
const MANIFEST: &str = ".lapsewerk-frames";

/// The render's settings in words (`name value`, one a line), beside the manifest: a refused resume
/// says which differ, there and here.
const SETTINGS_WORDS: &str = ".lapsewerk-settings";

/// The name the frames take when it is not the folder's (`--name`): `NAME_000001.tif` ….
const NAME_FILE: &str = ".lapsewerk-name";

/// The longest frame name (`--name`), in bytes.
const NAME_MAX: usize = 64;

/// A name for frames as given (`--name`): letters, digits, `-`, `_` and `.` (ASCII, as editing
/// systems and DPX headers take it), not starting with a dot, at most 64 characters.
///
/// # Errors
/// What is wrong with it, in words.
pub fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("the frames' name is empty".into());
    }
    if name.len() > NAME_MAX {
        return Err(format!("the frames' name is longer than {NAME_MAX} characters"));
    }
    if name.starts_with('.') {
        return Err("the frames' name starts with a dot (a hidden file)".into());
    }
    if name.starts_with('-') {
        return Err("the frames' name starts with '-' (read as an option by many programs)".into());
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(format!(
            "the frames' name holds {c:?}: use letters A-Z, digits, '-', '_' and '.' (as editing systems and DPX headers take them)"
        ));
    }
    Ok(name.to_owned())
}

/// [`preflight`] for a render whose frames are to be named `name` (`--name`; `None`: as they are
/// named now, the name given before or else the folder's). A name other than the one of the frames
/// there is refused, going on or replacing alike: going on would put a second sequence beside the
/// first, and its own frames are never removed unasked. The name is written (atomically, a link
/// in its place replaced, not followed) only once every check has passed.
///
/// # Errors
/// As [`preflight`]; [`Error::Target`] for a name [`clean_name`] refuses or one that differs from
/// the frames' there.
pub fn preflight_named(
    dir: &Path,
    format: FrameFormat,
    overwrite: bool,
    resume: bool,
    pq: bool,
    name: Option<&str>,
) -> Result<(), Error> {
    let target = |reason: String| Error::Target {
        path: dir.to_path_buf(),
        reason,
    };
    let Some(name) = name.map(clean_name).transpose().map_err(target)? else {
        return preflight(dir, format, overwrite, resume, pq);
    };
    let now = folder_stem(dir);
    let theirs = [FrameFormat::Tiff, FrameFormat::Dpx, FrameFormat::Exr]
        .into_iter()
        .any(|f| finished_frames_as(dir, &now, f) > 0 || !frames_from(dir, &now, 0, f).is_empty());
    if name != now && theirs && dir.join(MANIFEST).is_file() {
        return Err(target(format!(
            "its frames are named {now}_…, not {name}_…: give --name {now} to go on with them or replace them; for a delivery named {name}, choose another folder (or move the {now}_… frames out first)"
        )));
    }
    preflight_with(dir, &name, format, overwrite, resume, pq)?;
    if given_name(dir).as_deref() != Some(name.as_str()) {
        write_name(dir, &name).map_err(|e| {
            target(format!(
                "{NAME_FILE} (the frames' name) could not be written: {e}"
            ))
        })?;
    }
    Ok(())
}

/// Writes `name` as the frames' name of `dir` ([`NAME_FILE`]): into a new temporary file, then put
/// in place, so a link there is replaced and whatever it pointed at is left alone.
fn write_name(dir: &Path, name: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let tmp = dir.join(format!("{NAME_FILE}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(format!("{name}\n").as_bytes())?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, dir.join(NAME_FILE)));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The name given to the frames of `dir` (`--name`), when there is one and it is sound.
#[must_use]
pub fn given_name(dir: &Path) -> Option<String> {
    // A name is a line: anything larger than a few kilobytes, or no plain file, is none (asked
    // for at every frame the app draws).
    let path = dir.join(NAME_FILE);
    std::fs::metadata(&path)
        .ok()
        .filter(|m| m.is_file() && m.len() <= 4096)?;
    read_text(path)
        .ok()
        .and_then(|t| clean_name(t.lines().next().unwrap_or_default()).ok())
}

/// The name the frames in `dir` take: the one given with `--name`, else the folder's.
#[must_use]
pub fn frame_stem(dir: &Path) -> String {
    folder_stem(dir)
}

/// In a manifest: the exposure offsets follow (stops, comma-separated), compared with a
/// tolerance, not to the digit.
pub const OFFSETS_MARK: &str = "offsets ";

/// The most two renders' offsets of a frame may differ and still be one render (stops).
const OFFSET_TOLERANCE: f64 = 0.002;

/// Whether manifests `had` and `wanted` name the same render: the same text up to the offsets,
/// and offsets within [`OFFSET_TOLERANCE`] of each other.
fn same_render(had: &str, wanted: &str) -> bool {
    let (had, wanted) = (had.trim(), wanted.trim());
    match (had.split_once(OFFSETS_MARK), wanted.split_once(OFFSETS_MARK)) {
        (Some((ha, ho)), Some((wa, wo))) => {
            let parse = |s: &str| {
                s.split(',')
                    .map(|v| v.trim().parse::<f64>().ok())
                    .collect::<Vec<_>>()
            };
            let (ho, wo) = (parse(ho), parse(wo));
            ha == wa
                && ho.len() == wo.len()
                && ho.iter().zip(&wo).all(|(a, b)| match (a, b) {
                    (Some(a), Some(b)) => (a - b).abs() <= OFFSET_TOLERANCE,
                    _ => false,
                })
        }
        _ => had == wanted,
    }
}

/// The manifest a render with `settings` in `format` keeps beside its frames.
fn wanted_manifest(settings: &VideoSettings, format: FrameFormat, manifest: &str) -> String {
    // What is compared: the first line (the lines after it are the settings in words).
    let manifest = manifest.lines().next().unwrap_or_default();
    let size = format!("{}x{} {:?}", settings.width, settings.height, settings.transfer);
    // TIFF as before (folders of earlier versions resume); another format is named.
    let size = match format {
        FrameFormat::Tiff => size,
        f => format!("{size} {}", f.ext()),
    };
    format!("{size} {manifest}")
}

/// Which part of two manifests differs, in words: the size or curve (the leading
/// "WxH transfer [format]"), the corrections (the offsets), or the rest (rate, timecode, look,
/// repairs, all in the fingerprint).
fn what_differs(had: &str, wanted: &str, dir: &Path) -> String {
    let head = |m: &str| -> String {
        m.split_whitespace()
            .take_while(|w| !(w.len() == 16 && w.bytes().all(|b| b.is_ascii_hexdigit())))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let print = |m: &str| {
        m.split_whitespace()
            .find(|w| w.len() == 16 && w.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(str::to_owned)
    };
    if had.trim().is_empty() {
        "its settings file is missing or unreadable".into()
    } else if head(had) != head(wanted) {
        // Both spelled out: "there 1920x1080 Srgb, here 1280x720 Srgb".
        format!(
            "another size or curve: there {}, here {}",
            head(had),
            head(wanted)
        )
    } else if print(had) != print(wanted) {
        // The rate and timecode there, when its INFO.txt tells them (a render that ended).
        let info = read_text(dir.join(INFO_FILE)).unwrap_or_default();
        let field = |key: &str| {
            info.lines()
                .find_map(|l| l.strip_prefix(key))
                .map(|v| v.trim().to_owned())
        };
        let there = match (field("rate "), field("timecode ")) {
            (Some(r), Some(t)) => format!("; there: {r}, timecode {t}"),
            (Some(r), None) => format!("; there: {r}"),
            _ => String::new(),
        };
        format!("another frame rate, timecode, look, keyframes or repairs{there}")
    } else {
        "other corrections: deflicker, day to night or brightness".into()
    }
}

/// What a folder holds of a render, said when a new one would go there without resume or
/// overwrite: the frames of its sequence (as its INFO.txt counts them), and past its end, by name.
fn holds_text(dir: &Path, stem: &str, format: FrameFormat) -> String {
    let all = frames_from(dir, stem, 0, format);
    let named = read_text(dir.join(INFO_FILE)).ok().and_then(|t| {
        let (head, _) = t.split_once(" frames, numbered")?;
        head.rsplit('(').next()?.trim().parse::<usize>().ok()
    });
    let beyond = named
        .map(|n| frames_from(dir, stem, n, format))
        .unwrap_or_default();
    let own = all.len().saturating_sub(beyond.len());
    let past = if beyond.is_empty() {
        String::new()
    } else {
        format!(", and {} past its end ({})", beyond.len(), beyond.join(", "))
    };
    format!("holds {own} frames already{past} (resume, or overwrite to replace them)")
}

/// Why frames in format `there` are not resumed as `here`.
fn other_format(there: FrameFormat, here: FrameFormat) -> String {
    format!(
        "its frames are {} files, here {} is asked for: resuming would mix them; overwrite instead (replaces them)",
        there.name(),
        here.name()
    )
}

/// The settings that differ between the render in `dir` (its [`SETTINGS_WORDS`]) and `words`,
/// each "name: there A, here B"; `None` when it has none written (an older version) or none differ.
fn words_differ(dir: &Path, words: &str) -> Option<String> {
    let had = read_text(dir.join(SETTINGS_WORDS)).ok()?;
    words_differ_text(&had, words)
}

/// [`words_differ`] on the written settings `had`.
fn words_differ_text(had: &str, words: &str) -> Option<String> {
    let map = |t: &str| -> std::collections::BTreeMap<String, String> {
        t.lines()
            .filter_map(|l| l.split_once(": "))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    };
    let (there, here) = (map(had), map(words));
    let mut keys: Vec<&String> = there.keys().chain(here.keys()).collect();
    keys.sort();
    keys.dedup();
    let none = String::from("none");
    let differ: Vec<String> = keys
        .into_iter()
        .filter_map(|k| {
            // A setting an earlier version did not write down: not known to differ.
            if !k.starts_with("keyframe ") && !there.contains_key(k) {
                return None;
            }
            let (a, b) = (there.get(k).unwrap_or(&none), here.get(k).unwrap_or(&none));
            // A keyframe's look is a fingerprint: said as what happened to it.
            if k.starts_with("keyframe ") && a != b {
                return Some(match (there.contains_key(k), here.contains_key(k)) {
                    (false, _) => format!("{k} added since"),
                    (_, false) => format!("{k} taken away since"),
                    _ => format!("{k} edited since (in the app, LightCraft or by hand)"),
                });
            }
            (a != b).then(|| format!("{k}: there {a}, here {b}"))
        })
        .collect();
    (!differ.is_empty()).then(|| differ.join("; "))
}

/// What a folder holds of a frame render, before it is (re)made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderThere {
    /// Frames there in the format asked for.
    pub frames: usize,
    /// The render ended (its INFO.txt written after it started), rather than stopped.
    pub complete: bool,
    /// Made with the settings given (`None`: no settings given to compare with).
    pub same: Option<bool>,
    /// Not made by Lapsewerk there (no manifest beside them): never written over.
    pub foreign: bool,
    /// Another render (a terminal, another computer) is writing into it now.
    pub busy: bool,
    /// That render: which process, on which computer, since when.
    pub holder: Option<LockHolder>,
    /// Frames of this name past the end the render's INFO.txt names (put there by hand, or left
    /// by a longer render): not part of it, gone with the next render.
    pub beyond: Vec<String>,
    /// Other picture files in the folder (another delivery's): kept, but not part of this one.
    pub strangers: Vec<String>,
    /// Made with other settings: which, "name: there A, here B" (as a refused resume says it).
    pub why_other: Option<String>,
    /// Frames cut short with whole frames after them (a copy that broke off there), by name.
    pub damaged: Vec<String>,
    /// The render there ended (its INFO.txt and checksum list written), whole or since damaged.
    pub ended: bool,
}

/// The frames of `stem` from `from` on (counted from 0) as long as they follow on: those a render
/// ending at `from` removes.
#[must_use]
pub fn frames_from(dir: &Path, stem: &str, from: usize, format: FrameFormat) -> Vec<String> {
    // Every one of them, gaps or not (31 and 40 past an end of 30).
    let prefix = format!("{stem}_");
    let ext = format!(".{}", format.ext());
    let mut found: Vec<(usize, String)> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let number = name.strip_prefix(&prefix)?.strip_suffix(&ext)?;
                    if number.len() != 6 || !number.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    let index = number.parse::<usize>().ok()?.checked_sub(1)?;
                    (index >= from && e.path().is_file()).then_some((index, name))
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found.into_iter().map(|(_, n)| n).collect()
}

/// The name Lapsewerk's frames in `dir` carry when it is not the folder's (the folder renamed
/// after they were made): a render there is refused until it is named so again or they are moved.
#[must_use]
pub fn renamed_frames(dir: &Path) -> Option<String> {
    // Cheap first (asked as the page is drawn): no render there, or its first frame by this name.
    if !dir.join(MANIFEST).is_file() {
        return None;
    }
    let stem = folder_stem(dir);
    let first_here = [FrameFormat::Tiff, FrameFormat::Dpx, FrameFormat::Exr]
        .into_iter()
        .any(|f| frame_path_as(dir, &stem, 0, f).exists());
    if first_here {
        return None;
    }
    renamed_from(dir, &stem)
}

/// Picture files in `dir` not named `stem_…` (another delivery's), sorted.
#[must_use]
pub fn other_pictures(dir: &Path, stem: &str) -> Vec<String> {
    let prefix = format!("{}_", stem.to_lowercase());
    let mut names: Vec<String> = std::fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| {
                    let lower = name.to_lowercase();
                    !lower.starts_with(&prefix)
                        && [".tif", ".tiff", ".dpx", ".exr"]
                            .iter()
                            .any(|x| lower.ends_with(x))
                })
                .collect()
        },
    );
    names.sort();
    names
}

/// What `dir` holds of a render in `format`: how many frames, whether it ended, and (with
/// `manifest`, as [`FrameWriter::create_as`] takes it) whether it was made with these settings,
/// so going on would not mix two renders. `None` when there are no such frames.
#[must_use]
pub fn render_there(
    dir: &Path,
    settings: &VideoSettings,
    format: FrameFormat,
    manifest: Option<&str>,
) -> Option<RenderThere> {
    let stem = folder_stem(dir);
    let frames = finished_frames_as(dir, &stem, format);
    let holder = lock_holder_at(dir);
    // None there (frames after a lost first one count: they are this render's).
    if frames == 0 && (holder.is_some() || frames_from(dir, &stem, 0, format).is_empty()) {
        // Another render started and measures (minutes, before its first frame): said already.
        return holder.map(|h| RenderThere {
            frames: 0,
            complete: false,
            same: None,
            foreign: false,
            busy: true,
            holder: Some(h),
            beyond: Vec::new(),
            strangers: Vec::new(),
            why_other: None,
            damaged: Vec::new(),
            ended: false,
        });
    }
    // Another render writes here now: only the frames it wrote (since it started) are its own; those
    // of an earlier run it replaces do not count (it said "27 frames so far" while it measured).
    if let Some(h) = holder.as_ref().filter(|h| h.since.is_some()) {
        let since = h.since.unwrap_or(0);
        let written = (0..frames)
            .filter(|&i| {
                std::fs::metadata(frame_path_as(dir, &stem, i, format))
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .is_some_and(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX) >= since)
            })
            .count();
        return Some(RenderThere {
            frames: written,
            complete: false,
            same: None,
            foreign: false,
            busy: true,
            holder,
            beyond: Vec::new(),
            strangers: Vec::new(),
            why_other: None,
            damaged: Vec::new(),
            ended: false,
        });
    }
    let foreign = foreign_frames(dir, &stem) > 0;
    // Frames up to the first one cut short are what is done (going on makes the rest again).
    let cut = first_cut_short(dir, &stem, format, frames, settings.width, settings.height);
    let changed = |name: &str| std::fs::metadata(dir.join(name)).and_then(|m| m.modified()).ok();
    // Ended (INFO.txt written after it started), with every frame it names still there.
    let named = read_text(dir.join(INFO_FILE)).ok().and_then(|t| {
        let (head, _) = t.split_once(" frames, numbered")?;
        head.rsplit('(').next()?.trim().parse::<usize>().ok()
    });
    // Frames past the end INFO.txt names: not this render's (a copy put there, a longer
    // render's); it ended all the same.
    let beyond: Vec<String> = named
        .map(|n| frames_from(dir, &stem, n, format))
        .unwrap_or_default();
    let frames = named.map_or(frames, |n| n.min(frames));
    // The render ended: INFO.txt written after it started, with its checksum list (written before
    // INFO.txt): not cut off in between.
    let ended = !foreign
        && holder.is_none()
        && match (changed(INFO_FILE), changed(MANIFEST)) {
            (Some(info), Some(started)) => info >= started,
            _ => false,
        }
        && read_text(dir.join(CHECKSUMS_FILE)).is_ok();
    let complete = ended
        && named.is_none_or(|n| n == frames)
        // A frame cut short (a copy that broke off) is no finished frame.
        && cut.is_none();
    let had = read_text(dir.join(MANIFEST)).unwrap_or_default();
    let same = manifest.map(|m| same_render(&had, &wanted_manifest(settings, format, m)));
    let why_other = manifest.filter(|_| same == Some(false)).map(|m| {
        let words = m.split_once('\n').map_or("", |(_, w)| w);
        words_differ(dir, words)
            .unwrap_or_else(|| what_differs(&had, &wanted_manifest(settings, format, m), dir))
    });
    // Made with other settings (another size): the frames are not judged by this size.
    let other = same == Some(false);
    // A frame cut short with whole ones after it: damaged (a copy that broke there), not a render
    // stopped there; named.
    let name_of = |i: usize| {
        frame_path_as(dir, &stem, i, format)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    };
    let mut damaged: Vec<String> = cut
        .filter(|c| !other && c + 1 < frames)
        .and_then(name_of)
        .into_iter()
        .collect();
    // Frames gone from a sequence that ended (deleted in a copy) with frames after them: missing,
    // by name, not a render stopped there.
    // The end its INFO.txt names, or (a render that did not end) as far as its noted checksums go.
    let noted_end = named.or_else(|| noted_sums(dir).keys().next_back().map(|i| i + 1));
    if let Some(end) =
        noted_end.filter(|n| !other && *n > frames && !frames_from(dir, &stem, frames, format).is_empty())
    {
        damaged.extend(
            (frames..end)
                .filter(|&i| !frame_path_as(dir, &stem, i, format).exists())
                .take(1000)
                .filter_map(name_of),
        );
    }
    Some(RenderThere {
        frames: if other {
            frames
        } else {
            cut.map_or(frames, |c| c.min(frames))
        },
        complete,
        same: if foreign { Some(false) } else { same },
        foreign,
        busy: holder.is_some(),
        holder,
        beyond,
        strangers: other_pictures(dir, &stem),
        why_other,
        damaged,
        ended,
    })
}

/// Each frame's brightest pixel and average (cd/m²), by frame number.
type FrameLight = std::collections::BTreeMap<usize, (f32, f64)>;

/// PQ sequences: each frame's light (`index max average`, one line a frame).
const LIGHT: &str = ".lapsewerk-light";

/// PQ sequences: the HDR10 light levels of the whole sequence, written when it is finished.
pub const HDR10_FILE: &str = "HDR10.txt";

/// Every finished sequence: what its files are, for whoever edits or grades them.
pub const INFO_FILE: &str = "INFO.txt";

/// `frame` (from the first frame of the day at the rate rounded to whole frames) as
/// `HH:MM:SS:FF`, non-drop.
pub fn timecode_text(frame: u32, fps: (u32, u32), drop: bool) -> String {
    let base = ((f64::from(fps.0) / f64::from(fps.1.max(1))).round() as u32).max(1);
    if drop && (base == 30 || base == 60) {
        let (h, m, s, f) = crate::drop_frame_time(frame, base);
        return format!("{h:02}:{m:02}:{s:02};{f:02}");
    }
    let c = frame % (24 * 3600 * base);
    let (f, s) = (c % base, c / base);
    format!("{:02}:{:02}:{:02}:{f:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// Whether `path` is a whole frame of `width × height`: decoded in full, so a file cut short or
/// damaged inside (a copy that broke off, a network drive that went away) does not count.
fn whole_frame(path: &Path, format: FrameFormat, width: usize, height: usize) -> bool {
    // A plain file of a frame's size at most (a pipe would never end, /dev/zero never stop).
    let most = (width.saturating_mul(height).saturating_mul(16) as u64).saturating_add(1 << 20);
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= most) {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    match format {
        FrameFormat::Tiff => lightcraft_codecs::decode(&bytes, lightcraft_codecs::DecodeOptions::default())
            .is_ok_and(|d| d.width as usize == width && d.height as usize == height),
        // Uncompressed: the size says it all.
        FrameFormat::Dpx => bytes.len() == 2048 + width * height * 4 && bytes.starts_with(b"SDPX"),
        FrameFormat::Exr => crate::still::exr_complete(&bytes, width, height),
    }
}

/// The ICC profile that says how a frame's values are meant: sRGB, or Rec. 709 primaries with
/// gamma 2.4 (BT.1886); for HDR, Rec. 2020 with the signal named in a `cicp` tag (ICC 4.4: PQ or
/// HLG, as video files name it), which HDR-aware programs read, and a curve for the others that
/// puts SDR white (203 cd/m²) at white, so they show the frame at its right brightness with the
/// highlights above white clipped.
fn icc_for(t: crate::Transfer) -> Option<Vec<u8>> {
    use lapsewerk_core::hdr::{SDR_WHITE_NITS, hlg_encode, pq_encode};
    use lightcraft_codecs::icc::{write_matrix_trc, write_named};
    use lightcraft_codecs::space::{NamedSpace, Trc};
    let hdr = |name: &'static str, encode: fn(f64) -> f64, transfer: u8| {
        let mut space = NamedSpace::Rec2020.rgb_space();
        space.name = name;
        // Each signal value's light, relative to SDR white, found by bisection of the encoding.
        let table: Vec<u16> = (0..ICC_TABLE)
            .map(|k| {
                let v = k as f64 / (ICC_TABLE - 1) as f64;
                let (mut lo, mut hi) = (0.0f64, 10_000.0f64);
                for _ in 0..60 {
                    let mid = f64::midpoint(lo, hi);
                    if encode(mid) < v { lo = mid } else { hi = mid }
                }
                ((lo / SDR_WHITE_NITS).clamp(0.0, 1.0) * 65535.0).round() as u16
            })
            .collect();
        with_cicp(&write_matrix_trc(&space, &Trc::Table(table)), 9, transfer)
    };
    match t {
        crate::Transfer::Srgb => Some(write_named(NamedSpace::Srgb)),
        crate::Transfer::Rec709 => {
            // sRGB's primaries and white are Rec. 709's; the curve is BT.1886's gamma 2.4.
            let mut space = NamedSpace::Srgb.rgb_space();
            space.name = "Rec. 709 (BT.1886)";
            Some(write_matrix_trc(&space, &Trc::Gamma(2.4)))
        }
        crate::Transfer::Pq { .. } => hdr("Rec. 2020 PQ (ST 2084)", pq_encode, 16),
        crate::Transfer::Hlg { .. } => hdr("Rec. 2020 HLG", hlg_encode, 18),
    }
}

/// Entries of the curve table of an HDR frame's profile.
const ICC_TABLE: usize = 1024;

/// `profile` with a `cicp` tag (ICC.1:2022, 4.4): colour primaries, transfer characteristics,
/// matrix 0 (RGB) and full range, as H.273 numbers them; its version raised to 4.4. `None` when
/// `profile` is not laid out as expected.
fn with_cicp(profile: &[u8], primaries: u8, transfer: u8) -> Option<Vec<u8>> {
    let be = |i: usize| -> Option<usize> {
        let b: [u8; 4] = profile.get(i..i + 4)?.try_into().ok()?;
        Some(u32::from_be_bytes(b) as usize)
    };
    let count = be(128)?;
    let table_end = 132 + 12 * count;
    if count > 100 || profile.len() < table_end {
        return None;
    }
    let mut out = Vec::with_capacity(profile.len() + 28);
    out.extend_from_slice(profile.get(..128)?);
    out.extend_from_slice(&u32::try_from(count + 1).ok()?.to_be_bytes());
    // The tags as they were, their data 12 bytes further on (the table grew by one entry).
    for t in 0..count {
        let at = 132 + 12 * t;
        out.extend_from_slice(profile.get(at..at + 4)?);
        out.extend_from_slice(&u32::try_from(be(at + 4)? + 12).ok()?.to_be_bytes());
        out.extend_from_slice(profile.get(at + 8..at + 12)?);
    }
    let data_at = profile.len() + 12;
    out.extend_from_slice(b"cicp");
    out.extend_from_slice(&u32::try_from(data_at).ok()?.to_be_bytes());
    out.extend_from_slice(&12u32.to_be_bytes());
    out.extend_from_slice(profile.get(table_end..)?);
    // The tag data: 'cicp', reserved, primaries, transfer, matrix (RGB), full range.
    out.extend_from_slice(b"cicp");
    out.extend_from_slice(&[0, 0, 0, 0, primaries, transfer, 0, 1]);
    let size = u32::try_from(out.len()).ok()?;
    out.get_mut(0..4)?.copy_from_slice(&size.to_be_bytes());
    // Version 4.4; the profile ID (MD5) no longer matches: cleared (not computed).
    out.get_mut(8..12)?.copy_from_slice(&[4, 0x40, 0, 0]);
    out.get_mut(84..100)?.fill(0);
    Some(out)
}

/// A 64-bit FNV-1a fingerprint of `text` (stable across builds, unlike the std hasher).
#[must_use]
pub fn fingerprint(text: &str) -> String {
    fingerprint_bytes(text.as_bytes())
}

/// [`fingerprint`] of raw bytes (a file's, as they are: no byte is lost to a text conversion).
#[must_use]
pub fn fingerprint_bytes(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Files in `dir` named like frames (`<stem>_000001.tif|dpx|exr`) that no render of Lapsewerk
/// made there: without its manifest, they are someone's own (photos named like frames, an earlier
/// delivery copied in) and are never written over or removed.
fn foreign_frames(dir: &Path, stem: &str) -> usize {
    if dir.join(MANIFEST).is_file() {
        return 0;
    }
    [FrameFormat::Tiff, FrameFormat::Dpx, FrameFormat::Exr]
        .into_iter()
        .map(|f| finished_frames_as(dir, stem, f))
        .sum()
}

/// The refusal for [`foreign_frames`].
fn foreign_error(dir: &Path, n: usize) -> Error {
    Error::Target {
        path: dir.to_path_buf(),
        reason: format!(
            "holds {n} files named like frames that Lapsewerk did not make there (no .lapsewerk-frames beside them): they are left as they are; choose an empty folder"
        ),
    }
}

/// The first of `count` frames that is cut short, judged by size alone (no frame decoded): a DPX
/// frame has one size; a TIFF or EXR frame under a third of the series' middle size is no whole
/// frame of it.
fn first_cut_short(
    dir: &Path,
    stem: &str,
    format: FrameFormat,
    count: usize,
    width: usize,
    height: usize,
) -> Option<usize> {
    let sizes: Vec<u64> = (0..count)
        .map(|i| std::fs::metadata(frame_path_as(dir, stem, i, format)).map_or(0, |m| m.len()))
        .collect();
    match format {
        FrameFormat::Dpx => {
            let whole = 2048 + (width * height * 4) as u64;
            sizes.iter().position(|s| *s != whole)
        }
        FrameFormat::Tiff | FrameFormat::Exr => {
            let mut sorted = sizes.clone();
            sorted.sort_unstable();
            let middle = sorted.get(sorted.len() / 2).copied().unwrap_or(0);
            (0..count).find(|&i| {
                sizes.get(i).is_none_or(|s| *s < middle / 3)
                    // TIFF: its own description says where its pictures end (a few hundred
                    // bytes read): one cut off anywhere past its header is found.
                    || (format == FrameFormat::Tiff
                        && !tiff_whole(&frame_path_as(dir, stem, i, format)))
                    || (format == FrameFormat::Exr
                        && !exr_whole(&frame_path_as(dir, stem, i, format), width, height))
            })
        }
    }
}

/// Whether an OpenEXR frame ends where its last block does: its header and offset table read
/// (some kilobytes) and the last block's 8-byte header, not the pixels.
fn exr_whole(path: &Path, width: usize, height: usize) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let mut head = Vec::new();
    if (&mut f).take(256 * 1024).read_to_end(&mut head).is_err() {
        return false;
    }
    let Some(start) = crate::still::exr_last_block(&head, width, height) else {
        return false;
    };
    let mut block = [0u8; 8];
    if f.seek(SeekFrom::Start(start as u64)).is_err() || f.read_exact(&mut block).is_err() {
        return false;
    }
    let size = u64::from(u32::from_le_bytes([block[4], block[5], block[6], block[7]]));
    start as u64 + 8 + size == len
}

/// Whether a TIFF file holds every strip (or tile) its first directory names: the strips' ends
/// lie within the file. Reads the header and the directory only.
fn tiff_whole(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let mut head = [0u8; 8];
    if f.read_exact(&mut head).is_err() {
        return false;
    }
    let little = match &head[..4] {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return false,
    };
    let u16_of = |b: &[u8]| -> u64 {
        let a = [b.first().copied().unwrap_or(0), b.get(1).copied().unwrap_or(0)];
        u64::from(if little {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        })
    };
    let u32_of = |b: &[u8]| -> u64 {
        let mut a = [0u8; 4];
        for (d, s) in a.iter_mut().zip(b) {
            *d = *s;
        }
        u64::from(if little {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    };
    let ifd = u32_of(&head[4..]);
    if ifd.saturating_add(2) > len || f.seek(SeekFrom::Start(ifd)).is_err() {
        return false;
    }
    let mut n = [0u8; 2];
    if f.read_exact(&mut n).is_err() {
        return false;
    }
    let entries = u16_of(&n).min(512);
    let mut table = vec![0u8; (entries * 12) as usize];
    if f.read_exact(&mut table).is_err() {
        return false;
    }
    // (count, type, value or offset) of a tag.
    let find = |tag: u64| {
        table
            .as_chunks::<12>()
            .0
            .iter()
            .find(|e| u16_of(e.as_slice()) == tag)
            .map(|e| {
                (
                    u32_of(e.get(4..8).unwrap_or_default()),
                    u16_of(e.get(2..4).unwrap_or_default()),
                    e.get(8..12).map(<[u8]>::to_vec).unwrap_or_default(),
                )
            })
    };
    // Strips, else tiles.
    let (Some(offs), Some(sizes)) = (find(273).or_else(|| find(324)), find(279).or_else(|| find(325))) else {
        return false;
    };
    let values = |(count, kind, inline): (u64, u64, Vec<u8>), f: &mut std::fs::File| -> Option<Vec<u64>> {
        let width = if kind == 3 { 2 } else { 4 };
        let count = count.min(1 << 20);
        let bytes = if count * width <= 4 {
            inline
        } else {
            let at = u32_of(&inline);
            if at.saturating_add(count * width) > len {
                return None;
            }
            f.seek(SeekFrom::Start(at)).ok()?;
            let mut b = vec![0u8; (count * width) as usize];
            f.read_exact(&mut b).ok()?;
            b
        };
        Some(
            bytes
                .chunks_exact(width as usize)
                .take(count as usize)
                .map(|c| if width == 2 { u16_of(c) } else { u32_of(c) })
                .collect(),
        )
    };
    let (Some(offs), Some(sizes)) = (values(offs, &mut f), values(sizes, &mut f)) else {
        return false;
    };
    !offs.is_empty() && offs.iter().zip(&sizes).all(|(o, s)| o.saturating_add(*s) <= len)
}

/// The file that says a render is writing into the folder now (its process number).
const LOCK: &str = ".lapsewerk-lock";

/// Each frame's SHA-256, worked out as it is written (`index hex`, one line a frame).
const SUMS: &str = ".lapsewerk-sums";

/// Each run that wrote into the folder: from which frame (counted from 0), when and what made it
/// (`from<TAB>when<TAB>made`, newlines in it as `\u{1f}`), so a delivery made in two runs names
/// both.
const RUNS: &str = ".lapsewerk-runs";

/// Appends `line` to Lapsewerk's own hidden file `path`, never through a link: a symbolic link
/// there (to someone's file) is replaced by a file of our own; a hard-linked one (a `cp -al`
/// backup) is copied first, so the backup keeps what it had.
fn append_own(path: &Path, line: &str) {
    use std::io::Write as _;
    if let Ok(m) = std::fs::symlink_metadata(path) {
        #[cfg(unix)]
        let shared = {
            use std::os::unix::fs::MetadataExt as _;
            m.nlink() > 1
        };
        #[cfg(not(unix))]
        let shared = false;
        if m.file_type().is_symlink() {
            let _ = std::fs::remove_file(path);
        } else if shared && m.is_file() {
            let kept = std::fs::read(path).unwrap_or_default();
            let _ = std::fs::remove_file(path);
            let _ = std::fs::write(path, kept);
        } else if !m.is_file() {
            return;
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

/// `path` free for a file of Lapsewerk's: someone's own file there is kept beside as
/// "`stem` (before Lapsewerk).txt", numbered, never over an earlier kept one.
fn keep_theirs(path: &Path, stem: &str) {
    let Some(dir) = path.parent() else { return };
    let keep = (1..1000)
        .map(|k| {
            dir.join(if k == 1 {
                format!("{stem} (before Lapsewerk).txt")
            } else {
                format!("{stem} (before Lapsewerk) {k}.txt")
            })
        })
        .find(|p| std::fs::symlink_metadata(p).is_err());
    if let Some(keep) = keep {
        let _ = std::fs::rename(path, keep);
    }
}

/// Notes that a run writes into `dir` from frame `from` on (counted from 0), made by `made`.
pub fn note_run(dir: &Path, from: usize, when: &str, made: &str) {
    // One line, whatever it says (a line break or tab in it would make lines of its own).
    let made = made.replace('\n', "\u{1f}").replace(['\t', '\r'], " ");
    let when = when.replace(['\t', '\n', '\r'], " ");
    append_own(&dir.join(RUNS), &format!("{from}\t{when}\t{made}"));
}

/// The runs whose frames are still in `dir`, oldest first: `(from, when, made)`. A run that
/// started at or before an earlier one's first frame wrote over it.
#[must_use]
pub fn runs(dir: &Path) -> Vec<(usize, String, String)> {
    let mut kept: Vec<(usize, String, String)> = Vec::new();
    for line in read_text(dir.join(RUNS)).unwrap_or_default().lines() {
        let mut p = line.splitn(3, '\t');
        if let (Some(Ok(from)), Some(when), Some(made)) =
            (p.next().map(str::parse::<usize>), p.next(), p.next())
        {
            kept.retain(|(f, _, _)| *f < from);
            kept.push((from, when.to_owned(), made.replace('\u{1f}', "\n")));
        }
    }
    kept
}

/// The checksums of a finished sequence, as `sha256sum -c` checks them.
pub const CHECKSUMS_FILE: &str = "SHA256SUMS.txt";

/// SHA-256 of `bytes`, in hex.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Checks every frame `dir`'s [`CHECKSUMS_FILE`] names against its bytes: how many match, and
/// those that do not (`name: changed` / `name: missing`).
///
/// # Errors
/// When the list is not there or names nothing.
pub fn verify_checksums(dir: &Path) -> Result<(usize, Vec<String>), String> {
    let path = dir.join(CHECKSUMS_FILE);
    // A render that did not end has no list yet: the checksums noted as its frames were written
    // stand in for it (a stopped delivery is checked as far as it goes).
    let list = match read_text(&path) {
        Ok(text) => text,
        // There, but no list to read (a pipe, a device, a folder, far too large).
        Err(e) if std::fs::symlink_metadata(&path).is_ok() => {
            return Err(format!(
                "{}: can't be read as a checksum list ({e})",
                path.display()
            ));
        }
        Err(_) => noted_list(dir).ok_or_else(|| no_list(dir))?,
    };
    let mut ok = 0;
    let mut bad = Vec::new();
    let mut listed = std::collections::HashSet::new();
    let mut seen_sums: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut changed: Vec<String> = Vec::new();
    // The frames' usual size (the middle of those listed): one far smaller is cut short, not
    // changed in place.
    let usual = {
        let mut sizes: Vec<u64> = list
            .lines()
            .filter_map(|l| l.split_once("  ").map(|(_, n)| n.trim()))
            .filter(|n| !n.contains('/') && !n.contains('\\'))
            .filter_map(|n| {
                std::fs::metadata(dir.join(n))
                    .ok()
                    .filter(std::fs::Metadata::is_file)
            })
            .map(|m| m.len())
            .collect();
        sizes.sort_unstable();
        sizes.get(sizes.len() / 2).copied().unwrap_or(0)
    };
    for line in list.lines() {
        let line = line.trim_end_matches('\r');
        // "SHA256 (name) = sum", as `sha256sum --tag` writes it.
        let tagged = line
            .strip_prefix("SHA256 (")
            .and_then(|r| r.rsplit_once(") = "))
            .map(|(name, sum)| (sum, name));
        let Some((sum, name)) = tagged.or_else(|| line.split_once(' ')) else {
            continue;
        };
        // "sum  name" or, as `sha256sum -b` writes it, "sum *name".
        let name = if tagged.is_some() {
            name
        } else {
            name.strip_prefix(' ')
                .or_else(|| name.strip_prefix('*'))
                .unwrap_or(name)
        };
        // Only a plain name beside the list (a path elsewhere is no frame of this folder).
        if name.contains('/') || name.contains('\\') || name.starts_with('.') {
            bad.push(format!("{name}: not a frame of this folder"));
            continue;
        }
        // Named twice: checked once; with another checksum the second time, the list contradicts
        // itself (as `sha256sum -c` would fail one of the two).
        if !listed.insert(name.to_owned()) {
            if seen_sums
                .get(name)
                .is_some_and(|first: &String| !first.eq_ignore_ascii_case(sum))
            {
                bad.push(format!("{name}: listed twice with different checksums"));
            }
            continue;
        }
        seen_sums.insert(name.to_owned(), sum.to_owned());
        let file = dir.join(name);
        // A plain file only (a pipe or a device would never end).
        if !std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file()) {
            bad.push(format!("{name}: missing"));
            continue;
        }
        match std::fs::read(&file) {
            Ok(bytes) if sha256_hex(&bytes).eq_ignore_ascii_case(sum) => ok += 1,
            Ok(bytes) => {
                // A DPX frame has one size; a TIFF or EXR one under a third of the usual, or
                // smaller than usual and not whole (its last strip or block past the end), is cut.
                let len = bytes.len() as u64;
                let cut = usual > 0
                    && match FrameFormat::of_name(name) {
                        Some(FrameFormat::Dpx) => len < usual,
                        Some(FrameFormat::Exr) => {
                            len * 3 < usual
                                || (len < usual
                                    && crate::still::exr_size(&bytes)
                                        .is_none_or(|(w, h)| !crate::still::exr_complete(&bytes, w, h)))
                        }
                        _ => {
                            len * 3 < usual
                                || (len < usual
                                    && lightcraft_codecs::decode(
                                        &bytes,
                                        lightcraft_codecs::DecodeOptions::default(),
                                    )
                                    .is_err())
                        }
                    };
                bad.push(if cut {
                    format!("{name}: cut short ({} of about {usual} bytes)", bytes.len())
                } else {
                    format!("{name}: changed")
                });
                changed.push(name.to_owned());
            }
            Err(_) => bad.push(format!("{name}: missing")),
        }
    }
    if ok == 0 && bad.is_empty() {
        return Err(format!("{}: names no frames", path.display()));
    }
    // A changed frame of the same size and time (bits changed on a disk) is noted without them, so
    // the next `--resume` reads it, finds it changed and makes it again.
    if !changed.is_empty() {
        let noted = noted_sums(dir);
        for (i, (sum, _)) in &noted {
            let named = frame_names_in(dir, *i);
            if named.iter().any(|n| changed.contains(n)) {
                append_own(&dir.join(SUMS), &format!("{i} {sum}"));
            }
        }
    }
    // Pictures in the folder the list does not name (put there later, another delivery's): they
    // would go into an edit with the rest.
    let mut extra: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| {
                    let lower = n.to_lowercase();
                    !n.starts_with('.')
                        && !listed.contains(n)
                        && [".tif", ".tiff", ".dpx", ".exr"]
                            .iter()
                            .any(|x| lower.ends_with(x))
                })
                .collect()
        })
        .unwrap_or_default();
    extra.sort();
    bad.extend(extra.into_iter().map(|n| format!("{n}: {EXTRA}")));
    Ok((ok, bad))
}

/// The frames of a render that did not end, as a checksum list made of the sums noted while they
/// were written; `None` when it has none (or ended: its own list is there).
fn noted_list(dir: &Path) -> Option<String> {
    let stem = folder_stem(dir);
    let format = [FrameFormat::Tiff, FrameFormat::Dpx, FrameFormat::Exr]
        .into_iter()
        .max_by_key(|f| frames_from(dir, &stem, 0, *f).len())?;
    let noted = noted_sums(dir);
    if noted.is_empty() || frames_from(dir, &stem, 0, format).is_empty() {
        return None;
    }
    let text: String = noted
        .iter()
        .filter_map(|(i, (sum, _))| {
            let name = frame_path_as(dir, &stem, *i, format)
                .file_name()?
                .to_string_lossy()
                .into_owned();
            Some(format!("{sum}  {name}\n"))
        })
        .collect();
    Some(text)
}

/// Whether `dir` holds a render that did not end (frames, no checksum list yet): [`verify_checksums`]
/// checks it against the sums noted while writing.
#[must_use]
pub fn stopped_render(dir: &Path) -> bool {
    !dir.join(CHECKSUMS_FILE).exists() && noted_list(dir).is_some()
}

/// What [`verify_checksums`] says of a picture in the folder its list does not name.
pub const EXTRA: &str = "extra picture, not in the list (not part of this delivery)";

/// Why `dir` can't be checked: it does not exist, is a file, holds a stopped render (no list
/// yet) or a project whose sequences are below it.
fn no_list(dir: &Path) -> String {
    let shown = dir.display();
    if dir.is_file() {
        let holder = dir.parent().map_or_else(String::new, |p| p.display().to_string());
        return format!("{shown} is a file: give the folder of frames that holds it ({holder})");
    }
    if !dir.is_dir() {
        return format!("{shown}: no such folder");
    }
    // A render stopped before its end: its frames are there, its list comes when it ends.
    let stem = folder_stem(dir);
    let started = [FrameFormat::Tiff, FrameFormat::Dpx, FrameFormat::Exr]
        .into_iter()
        .map(|f| frames_from(dir, &stem, 0, f).len())
        .max()
        .unwrap_or(0);
    if started > 0 && dir.join(SUMS).exists() {
        return format!(
            "{shown}: {started} frame(s) of a render that did not end, no {CHECKSUMS_FILE} yet: finish it with lapsewerk render … --out {shown} --resume (the list is written when it ends)"
        );
    }
    // A project folder: the finished sequences below it (the app exports into Lapsewerk-Export).
    let mut below: Vec<String> = Vec::new();
    for sub in [dir.to_path_buf(), dir.join("Lapsewerk-Export")] {
        let Ok(entries) = std::fs::read_dir(&sub) else {
            continue;
        };
        for e in entries.flatten().take(10_000) {
            if e.path().join(CHECKSUMS_FILE).is_file()
                && let Ok(rel) = e.path().strip_prefix(dir)
            {
                below.push(rel.display().to_string());
            }
        }
    }
    below.sort();
    if !below.is_empty() {
        return format!(
            "{shown} has no {CHECKSUMS_FILE}; finished frame sequences below it: {} (give one of them)",
            below.join(", ")
        );
    }
    format!("{shown} has no {CHECKSUMS_FILE}: give the folder of a finished frame sequence")
}

/// The file names frame `i` (from 0) has in `dir`, in any format (by the folder's name).
fn frame_names_in(dir: &Path, i: usize) -> Vec<String> {
    let stem = folder_stem(dir);
    [FrameFormat::Tiff, FrameFormat::Dpx, FrameFormat::Exr]
        .into_iter()
        .filter_map(|f| {
            frame_path_as(dir, &stem, i, f)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .collect()
}

/// Whether `text` is a checksum list Lapsewerk wrote for frames named `stem_NNNNNN`.
fn own_checksums(text: &str, stem: &str) -> bool {
    !text.is_empty()
        && text.lines().all(|l| {
            l.split_once("  ").is_some_and(|(h, name)| {
                h.len() == 64
                    && h.bytes().all(|b| b.is_ascii_hexdigit())
                    && name.strip_prefix(stem).is_some_and(|r| r.starts_with('_'))
            })
        })
}

/// This computer's name (in a folder shared over the network, a lock of another computer's
/// process can't be checked here and counts as held).
fn host() -> String {
    // The machine's own id where there is one (a host name changes with the network on a Mac).
    read_text("/etc/machine-id")
        .ok()
        .or_else(|| {
            std::process::Command::new("ioreg")
                .args(["-rd1", "-c", "IOPlatformExpertDevice"])
                .output()
                .ok()
                .and_then(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .find(|l| l.contains("IOPlatformUUID"))
                        .and_then(|l| l.split('"').nth(3).map(str::to_owned))
                })
        })
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| read_text("/etc/hostname").ok())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Another render holding a folder's lock: its process, the computer it runs on (`None`: this
/// one) and since when (seconds since 1970, when the lock says).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockHolder {
    pub pid: u32,
    pub computer: Option<String>,
    pub since: Option<i64>,
    /// When the lock was last renewed (a render renews it every few minutes): its last sign of
    /// life, Unix seconds.
    pub seen: Option<i64>,
}

impl LockHolder {
    /// "process 4242 on mac-studio, since 22:09" (in English; the app says it in its own words).
    #[must_use]
    pub fn describe(&self) -> String {
        let on = self.computer.as_deref().unwrap_or("this computer");
        let when = |s: Option<i64>| {
            s.and_then(|s| chrono::DateTime::from_timestamp(s, 0)).map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
        };
        let mut text = match when(self.since) {
            Some(t) => format!("process {} on {on}, since {t}", self.pid),
            None => format!("process {} on {on}", self.pid),
        };
        if let Some(seen) = when(self.seen) {
            text.push_str(&format!(", last active {seen}"));
        }
        text
    }
}

/// The process of another render holding `dir`'s lock: running here, or on another computer.
#[must_use]
pub fn lock_holder(dir: &Path) -> Option<u32> {
    lock_holder_at(dir).map(|h| h.pid)
}

/// [`lock_holder`] with the computer it runs on and since when. Another computer's lock untouched
/// for 30 minutes counts as left over (that computer crashed; its renders renew it every four or
/// five minutes, while measuring too).
#[must_use]
pub fn lock_holder_at(dir: &Path) -> Option<LockHolder> {
    let path = dir.join(LOCK);
    let text = read_text(&path).ok()?;
    let mut parts = text.split_whitespace();
    let pid = parts.next()?.parse::<u32>().ok()?;
    let id = parts.next().map_or_else(host, str::to_owned);
    // Said by the computer's name (written third); the id is what tells computers apart.
    let name = parts.next().map_or_else(|| id.clone(), str::to_owned);
    let since = parts.next().and_then(|t| t.parse::<i64>().ok());
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let seen = modified
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_secs()).ok());
    if id != host() {
        let old = modified
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > std::time::Duration::from_secs(30 * 60));
        return (!old).then_some(LockHolder {
            pid,
            computer: Some(name),
            since,
            seen,
        });
    }
    // PID 1 is the system's own first process, never a render: a lock naming it is left over.
    (pid > 1 && pid != std::process::id() && alive(pid)).then_some(LockHolder {
        pid,
        computer: None,
        since,
        seen,
    })
}

/// Why a folder another render holds is refused.
fn busy_reason(h: &LockHolder) -> String {
    format!(
        "another render ({}) is writing into it now: wait for it to end or stop it (left over after a restart or a crash? delete {LOCK} there)",
        h.describe()
    )
}

/// What a lock holds: the process, the computer's id, its name and when the render started.
fn lock_text() -> String {
    let name = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| read_text("/etc/hostname").ok())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
        .map(|h| h.trim().replace(char::is_whitespace, "-"))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this-computer".into());
    format!(
        "{} {} {name} {}",
        std::process::id(),
        host(),
        chrono::Utc::now().timestamp()
    )
}

/// Removes `dir`'s HDR10.txt when it is Lapsewerk's own (by its first line).
fn remove_own_hdr10(dir: &Path) {
    let p = dir.join(HDR10_FILE);
    if read_text(&p).is_ok_and(|t| t.starts_with("HDR10 (SMPTE ST 2084")) {
        let _ = std::fs::remove_file(p);
    }
}

/// The folders this process holds the lock of: freed by [`release_all_locks`] when it is
/// interrupted (Ctrl-C, `kill`), as no destructor runs then.
static HELD: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

fn held() -> std::sync::MutexGuard<'static, Vec<PathBuf>> {
    HELD.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A frame's checksum as noted when it was written, with its size and time then.
type Noted = (String, (Option<u64>, Option<u128>));

/// The checksums noted in `dir` ([`SUMS`]), by frame; the last one for a frame made again.
fn noted_sums(dir: &Path) -> std::collections::BTreeMap<usize, Noted> {
    let mut noted = std::collections::BTreeMap::new();
    for line in read_text(dir.join(SUMS)).unwrap_or_default().lines() {
        let mut p = line.split(' ');
        if let (Some(Ok(i)), Some(h)) = (p.next().map(str::parse::<usize>), p.next())
            && h.len() == 64
            && h.bytes().all(|b| b.is_ascii_hexdigit())
        {
            let stamp = (
                p.next().and_then(|v| v.parse::<u64>().ok()),
                p.next().and_then(|v| v.parse::<u128>().ok()),
            );
            noted.insert(i, (h.to_ascii_lowercase(), stamp));
        }
    }
    noted
}

/// Moves `name` in `dir` aside under a name no edit takes for a frame (`NAME.kept`,
/// `NAME.kept-2`, …), as it is someone's and a render would write over it; the new name.
/// The files in `dir` set aside by a render (`name.kept`, `name.kept-2` …), sorted.
#[must_use]
pub fn set_aside_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| {
                    n.ends_with(".kept")
                        || n.rsplit_once(".kept-")
                            .is_some_and(|(_, k)| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn set_aside(dir: &Path, name: &str) -> Result<String, String> {
    let aside = (1..1000)
        .map(|k| {
            if k == 1 {
                format!("{name}.kept")
            } else {
                format!("{name}.kept-{k}")
            }
        })
        .find(|n| std::fs::symlink_metadata(dir.join(n)).is_err())
        .ok_or_else(|| format!("{name}: no free name to keep it aside"))?;
    std::fs::rename(dir.join(name), dir.join(&aside)).map_err(|e| format!("{name}: {e}"))?;
    Ok(aside)
}

/// Whether `path` is a frame Lapsewerk wrote here: named in its lists and still holding what it
/// wrote (a retouched copy put there under a frame's name is someone's, kept).
fn is_ours(ours: &std::collections::BTreeMap<String, Vec<Noted>>, name: &str, path: &Path) -> bool {
    ours.get(name)
        .is_some_and(|sums| sums.iter().any(|noted| as_written(path, noted)))
}

/// Whether frame `path` still holds what was written (its noted checksum): read only when its size
/// or time differ from then (a copied folder reads them all, once; untouched frames, none).
fn as_written(path: &Path, noted: &Noted) -> bool {
    let (sum, (len, time)) = noted;
    let now = std::fs::metadata(path).ok().map(|m| {
        (
            m.len(),
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos()),
        )
    });
    if let (Some(l), Some(t), Some(now)) = (len, time, now)
        && (*l, *t) == now
    {
        return true;
    }
    // Read only as a plain file (a pipe or a device in its place is no frame as written).
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() < (4 << 30))
        && std::fs::read(path).is_ok_and(|b| sha256_hex(&b) == *sum)
}

/// The name of Lapsewerk's frames in `dir` when they are not called `stem`: a first frame
/// `NAME_000001.ext` that its own checksum list or INFO.txt names.
fn renamed_from(dir: &Path, stem: &str) -> Option<String> {
    let listed = read_text(dir.join(CHECKSUMS_FILE)).unwrap_or_default()
        + &read_text(dir.join(INFO_FILE)).unwrap_or_default();
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let (other, rest) = name.rsplit_once('_')?;
        (rest.starts_with("000001.") && other != stem && listed.contains(&name)).then(|| other.to_owned())
    })
}

/// The name the frames in `dir` take: the one given with `--name` ([`NAME_FILE`]), else the
/// folder's own (".", ".." and a trailing "/" resolved); "frame" only for a folder without a name
/// (the root).
fn folder_stem(dir: &Path) -> String {
    if let Some(name) = given_name(dir) {
        return name;
    }
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty() && s != "." && s != "..")
        .or_else(|| {
            dir.canonicalize()
                .ok()
                .and_then(|c| c.file_name().map(|n| n.to_string_lossy().into_owned()))
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "frame".into())
}

/// Frees every lock this process holds (an interrupted render: another computer need not wait),
/// and removes the half-written frame or file it left there (`.NAME.PID.tmp`).
pub fn release_all_locks() -> Vec<PathBuf> {
    let dirs: Vec<PathBuf> = held().clone();
    let released = dirs.clone();
    let own = format!(".{}.tmp", std::process::id());
    for d in dirs {
        if let Ok(entries) = std::fs::read_dir(&d) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') && name.ends_with(&own) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        release_lock(&d);
    }
    released
}

/// A folder held for a render from its start (while it measures, for minutes, before the first
/// frame): another render is refused at once, and step 6 says so. Renewed every few minutes by a
/// thread of its own; freed when dropped.
pub struct Reservation {
    dir: PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Takes `dir`'s lock for a render about to start (made when not there).
///
/// # Errors
/// When another render holds it, or it cannot be made.
pub fn reserve(dir: &Path) -> Result<Reservation, Error> {
    std::fs::create_dir_all(dir).map_err(|e| Error::Target {
        path: dir.to_path_buf(),
        reason: e.to_string(),
    })?;
    take_lock(dir)?;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (d, s) = (dir.to_path_buf(), stop.clone());
    std::thread::spawn(move || {
        let mut waited = 0u32;
        while !s.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(1));
            waited += 1;
            if waited >= 240 {
                waited = 0;
                let path = d.join(LOCK);
                let own = std::process::id().to_string();
                if let Ok(text) = read_text(&path)
                    && text.split_whitespace().next() == Some(own.as_str())
                {
                    let _ = write_whole(&path, &text);
                }
            }
        }
    });
    Ok(Reservation {
        dir: dir.to_path_buf(),
        stop,
    })
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        release_lock(&self.dir);
    }
}

/// Frees the lock it stands for when dropped (an early return).
struct LockGuard<'a>(&'a Path);

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        release_lock(self.0);
    }
}

/// Frees `dir`'s lock when this process holds it.
fn release_lock(dir: &Path) {
    held().retain(|d| d != dir);
    let path = dir.join(LOCK);
    let own = std::process::id().to_string();
    if read_text(&path).is_ok_and(|t| t.split_whitespace().next() == Some(own.as_str())) {
        let _ = std::fs::remove_file(path);
    }
}

/// Whether process `pid` is running (another render, from the app's queue or a terminal).
fn alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        // A process that ended but is not yet reaped (a zombie, state Z) is not running.
        read_text(format!("/proc/{pid}/stat")).is_ok_and(|s| {
            s.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .is_some_and(|state| state != "Z" && state != "X")
        })
    } else {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

/// Marks `dir` as being written by this process; refused while another running one does (two
/// renders into one folder mixed their frames). A mark left by a process that ended is taken over.
/// Locks of renders that ended without freeing them (a crash, a computer switched off) that this
/// process took over: said by the render, not taken silently.
static TAKEN_OVER: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Notes the lock at `path` before it is replaced: whose it was.
fn note_left_over(path: &Path) {
    let Ok(text) = read_text(path) else { return };
    let mut p = text.split_whitespace();
    let (Some(pid), id) = (p.next(), p.next()) else {
        return;
    };
    if pid == std::process::id().to_string() {
        return;
    }
    let name = p.next().or(id).unwrap_or("this computer");
    let mut taken = TAKEN_OVER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    taken.push(format!(
        "{} (process {pid} on {name}, ended or silent for over 30 minutes)",
        path.display()
    ));
}

/// The left-over locks taken over since last asked (see [`TAKEN_OVER`]).
#[must_use]
pub fn taken_over_locks() -> Vec<String> {
    std::mem::take(
        &mut *TAKEN_OVER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

fn take_lock(dir: &Path) -> Result<(), Error> {
    let path = dir.join(LOCK);
    let own = std::process::id();
    let target = |reason: String| Error::Target {
        path: dir.to_path_buf(),
        reason,
    };
    // Made only when not there (two renders started at once: one wins); a mark of a process that
    // ended, or our own, is replaced.
    // Written whole first, then linked into place (a link is made only when nothing is there):
    // a second render never sees the lock empty, half written.
    let tmp = dir.join(format!("{LOCK}.{own}.tmp"));
    std::fs::write(&tmp, lock_text()).map_err(|e| target(e.to_string()))?;
    let result = (|| {
        for _ in 0..2 {
            match std::fs::hard_link(&tmp, &path) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Some(h) = lock_holder_at(dir) {
                        return Err(target(busy_reason(&h)));
                    }
                    // Just made by another render, still empty: held (an empty one older than
                    // a minute is left over: it is written in the moment it is made).
                    let young = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age < std::time::Duration::from_secs(60));
                    if young && read_text(&path).is_ok_and(|t| t.trim().is_empty()) {
                        return Err(target(format!(
                            "another render is starting to write into it now: wait for it to end (left over? delete {LOCK} there)"
                        )));
                    }
                    note_left_over(&path);
                    let _ = std::fs::remove_file(&path);
                }
                // A drive without links (FAT, exFAT, SD cards, some network shares): made
                // only when not there, then written (a moment's gap a second render reads as
                // "starting").
                Err(_) => {
                    use std::io::Write;
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                    {
                        Ok(mut f) => {
                            return f
                                .write_all(lock_text().as_bytes())
                                .map_err(|e| target(e.to_string()));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(e) => return Err(target(e.to_string())),
                    }
                    if let Some(h) = lock_holder_at(dir) {
                        return Err(target(busy_reason(&h)));
                    }
                    // Made by another render a moment ago and not written yet (on these drives
                    // it is made first, then written): held, as with links.
                    let young = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_none_or(|age| age < std::time::Duration::from_secs(60));
                    if young && read_text(&path).is_ok_and(|t| t.trim().is_empty()) {
                        return Err(target(format!(
                            "another render is starting to write into it now: wait for it to end (left over? delete {LOCK} there)"
                        )));
                    }
                    note_left_over(&path);
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        Err(target(format!("{LOCK} there could not be taken")))
    })();
    let _ = std::fs::remove_file(&tmp);
    if result.is_ok() {
        let mut h = held();
        if !h.iter().any(|d| d == dir) {
            h.push(dir.to_path_buf());
        }
    }
    result
}

/// Writes `text` to `path` whole or not at all, and never through a link: a new file put in its
/// place (an INFO.txt that is a link to a photo, a hard-linked backup, stays as it was).
pub fn write_whole(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        std::process::id()
    ));
    // A full disk: nothing half-written stays, the file there as it was.
    std::fs::write(&tmp, text).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Whether `path` begins as a frame of `format` does (TIFF, DPX, OpenEXR magic numbers): a file
/// of the frames' name that is none (someone's note) is not taken for one.
fn is_frame_file(path: &Path) -> bool {
    use std::io::Read;
    // A plain file only: opening a pipe there would wait for ever.
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut head = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && (head == *b"II*\0"
            || head == *b"MM\0*"
            || head == *b"SDPX"
            || head == *b"XPDS"
            || head == [0x76, 0x2f, 0x31, 0x01])
}

/// What can be checked about a sequence's folder before any frame is measured or rendered (a
/// render of 2000 frames measures for minutes first): a file in its place, a folder that cannot be
/// made, frames there without `resume` or `overwrite`, a resume over frames of another format.
///
/// # Errors
/// As [`FrameWriter::create_as`] would, later.
pub fn preflight(
    dir: &Path,
    format: FrameFormat,
    overwrite: bool,
    resume: bool,
    pq: bool,
) -> Result<(), Error> {
    preflight_with(dir, &folder_stem(dir), format, overwrite, resume, pq)
}

/// [`preflight`] for frames named `stem`.
fn preflight_with(
    dir: &Path,
    stem: &str,
    format: FrameFormat,
    overwrite: bool,
    resume: bool,
    pq: bool,
) -> Result<(), Error> {
    let target = |reason: String| Error::Target {
        path: dir.to_path_buf(),
        reason,
    };
    if dir.is_file() {
        return Err(target("is a file, not a folder for frames".into()));
    }
    std::fs::create_dir_all(dir).map_err(|e| target(e.to_string()))?;
    let stem = stem.to_owned();
    // A folder or pipe named like a frame: the render could never write that frame (it ended
    // there after hours); said now.
    if let Ok(entries) = std::fs::read_dir(dir) {
        let prefix = format!("{stem}_");
        for e in entries.flatten().take(1_000_000) {
            let name = e.file_name().to_string_lossy().into_owned();
            let numbered = name
                .strip_prefix(&prefix)
                .and_then(|r| {
                    ["tif", "dpx", "exr"]
                        .iter()
                        .find_map(|x| r.strip_suffix(x).and_then(|r| r.strip_suffix('.')))
                })
                .is_some_and(|n| n.len() == 6 && n.bytes().all(|b| b.is_ascii_digit()));
            if numbered && !std::fs::metadata(e.path()).is_ok_and(|m| m.is_file()) {
                return Err(target(format!(
                    "{name} there is no frame file (a folder or a pipe): a frame of this name can't be written; move it out first"
                )));
            }
        }
    }
    let foreign = foreign_frames(dir, &stem);
    if foreign > 0 {
        return Err(foreign_error(dir, foreign));
    }
    // Lapsewerk's frames there under another name (the folder renamed after they were made): going
    // on would make them all again beside the first ones.
    if dir.join(MANIFEST).is_file()
        && finished_frames_as(dir, &stem, format) == 0
        && let Some(other) = renamed_from(dir, &stem)
    {
        return Err(target(format!(
            "holds Lapsewerk's frames named {other}_…: give --name {other} (or name the folder {other} again) to go on with them; to replace them, move the {other}_… frames out of it first; or choose another folder"
        )));
    }
    // A folder where a file of the delivery goes (INFO.txt …): it could not be written.
    if let Some(name) = [INFO_FILE, HDR10_FILE, CHECKSUMS_FILE]
        .into_iter()
        .find(|n| dir.join(n).is_dir())
    {
        return Err(target(format!(
            "holds a folder named {name}, where the delivery's {name} goes: rename or move it"
        )));
    }
    if let Some(h) = lock_holder_at(dir) {
        return Err(target(busy_reason(&h)));
    }
    let there = finished_frames_as(dir, &stem, format);
    let present = there > 0 || !frames_from(dir, &stem, 0, format).is_empty();
    if present && !resume && !overwrite {
        return Err(target(holds_text(dir, &stem, format)));
    }
    if resume
        && there == 0
        && let Some(f) = [FrameFormat::Tiff, FrameFormat::Dpx, FrameFormat::Exr]
            .into_iter()
            .find(|f| *f != format && finished_frames_as(dir, &stem, *f) > 0)
    {
        return Err(target(other_format(f, format)));
    }
    if !resume
        && !overwrite
        && let Some((f, n)) = [FrameFormat::Tiff, FrameFormat::Dpx, FrameFormat::Exr]
            .into_iter()
            .filter(|f| *f != format)
            .map(|f| (f, finished_frames_as(dir, &stem, f)))
            .find(|(_, n)| *n > 0)
    {
        return Err(target(format!(
            "holds {n} {} frames already (overwrite to replace them)",
            f.name()
        )));
    }
    // A PQ resume on frames made without PQ: another curve, said as such (not as light missing).
    if resume
        && pq
        && there > 0
        && let Some(curve) = read_text(dir.join(SETTINGS_WORDS))
            .ok()
            .and_then(|t| {
                t.lines()
                    .find_map(|l| l.strip_prefix("curve: ").map(str::to_owned))
            })
            .filter(|c| !c.starts_with("PQ"))
    {
        return Err(target(format!(
            "its frames were rendered with other settings (curve: there {curve}, here PQ (HDR)): resuming would mix two renders; overwrite instead"
        )));
    }
    // A PQ resume needs the light of the frames there (as the writer checks, but before any
    // measuring).
    if resume && pq && there > 2 {
        let measured = read_text(dir.join(LIGHT))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_whitespace().next()?.parse::<usize>().ok())
            .filter(|i| *i < there)
            .count();
        if there - measured.min(there) > 2 {
            return Err(target(format!(
                "{} of its {there} frames have no light measurement (a copy without the hidden {LIGHT} file?): resuming would give wrong HDR10 figures; overwrite instead",
                there - measured.min(there)
            )));
        }
    }
    // Can frames be written there at all (a read-only share, a full disk)?
    let probe = dir.join(format!(".{stem}.{}.probe", std::process::id()));
    let wrote = std::fs::write(&probe, b"lapsewerk");
    // Gone whether or not the write worked (a full disk leaves an empty file).
    let _ = std::fs::remove_file(&probe);
    wrote.map_err(|e| target(e.to_string()))?;
    Ok(())
}

/// A resume refused before any frame is measured when a setting known without measuring differs
/// from the render there (size, frame rate, timecode, curve …): `early` holds just those settings,
/// one `name: value` a line as the render writes them. Settings `early` leaves out are compared
/// later, when the frames are written ([`FrameWriter::create_as`]).
///
/// # Errors
/// [`Error::Target`] naming each setting that differs, as the writer would say it later.
pub fn resume_preflight(dir: &Path, format: FrameFormat, early: &str) -> Result<(), Error> {
    let stem = folder_stem(dir);
    if finished_frames_as(dir, &stem, format) == 0 && frames_from(dir, &stem, 0, format).is_empty() {
        return Ok(());
    }
    let Ok(had) = read_text(dir.join(SETTINGS_WORDS)) else {
        return Ok(());
    };
    let keys: std::collections::BTreeSet<&str> = early
        .lines()
        .filter_map(|l| l.split_once(": ").map(|(k, _)| k))
        .collect();
    let had: String = had
        .lines()
        .filter(|l| l.split_once(": ").is_some_and(|(k, _)| keys.contains(k)))
        .map(|l| format!("{l}\n"))
        .collect();
    match words_differ_text(&had, early) {
        Some(why) => Err(Error::Target {
            path: dir.to_path_buf(),
            reason: format!(
                "its frames were rendered with other settings ({why}): resuming would mix two renders; overwrite instead"
            ),
        }),
        None => Ok(()),
    }
}

/// Removes temporary frame files a killed render left (`.STEM.PID.tmp`, untouched for 10 minutes:
/// a frame being written by another render is seconds old).
fn remove_stale_temporaries(dir: &Path, stem: &str) {
    let prefix = format!(".{stem}.");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // Its frames' and lock's temporaries, and the delivery files' (".INFO.txt.PID.tmp" …).
        let delivery = [INFO_FILE, CHECKSUMS_FILE, HDR10_FILE, MANIFEST, SETTINGS_WORDS]
            .iter()
            .any(|f| name.starts_with(&format!(".{f}.")) || name.starts_with(&format!("{f}.")));
        let ours = (name.starts_with(&prefix) || name.starts_with(&format!("{LOCK}.")) || delivery)
            && (name.ends_with(".tmp") || name.ends_with(".probe"));
        // Left by a process of this computer that ended (killed mid-frame): gone at once (this
        // render holds the folder's lock, so no other writes here now).
        let dead = name
            .rsplit('.')
            .nth(1)
            .and_then(|p| p.parse::<u32>().ok())
            .is_some_and(|pid| pid != std::process::id() && !alive(pid));
        if ours && dead {
            let _ = std::fs::remove_file(e.path());
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > std::time::Duration::from_secs(600));
        if ours && old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

impl Drop for FrameWriter {
    fn drop(&mut self) {
        // Ended, finished or not: the folder is free again (only our own mark goes).
        release_lock(&self.dir);
    }
}

impl FrameWriter {
    /// A writer into folder `dir` (made if needed), files named after the folder. With `resume`,
    /// frames already there are kept and writing starts after them ([`Self::next`]); otherwise a
    /// folder holding frames is refused unless `overwrite`.
    ///
    /// # Errors
    /// When the size is empty or odd, or the folder cannot be made or holds frames.
    pub fn create(dir: &Path, settings: VideoSettings, overwrite: bool, resume: bool) -> Result<Self, Error> {
        Self::create_for(dir, settings, overwrite, resume, "")
    }

    /// [`Self::create`], with `manifest` naming what the frames are rendered with (a fingerprint of
    /// the settings): kept in the folder, and a resume with other settings is refused, so a
    /// sequence never mixes two renders.
    ///
    /// # Errors
    /// As [`Self::create`], and when resuming frames made with other settings.
    pub fn create_for(
        dir: &Path,
        settings: VideoSettings,
        overwrite: bool,
        resume: bool,
        manifest: &str,
    ) -> Result<Self, Error> {
        Self::create_as(dir, settings, FrameFormat::Tiff, overwrite, resume, manifest)
    }

    /// [`Self::create_for`] in `format`.
    ///
    /// # Errors
    /// As [`Self::create_for`].
    pub fn create_as(
        dir: &Path,
        settings: VideoSettings,
        format: FrameFormat,
        overwrite: bool,
        resume: bool,
        manifest: &str,
    ) -> Result<Self, Error> {
        if settings.width == 0 || settings.height == 0 {
            return Err(Error::Settings("the frame size is empty".into()));
        }
        let stem = folder_stem(dir);
        std::fs::create_dir_all(dir).map_err(|e| Error::Target {
            path: dir.to_path_buf(),
            reason: e.to_string(),
        })?;
        let target = |reason: String| Error::Target {
            path: dir.to_path_buf(),
            reason,
        };
        take_lock(dir)?;
        // Held now: what an ended render left half-written goes (before, only after ten minutes).
        remove_stale_temporaries(dir, &stem);
        // Refused from here on: the lock goes again (a refused resume left it behind).
        let guard = LockGuard(dir);
        let foreign = foreign_frames(dir, &stem);
        if foreign > 0 {
            return Err(foreign_error(dir, foreign));
        }
        let mut there = finished_frames_as(dir, &stem, format);
        // Frames there, the first one gone or not (a copy that lost frame 1).
        let present = there > 0 || !frames_from(dir, &stem, 0, format).is_empty();
        let mut damaged = None;
        let mut damaged_all = Vec::new();
        let mut keepable = Vec::new();
        if present && !resume && !overwrite {
            return Err(target(holds_text(dir, &stem, format)));
        }
        // Frames in another format there (a TIFF delivery, now DPX): replaced only when asked to.
        if !resume
            && !overwrite
            && let Some((f, n)) = [FrameFormat::Tiff, FrameFormat::Dpx, FrameFormat::Exr]
                .into_iter()
                .filter(|f| *f != format)
                .map(|f| (f, finished_frames_as(dir, &stem, f)))
                .find(|(_, n)| *n > 0)
        {
            return Err(target(format!(
                "holds {n} {} frames already (overwrite to replace them)",
                f.name()
            )));
        }
        // Resuming in another format would delete the frames there: refused.
        if resume
            && !present
            && let Some(f) = [FrameFormat::Tiff, FrameFormat::Dpx, FrameFormat::Exr]
                .into_iter()
                .find(|f| *f != format && finished_frames_as(dir, &stem, *f) > 0)
        {
            return Err(target(other_format(f, format)));
        }
        let manifest_path = dir.join(MANIFEST);
        let wanted = wanted_manifest(&settings, format, manifest);
        // The frames Lapsewerk wrote here, by its lists (before a fresh start takes them away).
        // (by number in any of the formats: a fresh start in another format replaces them).
        let mut ours: std::collections::BTreeMap<String, Vec<Noted>> = std::collections::BTreeMap::new();
        for (i, noted) in noted_sums(dir) {
            for f in [FrameFormat::Tiff, FrameFormat::Dpx, FrameFormat::Exr] {
                if let Some(n) = frame_path_as(dir, &stem, i, f).file_name() {
                    ours.entry(n.to_string_lossy().into_owned())
                        .or_default()
                        .push(noted.clone());
                }
            }
        }
        if let Ok(t) = read_text(dir.join(CHECKSUMS_FILE))
            && own_checksums(&t, &stem)
        {
            for (sum, n) in t.lines().filter_map(|l| l.split_once("  ")) {
                ours.entry(n.trim().to_owned())
                    .or_default()
                    .push((sum.trim().to_ascii_lowercase(), (None, None)));
            }
        }
        let mut kept_foreign = Vec::new();
        if resume && present {
            let had = read_text(&manifest_path).unwrap_or_default();
            if !same_render(&had, &wanted) {
                let words = manifest.split_once('\n').map_or("", |(_, w)| w);
                let why = words_differ(dir, words).unwrap_or_else(|| what_differs(&had, &wanted, dir));
                return Err(target(format!(
                    "its frames were rendered with other settings ({why}): resuming would mix two renders; overwrite instead"
                )));
            }
            // The last frames written before the stop may be damaged (a crash, a full disk):
            // checked, and made again when they are.
            let whole = |i: usize| {
                whole_frame(
                    &frame_path_as(dir, &stem, i, format),
                    format,
                    settings.width,
                    settings.height,
                )
            };
            // Frames after a gap (one deleted in a copy) belong to this render too, up to the end
            // its INFO.txt names: the missing ones are made again, the rest kept where whole.
            let named_end = read_text(dir.join(INFO_FILE)).ok().and_then(|t| {
                let (head, _) = t.split_once(" frames, numbered")?;
                head.rsplit('(').next()?.trim().parse::<usize>().ok()
            });
            // A render that did not end: as far as the checksums noted while writing go.
            let named_end = named_end.or_else(|| noted_sums(dir).keys().next_back().map(|i| i + 1));
            let after_gap = named_end.map_or(there, |end| {
                frames_from(dir, &stem, there, format)
                    .iter()
                    .filter_map(|n| {
                        let digits = n.strip_prefix(&format!("{stem}_"))?.get(..6)?;
                        digits.parse::<usize>().ok()
                    })
                    .filter(|&k| k <= end)
                    .max()
                    .unwrap_or(there)
                    .max(there)
            });
            let finished = there;
            let counted = after_gap;
            while there > 0 && there == counted && !whole(there - 1) {
                there -= 1;
            }
            if there > 1 && there + 2 >= counted && !whole(there - 2) {
                there -= 2;
            }
            // Further back, cheaply: a frame cut short in the middle (a copy that broke off) is
            // made again from there on.
            if let Some(i) = first_cut_short(dir, &stem, format, there, settings.width, settings.height) {
                there = i;
            }
            // And a frame that no longer holds what was written (bits changed on a disk or in a
            // copy, the size the same): made again from there, not listed with a new checksum.
            let noted = noted_sums(dir);
            if let Some(i) = (0..there).find(|&i| {
                noted
                    .get(&i)
                    .is_some_and(|n| !as_written(&frame_path_as(dir, &stem, i, format), n))
            }) {
                there = i;
            }
            if there < counted {
                damaged = Some(there);
                // Each frame from there that is damaged, and those that are whole and as written
                // (they can be kept rather than made again).
                for i in there..counted {
                    let path = frame_path_as(dir, &stem, i, format);
                    // Past a gap only with its checksum noted (known to be this render's frame).
                    let unproven = i > finished && !noted.contains_key(&i);
                    let bad = unproven || !whole(i) || noted.get(&i).is_some_and(|n| !as_written(&path, n));
                    if bad {
                        // Whole, but not what this render wrote (a retouched copy, someone's frame
                        // put there): set aside before it is made again, not written over.
                        if whole(i)
                            && let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned())
                        {
                            kept_foreign.push(set_aside(dir, &name).map_err(target)?);
                        }
                        damaged_all.push(i);
                    } else {
                        keepable.push(i);
                    }
                }
            }
        }
        let mut removed = 0;
        if !resume || (there == 0 && damaged.is_none()) {
            // A fresh sequence: an earlier one's INFO.txt and HDR10.txt (ours) go too, so a stopped
            // render never stands beside an old delivery's description.
            for name in [INFO_FILE, HDR10_FILE, CHECKSUMS_FILE] {
                let p = dir.join(name);
                // Ours by their first line, or as the list of our frames (someone's note naming
                // Lapsewerk stays).
                if read_text(&p).is_ok_and(|t| {
                    t.starts_with("Lapsewerk frame sequence")
                        || t.starts_with("HDR10 (SMPTE ST 2084")
                        || own_checksums(&t, &stem)
                }) {
                    let _ = std::fs::remove_file(p);
                }
            }
            let _ = std::fs::remove_file(dir.join(SUMS));
            let _ = std::fs::remove_file(dir.join(RUNS));
            // A fresh sequence: the frames of an earlier one go, so none is left past the new end.
            if let Ok(entries) = std::fs::read_dir(dir) {
                let prefix = format!("{stem}_");
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let numbered = name
                        .strip_prefix(&prefix)
                        .and_then(|r| {
                            ["tif", "dpx", "exr"]
                                .iter()
                                .find_map(|x| r.strip_suffix(x).and_then(|r| r.strip_suffix('.')))
                        })
                        .is_some_and(|n| n.len() == 6 && n.bytes().all(|b| b.is_ascii_digit()));
                    // Only frames Lapsewerk wrote (its lists): a file of the same name put there by
                    // someone (a retouched copy) stays, and is named.
                    // Any plain file of a frame's name: ours goes, someone's (a retouched copy, a
                    // note or a JPEG saved under that name) is set aside.
                    if numbered && std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.is_file()) {
                        if is_frame_file(&e.path()) && is_ours(&ours, &name, &e.path()) {
                            std::fs::remove_file(e.path()).map_err(|e| target(e.to_string()))?;
                            removed += 1;
                        } else {
                            // Set aside under a name no edit takes for a frame (the render would
                            // write over it in place).
                            kept_foreign.push(set_aside(dir, &name).map_err(target)?);
                        }
                    }
                }
            }
            there = 0;
        }
        write_whole(&manifest_path, &wanted).map_err(|e| target(e.to_string()))?;
        if let Some((_, words)) = manifest.split_once('\n') {
            let _ = write_whole(&dir.join(SETTINGS_WORDS), words);
        }
        // Made: the writer holds the lock now (and frees it when dropped).
        std::mem::forget(guard);
        kept_foreign.sort();
        Ok(Self {
            removed,
            ours,
            kept_foreign,
            damaged,
            damaged_all,
            keepable,
            dir: dir.to_path_buf(),
            stem,
            format,
            transfer: settings.transfer,
            fps: (settings.fps_num, settings.fps_den),
            timecode: settings.timecode,
            drop_frame: settings.drop_frame,
            width: settings.width,
            height: settings.height,
            table: settings.transfer.conversion(),
            meter: match settings.transfer {
                crate::Transfer::Pq { peak } => {
                    // A resume: the frames kept bring their light along; a fresh start, none.
                    let mut kept = std::collections::BTreeMap::new();
                    if there > 0 {
                        for line in read_text(dir.join(LIGHT)).unwrap_or_default().lines() {
                            let mut p = line.split_whitespace();
                            if let (Some(Ok(i)), Some(Ok(m)), Some(Ok(a))) = (
                                p.next().map(str::parse::<usize>),
                                p.next().map(str::parse::<f32>),
                                p.next().map(str::parse::<f64>),
                            ) && i < there
                            {
                                kept.insert(i, (m, a));
                            }
                        }
                    } else {
                        let _ = std::fs::remove_file(dir.join(LIGHT));
                        remove_own_hdr10(dir);
                    }
                    // HDR10's light levels need every frame's light. The last frame or two
                    // without it (stopped between writing a frame and noting its light) are
                    // made again; more missing (a copy without the hidden file, an older
                    // version) would give wrong figures: refused.
                    let measured = (0..there).take_while(|i| kept.contains_key(i)).count();
                    if there - measured > 2 {
                        return Err(target(format!(
                            "{} of its {there} frames have no light measurement (a copy without the hidden {LIGHT} file?): resuming would give wrong HDR10 figures; overwrite instead",
                            there - measured
                        )));
                    }
                    there = measured;
                    Some((lapsewerk_core::hdr::LightMeter::new(peak), kept))
                }
                _ => {
                    // No PQ: an earlier PQ render's light levels do not describe these frames.
                    let _ = std::fs::remove_file(dir.join(LIGHT));
                    remove_own_hdr10(dir);
                    // Someone's own HDR10.txt beside SDR frames would read as an HDR delivery:
                    // kept beside under another name.
                    let theirs = dir.join(HDR10_FILE);
                    if theirs.exists() {
                        keep_theirs(&theirs, "HDR10");
                    }
                    None
                }
            },
            icc: icc_for(settings.transfer),
            next: there,
            written: 0,
        })
    }

    /// PQ: the light the frames written by this run reach (MaxCLL / MaxFALL).
    #[must_use]
    pub fn light_level(&self) -> Option<lapsewerk_core::hdr::LightLevel> {
        let (meter, frames) = self.meter.as_ref()?;
        let mut all = meter.clone();
        for &(m, a) in frames.range(..self.next).map(|(_, v)| v) {
            all.add(m, a);
        }
        all.level()
    }

    /// Resuming: the first frame (from 0) that was damaged or cut short, and is made again.
    #[must_use]
    pub fn damaged(&self) -> Option<usize> {
        self.damaged
    }

    /// Resuming: every frame (from 0) found damaged.
    #[must_use]
    pub fn damaged_all(&self) -> &[usize] {
        &self.damaged_all
    }

    /// Resuming: whole frames after the first damaged one that may be kept.
    #[must_use]
    pub fn keepable(&self) -> &[usize] {
        &self.keepable
    }

    /// Keeps the frame there as the next one (whole and as written) instead of writing it.
    ///
    /// # Errors
    /// When it is not one of [`Self::keepable`].
    pub fn keep_existing(&mut self) -> Result<(), Error> {
        if !self.keepable.contains(&self.next) {
            return Err(Error::Frame(format!("frame {} is not kept", self.next + 1)));
        }
        self.next += 1;
        self.written += 1;
        Ok(())
    }

    /// Frames of an earlier render (of this name, any format) removed to make this one.
    #[must_use]
    pub fn removed(&self) -> usize {
        self.removed
    }

    /// The number (from 0) of the frame written next.
    #[must_use]
    pub fn next(&self) -> usize {
        self.next
    }

    /// Frames written by this writer.
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.written
    }

    /// Writes from frame `index` (from 0) on: a resume that renders a few frames again (the
    /// neighbours a repair needs) writes them as they were.
    pub fn restart_at(&mut self, index: usize) {
        self.next = index.min(self.next);
    }

    /// Writes the next frame: the `width × height` window at (`left`, `top`) of a 16-bit RGB image
    /// `src_width` pixels wide.
    ///
    /// # Errors
    /// When the window does not fit, or the file cannot be written.
    pub fn push(&mut self, rgb: &[u16], src_width: usize, left: usize, top: usize) -> Result<(), Error> {
        let (w, h) = (self.width, self.height);
        let src_height = (rgb.len() / 3).checked_div(src_width).unwrap_or(0);
        if src_width == 0 || left + w > src_width || top + h > src_height {
            return Err(Error::Frame(format!(
                "a {w}×{h} window at {left},{top} does not fit a {src_width}×{src_height} image"
            )));
        }
        let light = self.meter.as_ref().and_then(|(m, _)| {
            rgb.get(top * src_width * 3..(top + h) * src_width * 3)
                .and_then(|window| m.measure(window, src_width * 3, left, w))
        });
        let mut px = Vec::with_capacity(w * h * 3);
        for y in top..top + h {
            let start = (y * src_width + left) * 3;
            let row = rgb
                .get(start..start + w * 3)
                .ok_or_else(|| Error::Frame(format!("row {y} of the window is missing")))?;
            match &self.table {
                Some(c) => {
                    for p in row.as_chunks::<3>().0 {
                        px.extend_from_slice(&c.apply(*p));
                    }
                }
                None => px.extend_from_slice(row),
            }
        }
        let data = self.encode(&px, rgb, src_width, left, top)?;
        let path = frame_path_as(&self.dir, &self.stem, self.next, self.format);
        // Atomic: a stopped render never leaves half a frame under a frame's name.
        let tmp = self
            .dir
            .join(format!(".{}.{}.tmp", self.stem, std::process::id()));
        let target = |e: std::io::Error| Error::Target {
            path: path.clone(),
            reason: e.to_string(),
        };
        let written = (|| {
            use std::io::Write as _;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&data)?;
            // On the disk before it takes a frame's name: a power cut leaves no torn frame.
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = written {
            // A full disk: the half-written frame must not keep the room a resume needs.
            let _ = std::fs::remove_file(&tmp);
            return Err(target(e));
        }
        // The folder's entry too (Unix: a directory can be synced), so the name survives a power cut.
        if let Ok(d) = std::fs::File::open(&self.dir) {
            let _ = d.sync_all();
        }
        // Its checksum, for the delivery's list (one line a frame, appended: a stop keeps them).
        // With the frame's size and time: a frame changed after it was written (retouched
        // between two runs) is read again for the list rather than listed with an old sum.
        let stamp = std::fs::metadata(&path).ok().map(|m| {
            (
                m.len(),
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos()),
            )
        });
        if let Some((len, time)) = stamp {
            append_own(
                &self.dir.join(SUMS),
                &format!("{} {} {len} {time}", self.next, sha256_hex(&data)),
            );
        }
        if let (Some((_, frames)), Some((m, a))) = (self.meter.as_mut(), light) {
            frames.insert(self.next, (m, a));
            // One line a frame, appended: a stop keeps what was measured.
            append_own(&self.dir.join(LIGHT), &format!("{} {m} {a}", self.next));
        }
        self.next += 1;
        // Renewed every few minutes: another computer tells a running render from a crashed one
        // by it (a frame of a slow render can take minutes).
        let path = self.dir.join(LOCK);
        let stale = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > std::time::Duration::from_secs(300));
        if stale {
            let own = std::process::id().to_string();
            if let Ok(text) = read_text(&path)
                && text.split_whitespace().next() == Some(own.as_str())
            {
                let _ = write_whole(&path, &text);
            }
        }
        self.written += 1;
        Ok(())
    }

    /// One frame's file: `px` the window in the transfer's values; EXR takes the window of `rgb`
    /// (the stages' values) to linear light.
    fn encode(
        &self,
        px: &[u16],
        rgb: &[u16],
        src_width: usize,
        left: usize,
        top: usize,
    ) -> Result<Vec<u8>, Error> {
        use crate::Transfer;
        use crate::still::{Chromaticities, DpxTransfer, Timing, encode_dpx_with, encode_exr_with};
        let timing = Timing {
            fps: Some(self.fps),
            index: u32::try_from(self.next).unwrap_or(u32::MAX),
            start: self.timecode,
            drop: self.drop_frame,
        };
        let (w, h) = (self.width, self.height);
        let failed = || Error::Encoder(format!("{} frame {w}×{h}", self.format.ext()));
        match self.format {
            FrameFormat::Tiff => {
                let img = EncodeImage::new(w as u32, h as u32, 3, Samples::U16(px));
                let meta = EncodeMeta {
                    icc: self.icc.as_deref(),
                    ..EncodeMeta::default()
                };
                encode_tiff(&img, TiffCompression::Deflate, &meta).map_err(|e| Error::Encoder(e.to_string()))
            }
            FrameFormat::Dpx => {
                let (transfer, name) = match self.transfer {
                    Transfer::Srgb => (DpxTransfer::Srgb, "sRGB"),
                    Transfer::Rec709 => (DpxTransfer::Rec709, "Rec. 709 (BT.1886)"),
                    Transfer::Pq { .. } => (DpxTransfer::Other, "PQ (ST 2084), Rec. 2020"),
                    Transfer::Hlg { .. } => (DpxTransfer::Other, "HLG, Rec. 2020"),
                };
                let file = frame_path_as(Path::new(""), &self.stem, self.next, self.format)
                    .to_string_lossy()
                    .into_owned();
                encode_dpx_with(w, h, px, transfer, name, timing, &file).ok_or_else(failed)
            }
            FrameFormat::Exr => {
                let (peak, chroma) = match self.transfer {
                    Transfer::Pq { peak } | Transfer::Hlg { peak } => (peak, Chromaticities::REC2020),
                    _ => (1.0, Chromaticities::REC709),
                };
                let linear = lapsewerk_core::hdr::linear_table(peak);
                let mut out = Vec::with_capacity(w * h * 3);
                for y in top..top + h {
                    let start = (y * src_width + left) * 3;
                    let row = rgb.get(start..start + w * 3).ok_or_else(failed)?;
                    out.extend(
                        row.iter()
                            .map(|&v| linear.get(usize::from(v)).copied().unwrap_or(0.0)),
                    );
                }
                encode_exr_with(w, h, &out, chroma, timing).ok_or_else(failed)
            }
        }
    }

    /// The folder, once every frame is in. PQ: the sequence's HDR10 light levels go beside the
    /// frames ([`HDR10_FILE`]), for the grading or delivery tool.
    #[must_use]
    pub fn finish(self) -> PathBuf {
        let dir = self.dir.clone();
        self.finish_checked().unwrap_or(dir)
    }

    /// The frame files past this render's end: those Lapsewerk wrote (an earlier, longer render's:
    /// removed when it finishes) and those someone put there named like a frame (kept, named).
    #[must_use]
    pub fn past_end(&self) -> (Vec<String>, Vec<String>) {
        frames_from(&self.dir, &self.stem, self.next, self.format)
            .into_iter()
            .filter(|n| is_frame_file(&self.dir.join(n)))
            .partition(|n| is_ours(&self.ours, n, &self.dir.join(n)))
    }

    /// Files named like this render's frames, not made by Lapsewerk here, that a fresh start kept.
    #[must_use]
    pub fn kept_foreign(&self) -> &[String] {
        &self.kept_foreign
    }

    /// [`Self::finish`], saying when the delivery's files could not be written (a full disk):
    /// the frames are there, but no INFO.txt or SHA256SUMS.txt describe them.
    ///
    /// # Errors
    /// When INFO.txt, SHA256SUMS.txt or HDR10.txt can't be written, or a frame can't be read for
    /// its checksum.
    pub fn finish_checked(self) -> Result<PathBuf, Error> {
        let failed = |name: &str, e: &dyn std::fmt::Display| Error::Target {
            path: self.dir.clone(),
            reason: format!(
                "{name} could not be written ({e}; is the disk full?): the frames are there, resume to finish the delivery"
            ),
        };
        // Frames of the same name past the end (a longer earlier render, files put there by
        // hand) would be taken for part of this one in an edit: gone.
        // All of them, gaps or not; only real frame files (a note named like one stays).
        for name in self.past_end().0 {
            let _ = std::fs::remove_file(self.dir.join(name));
        }
        // Someone's own INFO.txt there (a note, a client's file) is kept beside, not written over.
        let info = self.dir.join(INFO_FILE);
        if read_text(&info).is_ok_and(|t| !t.starts_with("Lapsewerk frame sequence")) {
            // Never over an earlier kept one.
            let keep = (1..1000)
                .map(|k| {
                    self.dir.join(if k == 1 {
                        "INFO (before Lapsewerk).txt".to_owned()
                    } else {
                        format!("INFO (before Lapsewerk) {k}.txt")
                    })
                })
                .find(|p| !p.exists());
            if let Some(keep) = keep {
                let _ = std::fs::rename(&info, keep);
            }
        }
        // The list first, INFO.txt last: a stop or a full disk in between leaves no INFO.txt
        // newer than the render's start, so it counts as not ended (resume finishes it).
        self.write_checksums().map_err(|e| failed(CHECKSUMS_FILE, &e))?;
        if let (Some(l), crate::Transfer::Pq { peak }) = (self.light_level(), self.transfer) {
            let md = lapsewerk_core::hdr::Hdr10Metadata::for_peak(peak);
            let max = md
                .mastering_display
                .get(16..20)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map_or(0, u32::from_be_bytes);
            // Frames there from an earlier run without their light log (an older version, a copy
            // without hidden files): the figures cover only some frames, and say so.
            let incomplete = (l.frames as usize) < self.next;
            let text = format!(
                "HDR10 (SMPTE ST 2084 / PQ, Rec. 2020)\n{}MaxCLL {} cd/m2\nMaxFALL {} cd/m2\nframes measured {} of {}\nMastering display (SMPTE ST 2086): Display P3 primaries, D65 white, max {} cd/m2, min 0.0001 cd/m2\nST 2086 bytes (G B R white xy, max min luminance) {}\n",
                if incomplete {
                    "INCOMPLETE: frames from an earlier render were not measured; render with --overwrite for the true values\n"
                } else {
                    ""
                },
                l.max_cll,
                l.max_fall,
                l.frames,
                self.next,
                max / 10_000,
                lapsewerk_core::hdr::hex(&md.mastering_display)
            );
            let hdr10 = self.dir.join(HDR10_FILE);
            if read_text(&hdr10).is_ok_and(|t| !t.starts_with("HDR10 (SMPTE ST 2084")) {
                keep_theirs(&hdr10, "HDR10");
            }
            write_whole(&hdr10, &text).map_err(|e| failed(HDR10_FILE, &e))?;
        }
        write_whole(&info, &self.info()).map_err(|e| failed(INFO_FILE, &e))?;
        Ok(self.dir.clone())
    }

    /// [`CHECKSUMS_FILE`] for every frame: those noted while writing, the others (an earlier run of
    /// an older version) read again. Someone's own file of that name is kept beside.
    fn write_checksums(&self) -> std::io::Result<()> {
        let noted = noted_sums(&self.dir);
        let mut text = String::new();
        for i in 0..self.next {
            let path = frame_path_as(&self.dir, &self.stem, i, self.format);
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                return Err(std::io::Error::other("a frame without a name"));
            };
            // Noted when written, and the frame unchanged since (same size and time); else read.
            let now = std::fs::metadata(&path).ok().map(|m| {
                (
                    m.len(),
                    m.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_nanos()),
                )
            });
            let sum = match (noted.get(&i), now) {
                (Some((h, (Some(len), Some(time)))), Some(now)) if (*len, *time) == now => h.clone(),
                _ => sha256_hex(&std::fs::read(&path)?),
            };
            text.push_str(&format!("{sum}  {name}\n"));
        }
        let path = self.dir.join(CHECKSUMS_FILE);
        if read_text(&path).is_ok_and(|t| !own_checksums(&t, &self.stem)) {
            keep_theirs(&path, "SHA256SUMS");
        }
        write_whole(&path, &text)
    }

    /// The text of [`INFO_FILE`].
    fn info(&self) -> String {
        let n = self.next;
        let name = |i: usize| {
            frame_path_as(Path::new(""), &self.stem, i, self.format)
                .to_string_lossy()
                .into_owned()
        };
        let files = if n == 0 {
            "none".to_owned()
        } else {
            format!(
                "{} … {} ({n} frames, numbered from 1, 6 digits)",
                name(0),
                name(n - 1)
            )
        };
        let hdr = matches!(
            self.transfer,
            crate::Transfer::Pq { .. } | crate::Transfer::Hlg { .. }
        );
        let format = match self.format {
            FrameFormat::Tiff => "TIFF, 16-bit RGB, Deflate".to_owned(),
            FrameFormat::Dpx => "DPX, 10-bit RGB (packing method A, big-endian)".to_owned(),
            FrameFormat::Exr => "OpenEXR, half-float RGB, ZIP".to_owned(),
        };
        let transfer = match (self.format, self.transfer) {
            (FrameFormat::Exr, _) if hdr => {
                "linear display light, 1.0 = SDR white (203 cd/m2); the HDR grade's light, not scene light".to_owned()
            }
            (FrameFormat::Exr, _) => {
                "linear display light (the encoded frames decoded with their curve), 1.0 = white; nothing above 1.0".to_owned()
            }
            (_, crate::Transfer::Srgb) => "sRGB (IEC 61966-2-1)".to_owned(),
            (_, crate::Transfer::Rec709) => "Rec. 709, BT.1886 (gamma 2.4)".to_owned(),
            (_, crate::Transfer::Pq { .. }) => "PQ (SMPTE ST 2084)".to_owned(),
            (_, crate::Transfer::Hlg { .. }) => {
                "HLG (ARIB STD-B67, BT.2100): nominal peak 1000 cd/m2, system gamma 1.2, reference white 203 cd/m2".to_owned()
            }
        };
        let range = match self.format {
            FrameFormat::Tiff => "full (0-65535)",
            FrameFormat::Dpx => "full (0-1023)",
            FrameFormat::Exr => "float",
        };
        let timecode = self.timecode.map_or_else(
            || "none".to_owned(),
            |start| {
                let last = start.saturating_add(u32::try_from(n.saturating_sub(1)).unwrap_or(u32::MAX));
                let base = (f64::from(self.fps.0) / f64::from(self.fps.1.max(1))).round();
                format!(
                    "{} - {} ({}, in each frame's header{})",
                    timecode_text(start, self.fps, self.drop_frame),
                    timecode_text(last, self.fps, self.drop_frame),
                    if self.drop_frame { "drop-frame" } else { "non-drop" },
                    if base > 60.0 {
                        "; none in the headers above 60 fps"
                    } else if base > 30.0 {
                        "; above 30 fps as frame pairs with the field bit, SMPTE ST 12-1"
                    } else {
                        ""
                    }
                )
            },
        );
        // Files of the frames' names found there and set aside (an edited frame): named, so
        // whoever takes the folder knows they are no part of the delivery.
        let set_aside = set_aside_files(&self.dir);
        let aside = if set_aside.is_empty() {
            String::new()
        } else {
            format!(
                "set aside  {} (files that were there under frames' names, e.g. edited ones; not part of this delivery)\n",
                set_aside.join(", ")
            )
        };
        format!(
            "Lapsewerk frame sequence (Lapsewerk {})\n\
             files      {files}\n\
             format     {format}\n\
             size       {} x {}, square pixels\n\
             rate       {:.3} fps ({}/{})\n\
             timecode   {timecode}\n\
             curve      {transfer}\n\
             primaries  {}\n\
             range      {range}\n\
             checksums  {CHECKSUMS_FILE} (SHA-256; check in this folder with: sha256sum -c {CHECKSUMS_FILE}, or lapsewerk verify .)\n{aside}",
            env!("CARGO_PKG_VERSION"),
            self.width,
            self.height,
            f64::from(self.fps.0) / f64::from(self.fps.1.max(1)),
            self.fps.0,
            self.fps.1,
            if hdr { "Rec. 2020" } else { "Rec. 709 / sRGB (D65)" },
        )
    }

    /// Frames in the folder from the first on (those of an earlier run too).
    #[must_use]
    pub fn frames_total(&self) -> usize {
        self.next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Codec, Transfer};

    fn settings() -> VideoSettings {
        VideoSettings {
            width: 4,
            height: 2,
            fps_num: 25,
            fps_den: 1,
            codec: Codec::ProRes422Hq,
            bitrate: None,
            transfer: Transfer::Srgb,
            timecode: None,
            drop_frame: false,
        }
    }

    #[test]
    fn files_named_like_frames_that_lapsewerk_did_not_make_are_never_removed() {
        // Photos named like an earlier delivery (`clip_000001.tif`), and no manifest: overwrite
        // and resume are both refused, and every file stays.
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-foreign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..3 {
            std::fs::write(frame_path(&dir, "clip", i), b"someone's photo").unwrap();
        }
        assert!(preflight(&dir, FrameFormat::Tiff, true, false, false).is_err());
        assert!(FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, true, false, "").is_err());
        assert!(FrameWriter::create_as(&dir, settings(), FrameFormat::Dpx, true, false, "").is_err());
        assert!(FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, true, "").is_err());
        for i in 0..3 {
            assert_eq!(
                std::fs::read(frame_path(&dir, "clip", i)).unwrap(),
                b"someone's photo"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_tiff_cut_short_anywhere_is_found() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-tiffcut-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w =
            FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, false, "a offsets 0").unwrap();
        for k in 0..5u16 {
            w.push(&[k * 999; 4 * 2 * 3], 4, 0, 0).unwrap();
        }
        let _ = w.finish();
        assert!(
            render_there(&dir, &settings(), FrameFormat::Tiff, None)
                .unwrap()
                .complete
        );
        // Frame 3 (of 5) cut to 60 %: a size check alone lets it pass, its directory does not.
        let cut = frame_path_as(&dir, "clip", 2, FrameFormat::Tiff);
        let bytes = std::fs::read(&cut).unwrap();
        assert!(tiff_whole(&cut));
        std::fs::write(&cut, &bytes[..bytes.len() * 6 / 10]).unwrap();
        assert!(!tiff_whole(&cut));
        assert!(
            !render_there(&dir, &settings(), FrameFormat::Tiff, None)
                .unwrap()
                .complete
        );
        let w =
            FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, true, "a offsets 0").unwrap();
        assert_eq!(w.next(), 2);
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_exr_cut_short_anywhere_is_found() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-exrcut-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w =
            FrameWriter::create_as(&dir, settings(), FrameFormat::Exr, false, false, "a offsets 0").unwrap();
        for k in 0..5u16 {
            w.push(&[k * 999; 4 * 2 * 3], 4, 0, 0).unwrap();
        }
        let _ = w.finish();
        assert!(
            render_there(&dir, &settings(), FrameFormat::Exr, None)
                .unwrap()
                .complete
        );
        let cut = frame_path_as(&dir, "clip", 2, FrameFormat::Exr);
        assert!(exr_whole(&cut, 4, 2));
        let bytes = std::fs::read(&cut).unwrap();
        std::fs::write(&cut, &bytes[..bytes.len() * 9 / 10]).unwrap();
        assert!(!exr_whole(&cut, 4, 2));
        // Checked as delivered: cut short by a tenth only, still called cut short (not changed).
        let (_, bad) = verify_checksums(&dir).unwrap();
        assert!(
            bad.iter().any(|b| b.starts_with("clip_000003.exr: cut short")),
            "{bad:?}"
        );
        assert!(
            !render_there(&dir, &settings(), FrameFormat::Exr, None)
                .unwrap()
                .complete
        );
        let w =
            FrameWriter::create_as(&dir, settings(), FrameFormat::Exr, false, true, "a offsets 0").unwrap();
        assert_eq!(w.next(), 2);
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resuming_sets_a_retouched_frame_aside_before_making_it_again() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-resumekeep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w = FrameWriter::create(&dir, settings(), true, false).unwrap();
        for k in 0..5u16 {
            w.push(&[10_000 + k * 1000; 4 * 2 * 3], 4, 0, 0).unwrap();
        }
        let dir = w.finish();
        // Frame 3 retouched (a whole picture, not what was written); a note saved as frame 4.
        let copy = std::fs::read(frame_path(&dir, "clip", 0)).unwrap();
        std::fs::write(frame_path(&dir, "clip", 2), &copy).unwrap();
        let w = FrameWriter::create(&dir, settings(), false, true).unwrap();
        assert_eq!(w.kept_foreign(), ["clip_000003.tif.kept".to_owned()]);
        assert_eq!(std::fs::read(dir.join("clip_000003.tif.kept")).unwrap(), copy);
        drop(w);
        // A fresh start sets aside a plain file of a frame's name that is no frame too.
        std::fs::write(frame_path(&dir, "clip", 3), b"a note").unwrap();
        let w = FrameWriter::create(&dir, settings(), true, false).unwrap();
        assert!(
            w.kept_foreign().iter().any(|n| n == "clip_000004.tif.kept"),
            "{:?}",
            w.kept_foreign()
        );
        assert_eq!(
            std::fs::read(dir.join("clip_000004.tif.kept")).unwrap(),
            b"a note"
        );
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_folder_named_like_a_frame_is_refused_before_rendering() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-dirframe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        std::fs::create_dir_all(dir.join("clip_000030.tif")).unwrap();
        let err = preflight(&dir, FrameFormat::Tiff, true, false, false).unwrap_err();
        assert!(err.to_string().contains("clip_000030.tif"), "{err}");
        assert!(!is_frame_file(&dir.join("clip_000030.tif")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_refused_render_leaves_no_lock() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-refused-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w =
            FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, false, "a offsets 0").unwrap();
        w.push(&[1u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        drop(w);
        // Resuming with other settings is refused, and the folder is free again.
        assert!(
            FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, true, "b offsets 0").is_err()
        );
        assert!(!dir.join(LOCK).exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_frame_cut_short_in_the_middle_is_made_again() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-cut-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w =
            FrameWriter::create_as(&dir, settings(), FrameFormat::Dpx, false, false, "a offsets 0").unwrap();
        for _ in 0..5 {
            w.push(&[1u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        }
        let _ = w.finish();
        let r = render_there(&dir, &settings(), FrameFormat::Dpx, None).unwrap();
        assert!(r.complete);
        // Frame 2 (of 5) cut short: not finished, and going on starts there.
        let cut = frame_path_as(&dir, "clip", 1, FrameFormat::Dpx);
        let bytes = std::fs::read(&cut).unwrap();
        std::fs::write(&cut, &bytes[..100]).unwrap();
        assert!(
            !render_there(&dir, &settings(), FrameFormat::Dpx, None)
                .unwrap()
                .complete
        );
        let w =
            FrameWriter::create_as(&dir, settings(), FrameFormat::Dpx, false, true, "a offsets 0").unwrap();
        assert_eq!(w.next(), 1);
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn two_renders_never_write_into_one_folder() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        std::fs::create_dir_all(&dir).unwrap();
        // A running process (this test's parent stands for "another render").
        let other = std::os::unix::process::parent_id();
        std::fs::write(dir.join(LOCK), other.to_string()).unwrap();
        assert!(preflight(&dir, FrameFormat::Tiff, true, false, false).is_err());
        // Said before its first frame, with the computer and the start (an old lock: no time).
        let there = render_there(&dir, &settings(), FrameFormat::Tiff, None).unwrap();
        assert!(there.busy && there.frames == 0);
        let h = there.holder.unwrap();
        assert_eq!((h.pid, h.computer.as_deref(), h.since), (other, None, None));
        assert!(h.describe().contains("on this computer"), "{}", h.describe());
        std::fs::write(dir.join(LOCK), format!("{other} {} studio 1700000000", host())).unwrap();
        let e = preflight(&dir, FrameFormat::Tiff, true, false, false)
            .unwrap_err()
            .to_string();
        assert!(e.contains("on this computer, since 2023-11-1"), "{e}");
        std::fs::write(dir.join(LOCK), "4242 another-id mac-studio 1700000000").unwrap();
        let h = lock_holder_at(&dir).unwrap();
        assert_eq!(h.computer.as_deref(), Some("mac-studio"));
        assert!(
            h.describe().starts_with("process 4242 on mac-studio, since "),
            "{}",
            h.describe()
        );
        assert!(FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, true, false, "").is_err());
        // A mark left by a process that ended is taken over, and freed when done.
        std::fs::write(dir.join(LOCK), "4000000000").unwrap();
        let w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, true, false, "").unwrap();
        assert!(dir.join(LOCK).exists());
        drop(w);
        assert!(!dir.join(LOCK).exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_frame_changed_since_it_was_written_is_made_again_on_resume() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-flip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, false, "s").unwrap();
        for v in [1u16, 2, 3] {
            w.push(&[v; 4 * 2 * 3], 4, 0, 0).unwrap();
        }
        let _ = w.finish();
        let copy = base.join("copy").join("clip");
        std::fs::create_dir_all(&copy).unwrap();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            std::fs::copy(e.path(), copy.join(e.file_name())).unwrap();
        }
        // A bit changed in the second frame, the size the same (a disk, a copy that went wrong).
        let second = frame_path_as(&dir, "clip", 1, FrameFormat::Tiff);
        let mut bytes = std::fs::read(&second).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        std::fs::write(&second, &bytes).unwrap();
        let w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, true, "s").unwrap();
        assert_eq!((w.next(), w.damaged()), (1, Some(1)));
        // Only the damaged one is named; the whole one after it may be kept.
        assert_eq!((w.damaged_all(), w.keepable()), (&[1][..], &[2][..]));
        drop(w);
        // A folder copied whole before (other times, the same bytes): nothing made again.
        let w = FrameWriter::create_as(&copy, settings(), FrameFormat::Tiff, false, true, "s").unwrap();
        assert_eq!(w.damaged(), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn verify_then_resume_makes_a_frame_changed_in_place_again() {
        // A bit changed, the size and the time as before: a resume alone trusts it, so verify
        // marks it, and the resume after makes it again (they went round in circles).
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-inplace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, false, "s").unwrap();
        for v in [1u16, 2, 3] {
            w.push(&[v; 4 * 2 * 3], 4, 0, 0).unwrap();
        }
        let _ = w.finish();
        let second = frame_path_as(&dir, "clip", 1, FrameFormat::Tiff);
        let time = std::fs::metadata(&second).unwrap().modified().unwrap();
        let mut bytes = std::fs::read(&second).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        std::fs::write(&second, &bytes).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&second)
            .unwrap()
            .set_modified(time)
            .unwrap();
        let (_, bad) = verify_checksums(&dir).unwrap();
        assert!(bad.iter().any(|b| b.ends_with(": changed")), "{bad:?}");
        let w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, true, "s").unwrap();
        assert_eq!(w.damaged(), Some(1));
        drop(w);
        // Half a frame of a render killed long ago is gone once a render holds the folder.
        std::fs::write(dir.join(".clip.4000000000.tmp"), b"half").unwrap();
        let w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, true, "s").unwrap();
        assert!(!dir.join(".clip.4000000000.tmp").exists());
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_refused_resume_names_the_values() {
        let dir = std::env::temp_dir().join(format!("lapsewerk-differs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let e = what_differs(
            "1920x1080 Srgb 0123456789abcdef",
            "1280x720 Srgb 0123456789abcdef",
            &dir,
        );
        assert!(e.contains("there 1920x1080 Srgb, here 1280x720 Srgb"), "{e}");
        std::fs::write(
            dir.join(INFO_FILE),
            "rate       25.000 fps (25/1)\ntimecode   10:00:00:00\n",
        )
        .unwrap();
        let e = what_differs("4x2 Srgb 0123456789abcdef", "4x2 Srgb fedcba9876543210", &dir);
        assert!(
            e.ends_with("there: 25.000 fps (25/1), timecode 10:00:00:00"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn checksums_follow_a_retouched_frame_and_never_write_through_a_link() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-sums2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, false, "s").unwrap();
        w.push(&[1u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        drop(w);
        // Retouched between the runs (another size), and the hidden list made a link to
        // someone's file.
        let first = frame_path_as(&dir, "clip", 0, FrameFormat::Tiff);
        let mut bytes = std::fs::read(&first).unwrap();
        bytes.extend_from_slice(b"retouched");
        std::fs::write(&first, &bytes).unwrap();
        let theirs = base.join("theirs.txt");
        std::fs::write(&theirs, "keep me\n").unwrap();
        let noted = std::fs::read_to_string(dir.join(SUMS)).unwrap();
        std::fs::remove_file(dir.join(SUMS)).unwrap();
        std::os::unix::fs::symlink(&theirs, dir.join(SUMS)).unwrap();
        let mut w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, true, "s").unwrap();
        w.push(&[9u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        let _ = w.finish();
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "keep me\n");
        assert!(
            !std::fs::symlink_metadata(dir.join(SUMS))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let list = std::fs::read_to_string(dir.join(CHECKSUMS_FILE)).unwrap();
        let first_line = list.lines().next().unwrap();
        assert!(first_line.starts_with(&sha256_hex(&bytes)), "{list}");
        assert!(!noted.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_finished_sequence_lists_its_checksums() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-sums-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, false, "s").unwrap();
        w.push(&[1u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        drop(w);
        // Stopped after one, resumed: the noted sum and the new one, and one read again (an
        // older version noted none).
        std::fs::write(dir.join(SUMS), "").unwrap();
        let mut w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, true, "s").unwrap();
        w.push(&[9u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        let _ = w.finish();
        let list = std::fs::read_to_string(dir.join(CHECKSUMS_FILE)).unwrap();
        let lines: Vec<&str> = list.lines().collect();
        assert_eq!(lines.len(), 2, "{list}");
        for (i, line) in lines.iter().enumerate() {
            let path = frame_path_as(&dir, "clip", i, FrameFormat::Tiff);
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let sum = sha256_hex(&std::fs::read(&path).unwrap());
            assert_eq!(*line, format!("{sum}  {name}"));
        }
        assert!(own_checksums(&list, "clip"));
        assert!(
            std::fs::read_to_string(dir.join(INFO_FILE))
                .unwrap()
                .contains("sha256sum -c")
        );
        // Checked again: all as written; a picture put beside is named; a "*name" list is read.
        assert_eq!(verify_checksums(&dir).unwrap(), (2, Vec::new()));
        std::fs::write(dir.join("Shot010_000001.tif"), b"II*\0").unwrap();
        let (_, bad) = verify_checksums(&dir).unwrap();
        assert!(
            bad.iter()
                .any(|b| b.starts_with("Shot010_000001.tif: extra picture")),
            "{bad:?}"
        );
        std::fs::remove_file(dir.join("Shot010_000001.tif")).unwrap();
        let star = list.replace("  ", " *");
        std::fs::write(dir.join(CHECKSUMS_FILE), &star).unwrap();
        assert_eq!(verify_checksums(&dir).unwrap(), (2, Vec::new()));
        // As `sha256sum --tag` writes it.
        let tag: String = list
            .lines()
            .filter_map(|l| l.split_once("  "))
            .map(|(h, n)| format!("SHA256 ({n}) = {h}\n"))
            .collect();
        std::fs::write(dir.join(CHECKSUMS_FILE), &tag).unwrap();
        assert_eq!(verify_checksums(&dir).unwrap(), (2, Vec::new()));
        std::fs::write(dir.join(CHECKSUMS_FILE), &list).unwrap();
        // The folder above names the sequence below; a file and a missing folder are told so.
        let above = verify_checksums(&base).unwrap_err();
        assert!(
            above.contains("finished frame sequences below it: clip"),
            "{above}"
        );
        let file = frame_path_as(&dir, "clip", 0, FrameFormat::Tiff);
        assert!(verify_checksums(&file).unwrap_err().contains("is a file"));
        assert!(
            verify_checksums(&base.join("nix"))
                .unwrap_err()
                .ends_with("no such folder")
        );
        // Someone's own list of that name is kept beside; a fresh render removes only ours.
        std::fs::write(dir.join(CHECKSUMS_FILE), "my notes\n").unwrap();
        let mut w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, true, false, "s").unwrap();
        w.push(&[2u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        let _ = w.finish();
        assert_eq!(
            std::fs::read_to_string(dir.join("SHA256SUMS (before Lapsewerk).txt")).unwrap(),
            "my notes\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(CHECKSUMS_FILE))
                .unwrap()
                .lines()
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn what_a_folder_holds_of_a_render_is_told() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-there-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        assert_eq!(render_there(&dir, &settings(), FrameFormat::Tiff, None), None);
        let mut w = FrameWriter::create_as(
            &dir,
            settings(),
            FrameFormat::Tiff,
            false,
            false,
            "abc offsets 0.1",
        )
        .unwrap();
        w.push(&[1u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        drop(w);
        // Stopped: one frame, the same settings (offsets within the tolerance) or others.
        let r = render_there(&dir, &settings(), FrameFormat::Tiff, Some("abc offsets 0.1001")).unwrap();
        assert_eq!((r.frames, r.complete, r.same), (1, false, Some(true)));
        let r = render_there(&dir, &settings(), FrameFormat::Tiff, Some("abd offsets 0.1")).unwrap();
        assert_eq!(r.same, Some(false));
        let mut w = FrameWriter::create_as(
            &dir,
            settings(),
            FrameFormat::Tiff,
            false,
            true,
            "abc offsets 0.1",
        )
        .unwrap();
        w.push(&[1u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        let _ = w.finish();
        let r = render_there(&dir, &settings(), FrameFormat::Tiff, None).unwrap();
        assert_eq!((r.frames, r.complete, r.same), (2, true, None));
        // A frame gone from the finished sequence: no longer complete.
        std::fs::remove_file(frame_path(&dir, "clip", 1)).unwrap();
        let r = render_there(&dir, &settings(), FrameFormat::Tiff, None).unwrap();
        assert_eq!((r.frames, r.complete), (1, false));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn renders_whose_offsets_agree_to_a_thousandth_are_one() {
        let a = "1920x1080 Rec709 abc offsets 0.25000,2.51126";
        assert!(same_render(a, "1920x1080 Rec709 abc offsets 0.25000,2.51125"));
        assert!(same_render(a, "1920x1080 Rec709 abc offsets 0.25100,2.51226"));
        assert!(!same_render(a, "1920x1080 Rec709 abc offsets 0.26000,2.51126"));
        assert!(!same_render(a, "1920x1080 Rec709 abd offsets 0.25000,2.51126"));
        assert!(!same_render(a, "1920x1080 Rec709 abc offsets 0.25000"));
        assert!(same_render("x y", "x y"));
    }

    #[test]
    fn a_lost_first_frame_is_made_again_and_the_rest_kept() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-first-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, false, "s").unwrap();
        for k in 0..4u16 {
            w.push(&[k * 100; 4 * 2 * 3], 4, 0, 0).unwrap();
        }
        let dir = w.finish();
        std::fs::remove_file(frame_path_as(&dir, "clip", 0, FrameFormat::Tiff)).unwrap();
        // Not taken for an empty folder: neither written over without asking nor started anew.
        assert!(preflight(&dir, FrameFormat::Tiff, false, false, false).is_err());
        let there = render_there(&dir, &settings(), FrameFormat::Tiff, None).unwrap();
        assert_eq!(there.damaged, vec!["clip_000001.tif".to_owned()]);
        let w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, true, "s").unwrap();
        assert_eq!(w.damaged_all(), &[0]);
        assert_eq!(w.keepable(), &[1, 2, 3]);
        assert!(frame_path_as(&dir, "clip", 1, FrameFormat::Tiff).exists());
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_left_over_lock_is_said_when_taken_over() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-leftlock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LOCK), format!("4000000000 {} box 1700000000\n", host())).unwrap();
        let _ = taken_over_locks();
        let w = FrameWriter::create(&dir, settings(), false, false).unwrap();
        let said = taken_over_locks();
        assert!(said.iter().any(|l| l.contains("process 4000000000")), "{said:?}");
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_hole_in_a_stopped_render_is_not_its_end() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-stophole-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, false, "s").unwrap();
        for k in 0..4u16 {
            w.push(&[k * 100; 4 * 2 * 3], 4, 0, 0).unwrap();
        }
        drop(w);
        std::fs::remove_file(frame_path_as(&dir, "clip", 1, FrameFormat::Tiff)).unwrap();
        let there = render_there(&dir, &settings(), FrameFormat::Tiff, None).unwrap();
        assert_eq!(there.damaged, vec!["clip_000002.tif".to_owned()]);
        assert!(!there.ended);
        let w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, true, "s").unwrap();
        assert_eq!(w.damaged_all(), &[1]);
        assert_eq!(w.keepable(), &[2, 3]);
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_stopped_render_is_checked_as_far_as_it_goes() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-stopped-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, false, "s").unwrap();
        for k in 0..3u16 {
            w.push(&[k * 100; 4 * 2 * 3], 4, 0, 0).unwrap();
        }
        // Stopped: no finish, no list.
        drop(w);
        assert!(stopped_render(&dir));
        assert_eq!(verify_checksums(&dir).unwrap(), (3, Vec::new()));
        // A list naming a frame twice counts it once; twice with another sum, it is said.
        let once = noted_list(&dir).unwrap();
        std::fs::write(dir.join(CHECKSUMS_FILE), once.repeat(2)).unwrap();
        assert_eq!(verify_checksums(&dir).unwrap(), (3, Vec::new()));
        let other = once
            .lines()
            .next()
            .unwrap()
            .replacen(|c: char| c.is_ascii_hexdigit(), "0", 1);
        let other = if other == once.lines().next().unwrap() {
            once.lines().next().unwrap().replacen('0', "1", 1)
        } else {
            other
        };
        std::fs::write(dir.join(CHECKSUMS_FILE), format!("{once}{other}\n")).unwrap();
        let (_, bad) = verify_checksums(&dir).unwrap();
        assert!(
            bad.iter()
                .any(|b| b.ends_with("listed twice with different checksums")),
            "{bad:?}"
        );
        std::fs::remove_file(dir.join(CHECKSUMS_FILE)).unwrap();
        let second = frame_path_as(&dir, "clip", 1, FrameFormat::Tiff);
        let mut bytes = std::fs::read(&second).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(&second, bytes).unwrap();
        let (ok, bad) = verify_checksums(&dir).unwrap();
        assert_eq!(ok, 2);
        assert_eq!(bad, vec!["clip_000002.tif: changed".to_owned()]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_pipe_or_device_in_the_folder_is_not_read() {
        let dir = std::env::temp_dir().join(format!("lapsewerk-frames-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let list = dir.join(CHECKSUMS_FILE);
        let made = std::process::Command::new("mkfifo")
            .arg(&list)
            .status()
            .is_ok_and(|s| s.success());
        if made {
            // Would block for ever when read.
            assert!(verify_checksums(&dir).is_err());
            std::fs::remove_file(&list).unwrap();
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/dev/zero", &list).unwrap();
            assert!(verify_checksums(&dir).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_delivery_file_that_cannot_be_written_is_an_error_not_a_finished_delivery() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-full-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w = FrameWriter::create(&dir, settings(), false, false).unwrap();
        w.push(&[1u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        // INFO.txt can't be put there (a folder of that name with something in it).
        std::fs::create_dir_all(dir.join(INFO_FILE).join("x")).unwrap();
        let e = w.finish_checked().unwrap_err().to_string();
        assert!(e.contains("could not be written"), "{e}");
        // Nothing half-written left beside.
        let tmp = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(tmp, 0);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_frame_deleted_in_a_copy_is_made_again_and_the_rest_kept() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-gap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, false, "s").unwrap();
        for k in 0..6u16 {
            w.push(&[k * 100; 4 * 2 * 3], 4, 0, 0).unwrap();
        }
        let dir = w.finish();
        std::fs::remove_file(frame_path_as(&dir, "clip", 2, FrameFormat::Tiff)).unwrap();
        // Not to be resumed blindly, and said with the frames there.
        let held = preflight(&dir, FrameFormat::Tiff, false, false, false)
            .unwrap_err()
            .to_string();
        assert!(held.contains("holds 5 frames already"), "{held}");
        // Said before going on: missing, by name, not a render stopped there.
        let there = render_there(&dir, &settings(), FrameFormat::Tiff, None).unwrap();
        assert_eq!(there.damaged, vec!["clip_000003.tif".to_owned()]);
        // A delivery that ended and was damaged since, not a stopped export.
        assert!(there.ended);
        let w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, false, true, "s").unwrap();
        assert_eq!(w.damaged_all(), &[2]);
        assert_eq!(w.keepable(), &[3, 4, 5]);
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_renamed_folder_is_told_before_and_on_rendering() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-renamed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("A");
        let mut w = FrameWriter::create(&dir, settings(), false, false).unwrap();
        w.push(&[1u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        w.push(&[2u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        let dir = w.finish();
        assert_eq!(renamed_frames(&dir), None);
        let moved = base.join("Kunde_v2");
        std::fs::rename(&dir, &moved).unwrap();
        assert_eq!(renamed_frames(&moved).as_deref(), Some("A"));
        let refused = preflight(&moved, FrameFormat::Tiff, true, false, false)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(refused.contains("move the A_… frames out"), "{refused}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn frames_keep_the_name_given_and_another_is_refused_over_their_frames() {
        assert_eq!(clean_name(" SHOT010_v001 ").as_deref(), Ok("SHOT010_v001"));
        for bad in ["", "Lief ä", "a/b", ".hidden", "-x", &"x".repeat(65)] {
            assert!(clean_name(bad).is_err(), "{bad:?}");
        }
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("out");
        assert_eq!(frame_stem(&dir), "out");
        preflight_named(&dir, FrameFormat::Tiff, false, false, false, Some("A_1")).unwrap();
        assert_eq!(frame_stem(&dir), "A_1");
        // Frames made under it: going on or replacing without a name keeps it.
        let img = vec![30_000u16; 4 * 2 * 3];
        let mut w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, true, false, "").unwrap();
        w.push(&img, 4, 0, 0).unwrap();
        drop(w);
        assert!(dir.join("A_1_000001.tif").is_file());
        preflight_named(&dir, FrameFormat::Tiff, true, false, false, None).unwrap();
        assert_eq!(frame_stem(&dir), "A_1");
        // Another name over them: refused, going on and replacing alike, and the name stays.
        for (overwrite, resume) in [(false, true), (true, false)] {
            let e = preflight_named(&dir, FrameFormat::Tiff, overwrite, resume, false, Some("B2"))
                .unwrap_err()
                .to_string();
            assert!(e.contains("--name A_1"), "{e}");
            assert_eq!(frame_stem(&dir), "A_1");
        }
        // A link where the name goes is replaced, never written through.
        let other = base.join("victim.txt");
        std::fs::write(&other, "someone's note").unwrap();
        let fresh = base.join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&other, fresh.join(NAME_FILE)).unwrap();
        preflight_named(&fresh, FrameFormat::Tiff, false, false, false, Some("SHOT")).unwrap();
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "someone's note");
        assert_eq!(frame_stem(&fresh), "SHOT");
        // A name file edited by hand into something unsound, or too large, is not taken.
        std::fs::write(fresh.join(NAME_FILE), "../x\n").unwrap();
        assert_eq!(frame_stem(&fresh), "fresh");
        std::fs::write(fresh.join(NAME_FILE), "BIG\n".repeat(2000)).unwrap();
        assert_eq!(frame_stem(&fresh), "fresh");
        // A folder in its place: said by name.
        let held = base.join("held");
        std::fs::create_dir_all(held.join(NAME_FILE)).unwrap();
        let e = preflight_named(&held, FrameFormat::Tiff, false, false, false, Some("SHOT"))
            .unwrap_err()
            .to_string();
        assert!(e.contains(NAME_FILE), "{e}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_resume_of_another_size_is_refused_before_measuring() {
        let dir = std::env::temp_dir()
            .join(format!("lapsewerk-frames-early-{}", std::process::id()))
            .join("clip");
        let _ = std::fs::remove_dir_all(&dir);
        let early = "size: 1920x1080\nframe rate: 25/1\ncurve: sRGB";
        // Nothing there yet: nothing to compare.
        std::fs::create_dir_all(&dir).unwrap();
        assert!(resume_preflight(&dir, FrameFormat::Tiff, early).is_ok());
        let img = vec![30_000u16; 4 * 2 * 3];
        let mut w = FrameWriter::create_as(&dir, settings(), FrameFormat::Tiff, true, false, "").unwrap();
        w.push(&img, 4, 0, 0).unwrap();
        drop(w);
        std::fs::write(
            dir.join(SETTINGS_WORDS),
            "size: 1280x720\nframe rate: 25/1\ncurve: sRGB\nframes: 240\ndeflicker: 2\nkeyframe 1: aaa\n",
        )
        .unwrap();
        // Settings the early check doesn't know (frames, deflicker, keyframes) are left for later.
        let e = resume_preflight(&dir, FrameFormat::Tiff, early)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("rendered with other settings (size: there 1280x720, here 1920x1080)"),
            "{e}"
        );
        let same = "size: 1280x720\nframe rate: 25/1\ncurve: sRGB";
        assert!(resume_preflight(&dir, FrameFormat::Tiff, same).is_ok());
        // Frames of another format are no render of this one's to compare with.
        assert!(resume_preflight(&dir, FrameFormat::Dpx, early).is_ok());
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn a_refused_resume_says_what_differs_in_words() {
        let dir = std::env::temp_dir().join(format!("lapsewerk-frames-words-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(SETTINGS_WORDS),
            "curve: Rec. 709\nkeep light: 0.3\nkeyframe 1: aaa\nkeyframe 12: bbb\n",
        )
        .unwrap();
        let why = words_differ(
            &dir,
            "curve: Rec. 709\nkeep light: 0.5\nkeyframe 1: aaa\nkeyframe 12: ccc\nkeyframe 30: ddd",
        )
        .unwrap();
        assert_eq!(
            why,
            "keep light: there 0.3, here 0.5; keyframe 12 edited since (in the app, LightCraft or by hand); keyframe 30 added since"
        );
        assert!(
            other_format(FrameFormat::Tiff, FrameFormat::Dpx)
                .starts_with("its frames are TIFF files, here DPX")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_finished_sequence_says_what_it_is_and_keeps_others_files() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-info-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        let mut s = settings();
        s.timecode = Some(90_000);
        let mut w = FrameWriter::create(&dir, s, false, false).unwrap();
        // A frame of the same name past the end, from somewhere else.
        std::fs::write(frame_path(&dir, "clip", 2), b"II*\0old").unwrap();
        // A file named like a frame that is no picture is not ours to remove.
        std::fs::write(frame_path(&dir, "clip", 3), b"a note").unwrap();
        w.push(&[1u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        w.push(&[2u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        // Not Lapsewerk's (in none of its lists): kept, and named.
        assert_eq!(w.past_end(), (Vec::new(), vec!["clip_000003.tif".to_owned()]));
        let dir = w.finish();
        assert!(frame_path(&dir, "clip", 2).exists());
        assert!(frame_path(&dir, "clip", 3).exists());
        let info = std::fs::read_to_string(dir.join(INFO_FILE)).unwrap();
        assert!(
            info.contains("clip_000001.tif … clip_000002.tif (2 frames"),
            "{info}"
        );
        assert!(info.contains("01:00:00:00 - 01:00:00:01"), "{info}");
        assert!(info.contains("4 x 2"), "{info}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_folder_is_checked_before_anything_is_rendered() {
        let base = std::env::temp_dir().join(format!("lapsewerk-frames-pre-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clip");
        assert!(preflight(&dir, FrameFormat::Tiff, false, false, false).is_ok());
        let mut w = FrameWriter::create(&dir, settings(), false, false).unwrap();
        w.push(&[1u16; 4 * 2 * 3], 4, 0, 0).unwrap();
        assert!(preflight(&dir, FrameFormat::Tiff, false, false, false).is_err());
        assert!(preflight(&dir, FrameFormat::Tiff, false, true, false).is_ok());
        assert!(preflight(&dir, FrameFormat::Tiff, true, false, false).is_ok());
        assert!(preflight(&dir, FrameFormat::Dpx, false, true, false).is_err());
        // Another format, neither resumed nor overwritten: the TIFF frames are not swept away.
        assert!(preflight(&dir, FrameFormat::Dpx, false, false, false).is_err());
        assert!(preflight(&dir, FrameFormat::Dpx, true, false, false).is_ok());
        let file = base.join("file");
        std::fs::write(&file, b"x").unwrap();
        assert!(preflight(&file, FrameFormat::Tiff, true, false, false).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_resumed_pq_sequence_reports_the_light_of_all_its_frames() {
        let dir = std::env::temp_dir()
            .join(format!("lapsewerk-frames-pq-{}", std::process::id()))
            .join("clip");
        let _ = std::fs::remove_dir_all(&dir);
        let mut pq = settings();
        pq.transfer = Transfer::Pq { peak: 4.0 };
        let bright = vec![65_535u16; 4 * 2 * 3];
        let dark = vec![10_000u16; 4 * 2 * 3];
        // The bright frame first, then a stop.
        let mut w = FrameWriter::create(&dir, pq, false, false).unwrap();
        w.push(&bright, 4, 0, 0).unwrap();
        w.push(&dark, 4, 0, 0).unwrap();
        let first = w.light_level().unwrap();
        drop(w);
        // Resumed: only dark frames follow, the bright one still counts.
        let mut w = FrameWriter::create(&dir, pq, false, true).unwrap();
        assert_eq!(w.next(), 2);
        w.push(&dark, 4, 0, 0).unwrap();
        let all = w.light_level().unwrap();
        assert_eq!((all.max_cll, all.frames), (first.max_cll, 3));
        let dir_done = w.finish();
        let text = std::fs::read_to_string(dir_done.join(HDR10_FILE)).unwrap();
        assert!(
            text.contains(&format!("MaxCLL {} cd/m2", first.max_cll)),
            "{text}"
        );
        assert!(
            text.contains("frames measured 3 of 3") && text.contains("Mastering display"),
            "{text}"
        );
        assert!(!text.contains("INCOMPLETE"), "{text}");
        // Without the light of the frames there (a copy without hidden files) a resume would
        // report wrong figures: refused.
        let log = std::fs::read(dir.join(LIGHT)).unwrap();
        std::fs::remove_file(dir.join(LIGHT)).unwrap();
        assert!(FrameWriter::create(&dir, pq, false, true).is_err());
        std::fs::write(dir.join(LIGHT), log).unwrap();
        // A fresh start forgets them.
        let w = FrameWriter::create(&dir, pq, true, false).unwrap();
        assert!(w.light_level().is_none());
        assert!(!dir.join(HDR10_FILE).exists());
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn a_fresh_start_removes_its_own_frames_and_keeps_a_copy_put_there() {
        let dir = std::env::temp_dir()
            .join(format!("lapsewerk-frames-keepcopy-{}", std::process::id()))
            .join("clip");
        let _ = std::fs::remove_dir_all(&dir);
        let img = vec![30_000u16; 4 * 2 * 3];
        let mut w = FrameWriter::create(&dir, settings(), true, false).unwrap();
        for k in 0..3u16 {
            w.push(&[10_000 + k * 1000; 4 * 2 * 3], 4, 0, 0).unwrap();
        }
        let dir = w.finish();
        // A retouched copy beside them, named like frame 5; another over its own frame 3 (named
        // in its lists, but no longer what it wrote).
        std::fs::copy(frame_path(&dir, "clip", 0), frame_path(&dir, "clip", 4)).unwrap();
        std::fs::copy(frame_path(&dir, "clip", 0), frame_path(&dir, "clip", 2)).unwrap();
        let copy = std::fs::read(frame_path(&dir, "clip", 0)).unwrap();
        let mut w = FrameWriter::create(&dir, settings(), true, false).unwrap();
        assert_eq!(
            w.kept_foreign(),
            [
                "clip_000003.tif.kept".to_owned(),
                "clip_000005.tif.kept".to_owned()
            ]
        );
        assert!(!frame_path(&dir, "clip", 1).exists(), "its own earlier frames go");
        for _ in 0..3 {
            w.push(&img, 4, 0, 0).unwrap();
        }
        let dir = w.finish();
        // Set aside whole: frame 3 made anew does not write over the copy.
        assert_eq!(std::fs::read(dir.join("clip_000003.tif.kept")).unwrap(), copy);
        assert!(dir.join("clip_000005.tif.kept").exists(), "the copy stays");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn dpx_and_exr_frames_resume_and_replace_each_other() {
        let dir = std::env::temp_dir()
            .join(format!("lapsewerk-frames-fmt-{}", std::process::id()))
            .join("clip");
        let _ = std::fs::remove_dir_all(&dir);
        let img = vec![30_000u16; 4 * 2 * 3];
        for format in [FrameFormat::Dpx, FrameFormat::Exr] {
            let mut w = FrameWriter::create_as(&dir, settings(), format, true, false, "").unwrap();
            for _ in 0..3 {
                w.push(&img, 4, 0, 0).unwrap();
            }
            assert_eq!(finished_frames_as(&dir, "clip", format), 3);
            // The other format's frames went when this sequence started.
            assert_eq!(
                finished_frames_as(&dir, "clip", FrameFormat::Dpx)
                    + finished_frames_as(&dir, "clip", FrameFormat::Exr),
                3
            );
            let last = frame_path_as(&dir, "clip", 2, format);
            assert!(whole_frame(&last, format, 4, 2));
            // Cut short: made again on resume.
            let bytes = std::fs::read(&last).unwrap();
            std::fs::write(&last, &bytes[..bytes.len() - 3]).unwrap();
            let r = FrameWriter::create_as(&dir, settings(), format, false, true, "").unwrap();
            assert_eq!(r.next(), 2);
            // The format is part of the settings: no resume in another.
            let other = if format == FrameFormat::Dpx {
                FrameFormat::Exr
            } else {
                FrameFormat::Dpx
            };
            assert!(FrameWriter::create_as(&dir, settings(), other, false, true, "").is_err());
        }
        assert_eq!(FrameFormat::parse("TIFF"), Some(FrameFormat::Tiff));
        assert_eq!(FrameFormat::parse("mov"), None);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn writes_numbered_frames_and_resumes() {
        let dir = std::env::temp_dir()
            .join(format!("lapsewerk-frames-{}", std::process::id()))
            .join("clip");
        let _ = std::fs::remove_dir_all(&dir);
        let img = vec![30_000u16; 6 * 3 * 3];
        let mut w = FrameWriter::create(&dir, settings(), false, false).unwrap();
        for _ in 0..3 {
            w.push(&img, 6, 1, 0).unwrap();
        }
        assert_eq!(w.frames(), 3);
        assert!(frame_path(&dir, "clip", 2).ends_with("clip_000003.tif"));
        assert_eq!(finished_frames(&dir, "clip"), 3);
        let bytes = std::fs::read(frame_path(&dir, "clip", 0)).unwrap();
        assert!(bytes.starts_with(b"II") || bytes.starts_with(b"MM"));
        // A folder with frames: refused, resumed after them, or overwritten from the start.
        assert!(FrameWriter::create(&dir, settings(), false, false).is_err());
        assert_eq!(
            FrameWriter::create(&dir, settings(), false, true).unwrap().next(),
            3
        );
        // A window that does not fit.
        assert!(w.push(&img, 6, 4, 0).is_err());
        // Other settings: no resume. A damaged last frame is made again. Overwriting a longer
        // sequence leaves none of its frames past the new end.
        let mut other = settings();
        other.width = 2;
        assert!(FrameWriter::create(&dir, other, false, true).is_err());
        assert!(FrameWriter::create_for(&dir, settings(), false, true, "keyframes changed").is_err());
        // Cut in half: the header is there, the pixels are not.
        let whole = std::fs::read(frame_path(&dir, "clip", 2)).unwrap();
        std::fs::write(frame_path(&dir, "clip", 2), &whole[..whole.len() / 2]).unwrap();
        assert_eq!(
            FrameWriter::create(&dir, settings(), false, true).unwrap().next(),
            2
        );
        let mut fresh = FrameWriter::create(&dir, settings(), true, false).unwrap();
        fresh.push(&img, 6, 0, 0).unwrap();
        assert_eq!(finished_frames(&dir, "clip"), 1);
        // The colour profile is in the file.
        let bytes = std::fs::read(frame_path(&dir, "clip", 0)).unwrap();
        assert!(bytes.windows(4).any(|w| w == b"acsp"), "an ICC profile");
        assert_eq!(fingerprint("a"), fingerprint("a"));
        assert_ne!(fingerprint("a"), fingerprint("b"));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}
