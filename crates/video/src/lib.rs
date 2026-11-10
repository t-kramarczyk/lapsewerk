//! Video files from rendered frames: Apple ProRes and HEVC in QuickTime movies, written by Apple's
//! own encoders through AVFoundation.
//!
//! The encoding runs in a small helper program (`encoder.swift`, built with this crate) that reads
//! frames on its standard input, so Lapsewerk itself stays free of unsafe code and a failing encoder
//! cannot take the app down. [`Encoder`] writes to a temporary file beside the target and moves it
//! into place only when the movie is complete: an interrupted export never leaves a broken movie
//! under the requested name; [`abort_all`] does the same for every movie still being written when the
//! process is interrupted (Ctrl-C), since destructors do not run then.

pub mod frames;
pub mod space;
pub mod still;
pub use frames::FrameWriter;

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Video codecs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Codec {
    /// ProRes 422 HQ: the usual master for editing, 10-bit 4:2:2.
    #[default]
    ProRes422Hq,
    /// ProRes 422: about two thirds of HQ's size.
    ProRes422,
    /// ProRes 4444: 12-bit full chroma, for grading.
    ProRes4444,
    /// HEVC (H.265) Main 10: small files to share and play.
    Hevc,
}

impl Codec {
    /// Parses `prores422hq`, `prores422`, `prores4444`, `hevc` (case and `-`/`_` ignored).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let k: String = s
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        match k.as_str() {
            "prores422hq" | "prores" | "proreshq" => Some(Self::ProRes422Hq),
            "prores422" => Some(Self::ProRes422),
            "prores4444" => Some(Self::ProRes4444),
            "hevc" | "h265" => Some(Self::Hevc),
            _ => None,
        }
    }

    fn arg(self) -> &'static str {
        match self {
            Self::ProRes422Hq => "prores422hq",
            Self::ProRes422 => "prores422",
            Self::ProRes4444 => "prores4444",
            Self::Hevc => "hevc",
        }
    }
}

/// How the movie's values are encoded and tagged. SDR frames come in sRGB-encoded (Rec. 709 / sRGB
/// primaries) and are tagged Rec. 709 primaries and matrix; HDR frames come as
/// [`lapsewerk_core::hdr::encode_frame`] made them (Rec. 2020 primaries) and are tagged Rec. 2020.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Transfer {
    /// The sRGB curve the frames are rendered with, tagged as such: Apple players and browsers show
    /// the movie exactly as the stills look in LightCraft. For the web, phones, YouTube.
    #[default]
    Srgb,
    /// Broadcast master: values re-encoded for a BT.1886 display (gamma 2.4, the reference for HD
    /// television and streaming SDR deliveries) and tagged ITU-R BT.709, so a studio monitor shows
    /// what LightCraft showed. (Apple players show such files a little brighter: that is how they
    /// treat every Rec. 709 file.)
    Rec709,
    /// HDR, PQ (SMPTE ST 2084, HDR10): frames rendered with highlights up to `peak` × SDR white,
    /// SDR white at 203 cd/m².
    Pq { peak: f32 },
    /// HDR, HLG (ITU-R BT.2100): as [`Self::Pq`], SDR white at 75 % of the signal, clipped at
    /// 1000 cd/m².
    Hlg { peak: f32 },
}

/// How the stages' 16-bit values (sRGB curve of `value / peak`) become a transfer's signal.
pub(crate) enum Conversion {
    /// Each value on its own.
    Table(Vec<u16>),
    /// HLG: per pixel (BT.2100's OOTF works on the pixel's luminance), from each value's display
    /// light in cd/m².
    Hlg(Vec<f32>),
}

impl Conversion {
    /// The signal of a pixel.
    pub(crate) fn apply(&self, px: [u16; 3]) -> [u16; 3] {
        match self {
            Self::Table(t) => px.map(|v| t.get(usize::from(v)).copied().unwrap_or(v)),
            Self::Hlg(nits) => {
                let n = px.map(|v| f64::from(nits.get(usize::from(v)).copied().unwrap_or(0.0)));
                lapsewerk_core::hdr::hlg_encode_pixel(n)
                    .map(|s| (s * 65535.0).round().clamp(0.0, 65535.0) as u16)
            }
        }
    }
}

impl Transfer {
    /// How values become this transfer's signal (`None`: unchanged).
    pub(crate) fn conversion(self) -> Option<Conversion> {
        match self {
            Self::Hlg { peak } => Some(Conversion::Hlg(
                lapsewerk_core::hdr::linear_table(peak)
                    .into_iter()
                    .map(|l| l * lapsewerk_core::hdr::SDR_WHITE_NITS as f32)
                    .collect(),
            )),
            _ => self.table().map(Conversion::Table),
        }
    }

    /// Table from sRGB-encoded 16-bit values to this transfer's (`None`: unchanged); HLG's is
    /// for grey only (see [`Self::conversion`]).
    pub(crate) fn table(self) -> Option<Vec<u16>> {
        use lapsewerk_core::hdr::{Signal, signal_table};
        match self {
            Self::Srgb => None,
            Self::Pq { peak } => Some(signal_table(Signal::Pq, peak)),
            Self::Hlg { peak } => Some(signal_table(Signal::Hlg, peak)),
            Self::Rec709 => Some(
                (0..=u16::MAX)
                    .map(|v| {
                        let e = f64::from(v) / 65535.0;
                        let lin = if e <= 0.040_45 {
                            e / 12.92
                        } else {
                            ((e + 0.055) / 1.055).powf(2.4)
                        };
                        (lin.powf(1.0 / 2.4) * 65535.0).round().clamp(0.0, 65535.0) as u16
                    })
                    .collect(),
            ),
        }
    }

    /// Whether this is an HDR signal (frames rendered with HDR headroom, Rec. 2020).
    #[must_use]
    pub fn is_hdr(self) -> bool {
        matches!(self, Self::Pq { .. } | Self::Hlg { .. })
    }

    /// The helper's `--transfer` argument.
    fn arg(self) -> &'static str {
        match self {
            Self::Srgb => "srgb",
            Self::Rec709 => "709",
            Self::Pq { .. } => "pq",
            Self::Hlg { .. } => "hlg",
        }
    }
}

/// Size, rate and codec of a movie.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoSettings {
    pub width: usize,
    pub height: usize,
    /// Frame rate as a fraction: 25/1, 30000/1001 …
    pub fps_num: u32,
    pub fps_den: u32,
    pub codec: Codec,
    /// HEVC bit rate in bits per second; `None`: about 0.2 bits a pixel. ProRes ignores it.
    pub bitrate: Option<u64>,
    pub transfer: Transfer,
    /// A timecode track starting at this frame count (from 00:00:00:00, at the rate rounded to
    /// whole frames; drop-frame counts as [`parse_timecode_df`]); `None`: no timecode track.
    pub timecode: Option<u32>,
    /// The timecode counts drop-frame (29.97 and 59.94: frame numbers 0 and 1, or 0 to 3, left
    /// out at each minute but every tenth, so the timecode keeps to the clock).
    pub drop_frame: bool,
}

impl VideoSettings {
    fn check(&self) -> Result<(), Error> {
        let size_ok = |v: usize| (16..=16_384).contains(&v) && v.is_multiple_of(2);
        if !size_ok(self.width) || !size_ok(self.height) {
            return Err(Error::Settings(format!(
                "{}×{}: width and height must be even, 16 to 16384",
                self.width, self.height
            )));
        }
        if !(1..=240_000).contains(&self.fps_num) || !(1..=10_000).contains(&self.fps_den) {
            return Err(Error::Settings(format!(
                "frame rate {}/{}",
                self.fps_num, self.fps_den
            )));
        }
        Ok(())
    }
}

/// The start frame of a non-drop timecode `HH:MM:SS:FF` at a rate of `fps_num / fps_den` (counted
/// in whole frames: 30 for 29.97). `None` when it is not a timecode within a day at that rate, and
/// for a drop-frame timecode (`;`): [`parse_timecode_df`] reads both.
#[must_use]
pub fn parse_timecode(s: &str, fps_num: u32, fps_den: u32) -> Option<u32> {
    let base = (f64::from(fps_num) / f64::from(fps_den.max(1))).round() as u32;
    let parts: Vec<u32> = s
        .trim()
        .split(':')
        .map(|p| p.parse().ok())
        .collect::<Option<Vec<u32>>>()?;
    let [h, m, sec, f] = parts.as_slice() else {
        return None;
    };
    if base == 0 || *h >= 24 || *m >= 60 || *sec >= 60 || *f >= base {
        return None;
    }
    Some(((h * 60 + m) * 60 + sec) * base + f)
}

/// The start frame of a timecode at a rate of `fps_num / fps_den`, and whether it is drop-frame:
/// `HH:MM:SS:FF` non-drop as [`parse_timecode`], or `HH:MM:SS;FF` drop-frame at 29.97 or 59.94
/// (frame numbers 0 and 1, at 59.94 0 to 3, do not exist at the start of a minute but every
/// tenth). The frame count is that of the frames the timecode stands for, from 00:00:00;00.
#[must_use]
pub fn parse_timecode_df(s: &str, fps_num: u32, fps_den: u32) -> Option<(u32, bool)> {
    let s = s.trim();
    if !s.contains(';') {
        return parse_timecode(s, fps_num, fps_den).map(|c| (c, false));
    }
    let base = (f64::from(fps_num) / f64::from(fps_den.max(1))).round() as u32;
    if fps_den != 1001 || !(base == 30 || base == 60) {
        return None;
    }
    let (time, f) = s.rsplit_once(';')?;
    let parts: Vec<u32> = time
        .split(':')
        .chain(std::iter::once(f))
        .map(|p| p.parse().ok())
        .collect::<Option<Vec<u32>>>()?;
    let [h, m, sec, f] = parts.as_slice() else {
        return None;
    };
    let dropped = base / 15;
    if *h >= 24 || *m >= 60 || *sec >= 60 || *f >= base || (*sec == 0 && m % 10 != 0 && *f < dropped) {
        return None;
    }
    let minutes = h * 60 + m;
    Some((
        ((h * 60 + m) * 60 + sec) * base + f - dropped * (minutes - minutes / 10),
        true,
    ))
}

/// The drop-frame timecode (hours, minutes, seconds, frames) of frame `count` at the whole-frame
/// rate `base` (30 or 60), as [`parse_timecode_df`] counts.
#[must_use]
pub fn drop_frame_time(count: u32, base: u32) -> (u32, u32, u32, u32) {
    let dropped = (base / 15).max(1);
    let per_minute = base * 60 - dropped;
    let per_ten = per_minute * 10 + dropped;
    let c = count % (per_ten * 6 * 24);
    let (tens, rest) = (c / per_ten, c % per_ten);
    let shown = if rest < dropped {
        c + dropped * 9 * tens
    } else {
        c + dropped * 9 * tens + dropped * ((rest - dropped) / per_minute)
    };
    let (f, s) = (shown % base, shown / base);
    (s / 3600, (s / 60) % 60, s % 60, f)
}

/// Parses a frame rate: `25`, `29.97` (read as 30000/1001), `30000/1001`.
#[must_use]
pub fn parse_fps(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    if let Some((n, d)) = s.split_once('/') {
        let (n, d) = (n.trim().parse().ok()?, d.trim().parse().ok()?);
        return (n > 0 && d > 0).then_some((n, d));
    }
    match s {
        "23.976" | "23.98" => return Some((24_000, 1001)),
        "29.97" => return Some((30_000, 1001)),
        "59.94" => return Some((60_000, 1001)),
        _ => {}
    }
    if let Ok(n) = s.parse::<u32>() {
        return (n > 0).then_some((n, 1));
    }
    // Any other rate written as a decimal: whole (24.0), an NTSC rate (47.952 = 48000/1001), or
    // to the thousandth.
    let v: f64 = s.replace(',', ".").parse().ok()?;
    if !(v.is_finite() && v > 0.0 && v <= 1000.0) {
        return None;
    }
    let whole = v.round();
    if (v - whole).abs() < 1e-6 {
        return Some((whole as u32, 1));
    }
    let ntsc = (v * 1.001).round();
    if (v - ntsc * 1000.0 / 1001.0).abs() < 0.005 {
        return Some(((ntsc as u32) * 1000, 1001));
    }
    Some(((v * 1000.0).round() as u32, 1000))
}

/// Why a movie could not be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Settings(String),
    /// The helper program was not found or would not start.
    Helper(String),
    /// The target exists (and overwriting was not asked for) or cannot be written.
    Target {
        path: PathBuf,
        reason: String,
    },
    /// A frame of the wrong size.
    Frame(String),
    /// The encoder failed; its message.
    Encoder(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Settings(e) => write!(f, "video settings: {e}"),
            Self::Helper(e) => write!(f, "video encoder helper: {e}"),
            Self::Target { path, reason } => write!(f, "{}: {reason}", path.display()),
            Self::Frame(e) => write!(f, "frame: {e}"),
            Self::Encoder(e) => write!(f, "encoder: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// File name of the helper program.
pub const HELPER_NAME: &str = "lapsewerk-encode";

/// The helper program: beside the running executable (the app bundle, an installed copy), else the
/// one built with this crate.
fn helper() -> Result<PathBuf, Error> {
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        let beside = dir.join(HELPER_NAME);
        if beside.is_file() {
            return Ok(beside);
        }
    }
    match option_env!("LAPSEWERK_ENCODER_BUILT") {
        Some(p) if Path::new(p).is_file() => Ok(PathBuf::from(p)),
        _ => Err(Error::Helper(format!("{HELPER_NAME} not found (macOS only)"))),
    }
}

/// A movie being written, as [`abort_all`] sees it: its helper and its partial file.
struct Running {
    child: Mutex<Option<Child>>,
    partial: PathBuf,
}

/// Every movie being written in this process.
static RUNNING: Mutex<Vec<Arc<Running>>> = Mutex::new(Vec::new());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic elsewhere must not keep a partial movie from being cleaned up.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Stops every movie still being written and deletes its partial file; returns how many there were.
///
/// For an interrupted process (Ctrl-C, `kill`): `Drop` does not run when the process is ended, so the
/// hidden partial file (`.NAME.PID.partial.mov`) would stay behind, and a helper sent no signal of its
/// own would finish the movie once its input closes. Call it from the signal handler, then exit.
/// Movies already moved into place are kept.
pub fn abort_all() -> usize {
    let running: Vec<Arc<Running>> = std::mem::take(&mut *lock(&RUNNING));
    for r in &running {
        if let Some(mut child) = lock(&r.child).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&r.partial);
    }
    running.len()
}

/// The folder and file name of `target`, when a movie may be written there.
fn check_target(target: &Path, overwrite: bool) -> Result<(PathBuf, String), Error> {
    let target_err = |reason: String| Error::Target {
        path: target.to_path_buf(),
        reason,
    };
    if !overwrite && target.symlink_metadata().is_ok() {
        return Err(target_err("exists (use --overwrite to replace it)".into()));
    }
    if target.is_dir() {
        return Err(target_err("is a folder".into()));
    }
    let name = target
        .file_name()
        .ok_or_else(|| target_err("no file name".into()))?
        .to_string_lossy()
        .into_owned();
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    if !dir.is_dir() {
        return Err(target_err("folder does not exist".into()));
    }
    Ok((dir, name))
}

/// Checks everything [`Encoder::create`] would, without starting anything: the settings, the target
/// and its folder, the helper. Call it before work that takes long, so a wrong size or an existing
/// file is reported at once.
///
/// # Errors
/// As [`Encoder::create`].
pub fn preflight(target: &Path, settings: VideoSettings, overwrite: bool) -> Result<(), Error> {
    settings.check()?;
    check_target(target, overwrite)?;
    helper().map(|_| ())
}

/// Hidden partial movies of `target` left by earlier renders (`.NAME.PID.partial.mov`): a render
/// killed outright (out of memory, `kill -9`) leaves its file. A render to the same target that is
/// still running has one too, so they are only reported, never deleted here.
#[must_use]
pub fn stale_partials(target: &Path) -> Vec<PathBuf> {
    let Some(name) = target.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return Vec::new();
    };
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let prefix = format!(".{name}.");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(".partial.mov"))
                .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

/// Moves the finished movie `partial` to `target` without replacing anything: a file that appeared at
/// `target` during the render is kept, and the movie goes to a free name beside it, returned in the
/// error. Uses a hard link, which fails if the name exists; where the volume has none (exFAT, some
/// network shares), checks just before renaming.
fn place_without_replacing(partial: &Path, target: &Path) -> Result<PathBuf, Error> {
    let taken = |kept: PathBuf| Error::Target {
        path: target.to_path_buf(),
        reason: format!(
            "appeared while the movie was rendered and was kept; the new movie is {}",
            kept.display()
        ),
    };
    let io = |e: std::io::Error| Error::Target {
        path: target.to_path_buf(),
        reason: e.to_string(),
    };
    let link = |to: &Path| -> Result<bool, std::io::Error> {
        match std::fs::hard_link(partial, to) {
            Ok(()) => {
                let _ = std::fs::remove_file(partial);
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    };
    match link(target) {
        Ok(true) => return Ok(target.to_path_buf()),
        Ok(false) => {}
        // No hard links here: check, then rename (a file appearing in between is replaced).
        Err(_) => {
            if target.symlink_metadata().is_err() {
                std::fs::rename(partial, target).map_err(io)?;
                return Ok(target.to_path_buf());
            }
        }
    }
    let stem = target
        .file_stem()
        .map_or_else(|| "movie".into(), |s| s.to_string_lossy().into_owned());
    let dir = target.parent().unwrap_or_else(|| Path::new(""));
    for k in 2..1000 {
        let other = dir.join(format!("{stem} ({k}).mov"));
        if other.symlink_metadata().is_ok() {
            continue;
        }
        if link(&other).unwrap_or(false) || std::fs::rename(partial, &other).is_ok() {
            return Err(taken(other));
        }
    }
    Err(taken(partial.to_path_buf()))
}

/// Most bytes of a `moov` box read to find the content light level in (a movie's index: a few
/// megabytes for hours of video).
const MOOV_LIMIT: u64 = 256 << 20;

/// Writes `level` (CTA-861.3: MaxCLL, MaxFALL) into the `clli` box of the QuickTime / MP4 movie at
/// `path`, in place. `Ok(false)` when the movie has none.
///
/// # Errors
/// When the file cannot be read or written.
pub fn patch_content_light_level(path: &Path, level: [u8; 4]) -> std::io::Result<bool> {
    use std::io::{Seek as _, SeekFrom};
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let len = f.metadata()?.len();
    let mut at = 0u64;
    // The top-level boxes, to the `moov` (at most a few thousand in any movie).
    for _ in 0..100_000 {
        if at.saturating_add(8) > len {
            return Ok(false);
        }
        f.seek(SeekFrom::Start(at))?;
        let mut head = [0u8; 8];
        f.read_exact(&mut head)?;
        let [s0, s1, s2, s3, k0, k1, k2, k3] = head;
        let mut size = u64::from(u32::from_be_bytes([s0, s1, s2, s3]));
        let mut header = 8u64;
        if size == 1 {
            let mut big = [0u8; 8];
            f.read_exact(&mut big)?;
            size = u64::from_be_bytes(big);
            header = 16;
        } else if size == 0 {
            size = len - at;
        }
        if size < header || at.saturating_add(size) > len {
            return Ok(false);
        }
        if [k0, k1, k2, k3] == *b"moov" {
            let body = size - header;
            if body > MOOV_LIMIT {
                return Ok(false);
            }
            let mut moov = vec![0u8; usize::try_from(body).unwrap_or(0)];
            f.read_exact(&mut moov)?;
            // A `clli` box: size 12, the type, MaxCLL, MaxFALL.
            let mut found = false;
            let mut i = 0;
            while let Some(w) = moov.get(i..i + 8) {
                if w == [0, 0, 0, 12, b'c', b'l', b'l', b'i'] {
                    f.seek(SeekFrom::Start(at + header + i as u64 + 8))?;
                    f.write_all(&level)?;
                    found = true;
                    i += 12;
                } else {
                    i += 1;
                }
            }
            if found {
                f.sync_all()?;
            }
            return Ok(found);
        }
        at += size;
    }
    Ok(false)
}

/// The boxes inside `buf[from..to]`: (type, start of the body, end), in order.
fn boxes(buf: &[u8], from: usize, to: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut at = from;
    while let Some(head) = buf.get(at..at + 8) {
        if at + 8 > to {
            break;
        }
        let [s0, s1, s2, s3, k0, k1, k2, k3] = <[u8; 8]>::try_from(head).unwrap_or_default();
        let mut size = u32::from_be_bytes([s0, s1, s2, s3]) as usize;
        let mut header = 8;
        if size == 1 {
            let Some(big) = buf.get(at + 8..at + 16).and_then(|b| <[u8; 8]>::try_from(b).ok()) else {
                break;
            };
            size = usize::try_from(u64::from_be_bytes(big)).unwrap_or(usize::MAX);
            header = 16;
        } else if size == 0 {
            size = to - at;
        }
        if size < header || at.saturating_add(size) > to {
            break;
        }
        out.push(([k0, k1, k2, k3], at + header, at + size));
        at += size;
    }
    out
}

/// The first box of type `kind` inside `buf[from..to]`: (start of the body, end).
fn find_box(buf: &[u8], from: usize, to: usize, kind: &[u8; 4]) -> Option<(usize, usize)> {
    boxes(buf, from, to)
        .into_iter()
        .find(|(k, _, _)| k == kind)
        .map(|(_, a, b)| (a, b))
}

fn be32(buf: &[u8], at: usize) -> Option<u64> {
    buf.get(at..at + 4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map(|b| u64::from(u32::from_be_bytes(b)))
}

/// Gives the timecode track (`tmcd`) of the movie at `path` the length of `frames` frames of
/// `frame_units` (in its media's time scale) each: the helper writes it before the frames, as one
/// sample one frame long, and some readers (Avid, MediaInfo, QuickTime 7) show no timecode past
/// it or call the tracks' lengths mismatched. Only a track of that one-frame shape is changed;
/// `Ok(false)` when there is none.
///
/// # Errors
/// When the file cannot be read or written.
pub fn patch_timecode_length(path: &Path, frames: u64, frame_units: u64) -> std::io::Result<bool> {
    use std::io::{Seek as _, SeekFrom};
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let len = f.metadata()?.len();
    // The top-level boxes, to the `moov` (as `patch_content_light_level`).
    let mut at = 0u64;
    let mut moov = None;
    for _ in 0..100_000 {
        if at.saturating_add(8) > len {
            break;
        }
        f.seek(SeekFrom::Start(at))?;
        let mut head = [0u8; 8];
        f.read_exact(&mut head)?;
        let [s0, s1, s2, s3, k0, k1, k2, k3] = head;
        let mut size = u64::from(u32::from_be_bytes([s0, s1, s2, s3]));
        let mut header = 8u64;
        if size == 1 {
            let mut big = [0u8; 8];
            f.read_exact(&mut big)?;
            size = u64::from_be_bytes(big);
            header = 16;
        } else if size == 0 {
            size = len - at;
        }
        if size < header || at.saturating_add(size) > len {
            break;
        }
        if [k0, k1, k2, k3] == *b"moov" {
            moov = Some((at + header, size - header));
            break;
        }
        at += size;
    }
    let Some((start, body)) = moov.filter(|&(_, b)| b <= MOOV_LIMIT) else {
        return Ok(false);
    };
    let mut buf = vec![0u8; usize::try_from(body).unwrap_or(0)];
    f.seek(SeekFrom::Start(start))?;
    f.read_exact(&mut buf)?;
    let movie_scale = find_box(&buf, 0, buf.len(), b"mvhd").and_then(|(a, _)| {
        let v1 = buf.get(a) == Some(&1);
        be32(&buf, a + if v1 { 20 } else { 12 })
    });
    let Some(movie_scale) = movie_scale.filter(|&s| s > 0) else {
        return Ok(false);
    };
    // (offset in the moov, value, 8 bytes wide)
    let mut edits: Vec<(usize, u64, bool)> = Vec::new();
    for (kind, a, b) in boxes(&buf, 0, buf.len()) {
        if kind != *b"trak" {
            continue;
        }
        let Some((ma, mb)) = find_box(&buf, a, b, b"mdia") else {
            continue;
        };
        let is_tmcd = find_box(&buf, ma, mb, b"hdlr")
            .is_some_and(|(h, _)| buf.get(h + 8..h + 12) == Some(b"tmcd".as_slice()));
        if !is_tmcd {
            continue;
        }
        let Some((ha, _)) = find_box(&buf, ma, mb, b"mdhd") else {
            continue;
        };
        let v1 = buf.get(ha) == Some(&1);
        let Some(media_scale) = be32(&buf, ha + if v1 { 20 } else { 12 }).filter(|&s| s > 0) else {
            continue;
        };
        let media = frames.saturating_mul(frame_units);
        let movie = media.saturating_mul(movie_scale) / media_scale;
        // The one sample, one frame long: `stts` with one entry (1, frame_units).
        let stts = find_box(&buf, ma, mb, b"minf")
            .and_then(|(ia, ib)| find_box(&buf, ia, ib, b"stbl"))
            .and_then(|(sa, sb)| find_box(&buf, sa, sb, b"stts"));
        let Some((ta, _)) = stts else { continue };
        if be32(&buf, ta + 4) != Some(1) || be32(&buf, ta + 8) != Some(1) {
            continue;
        }
        if media > u64::from(u32::MAX) {
            continue;
        }
        edits.push((ta + 12, media, false));
        edits.push((ha + if v1 { 24 } else { 16 }, media, v1));
        if let Some((ka, _)) = find_box(&buf, a, b, b"tkhd") {
            let v1 = buf.get(ka) == Some(&1);
            edits.push((ka + if v1 { 28 } else { 20 }, movie, v1));
        }
        if let Some((ea, _)) =
            find_box(&buf, a, b, b"edts").and_then(|(da, db)| find_box(&buf, da, db, b"elst"))
        {
            let v1 = buf.get(ea) == Some(&1);
            if be32(&buf, ea + 4) == Some(1) {
                edits.push((ea + 8, movie, v1));
            }
        }
    }
    if edits.is_empty() {
        return Ok(false);
    }
    for (off, value, wide) in edits {
        f.seek(SeekFrom::Start(start + off as u64))?;
        if wide {
            f.write_all(&value.to_be_bytes())?;
        } else {
            f.write_all(&u32::try_from(value).unwrap_or(u32::MAX).to_be_bytes())?;
        }
    }
    f.sync_all()?;
    Ok(true)
}

/// A movie being written: [`Encoder::push`] each frame in order, then [`Encoder::finish`].
/// Dropped unfinished, it stops the encoder and deletes the partial file.
pub struct Encoder {
    running: Arc<Running>,
    stdin: Option<ChildStdin>,
    partial: PathBuf,
    target: PathBuf,
    /// Replace an existing target (else never, not even one that appears during the render).
    overwrite: bool,
    settings: VideoSettings,
    buffer: Vec<u8>,
    frames: u64,
    /// Re-encoding of the values for the transfer, when it is not sRGB.
    table: Option<Conversion>,
    /// PQ: the light the movie reaches, measured as it is written (HDR10's MaxCLL / MaxFALL).
    meter: Option<lapsewerk_core::hdr::LightMeter>,
}

impl Encoder {
    /// Starts writing a movie to `target` (`.mov`). An existing file is replaced only when
    /// `overwrite` is set, and only once the new movie is complete.
    ///
    /// # Errors
    /// On invalid settings, an existing target, or when the helper cannot start.
    pub fn create(target: &Path, settings: VideoSettings, overwrite: bool) -> Result<Self, Error> {
        Self::create_with(helper, target, settings, overwrite)
    }

    /// [`Self::create`] with the helper found by `helper` (asked only once the target is checked).
    fn create_with(
        helper: impl FnOnce() -> Result<PathBuf, Error>,
        target: &Path,
        settings: VideoSettings,
        overwrite: bool,
    ) -> Result<Self, Error> {
        settings.check()?;
        let (dir, name) = check_target(target, overwrite)?;
        // Same folder as the target, so the final rename is atomic; hidden; unique per process.
        let partial = dir.join(format!(".{name}.{}.partial.mov", std::process::id()));
        let _ = std::fs::remove_file(&partial);

        let mut cmd = Command::new(helper()?);
        cmd.arg("--out")
            .arg(&partial)
            .args(["--width", &settings.width.to_string()])
            .args(["--height", &settings.height.to_string()])
            .args(["--fps", &format!("{}/{}", settings.fps_num, settings.fps_den)])
            .args(["--codec", settings.codec.arg()])
            .args(["--transfer", settings.transfer.arg()]);
        if let Some(b) = settings.bitrate {
            cmd.args(["--bitrate", &b.to_string()]);
        }
        // HDR10: static metadata for PQ (what TVs and YouTube read to map the highlights): in the
        // HEVC stream, and for every codec in the movie's description.
        if let Transfer::Pq { peak } = settings.transfer {
            use lapsewerk_core::hdr::{Hdr10Metadata, hex};
            let m = Hdr10Metadata::for_peak(peak);
            cmd.args(["--mastering-display", &hex(&m.mastering_display)])
                .args(["--content-light-level", &hex(&m.content_light_level)]);
        }
        if let Some(tc) = settings.timecode {
            cmd.args(["--timecode-frame", &tc.to_string()]);
            if settings.drop_frame {
                cmd.args(["--drop-frame", "1"]);
            }
        }
        let mut child = cmd
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Helper(e.to_string()))?;
        let stdin = child.stdin.take();
        let running = Arc::new(Running {
            child: Mutex::new(Some(child)),
            partial: partial.clone(),
        });
        lock(&RUNNING).push(running.clone());
        Ok(Self {
            running,
            stdin,
            partial,
            target: target.to_path_buf(),
            overwrite,
            settings,
            buffer: Vec::new(),
            frames: 0,
            table: settings.transfer.conversion(),
            meter: match settings.transfer {
                Transfer::Pq { peak } => Some(lapsewerk_core::hdr::LightMeter::new(peak)),
                _ => None,
            },
        })
    }

    /// Settings the movie is written with.
    #[must_use]
    pub fn settings(&self) -> &VideoSettings {
        &self.settings
    }

    /// Frames written so far.
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// PQ: the light the frames written so far reach (MaxCLL / MaxFALL).
    #[must_use]
    pub fn light_level(&self) -> Option<lapsewerk_core::hdr::LightLevel> {
        self.meter
            .as_ref()
            .and_then(lapsewerk_core::hdr::LightMeter::level)
    }

    /// Appends one frame: the `width × height` window at (`left`, `top`) of a 16-bit RGB image
    /// (`rgb`, 3 samples a pixel, `src_width` pixels a row).
    ///
    /// # Errors
    /// When the window does not fit the image, or the encoder has stopped.
    pub fn push(&mut self, rgb: &[u16], src_width: usize, left: usize, top: usize) -> Result<(), Error> {
        let (w, h) = (self.settings.width, self.settings.height);
        let src_height = (rgb.len() / 3).checked_div(src_width).unwrap_or(0);
        let fits =
            |start: usize, size: usize, room: usize| start.checked_add(size).is_some_and(|end| end <= room);
        if src_width == 0
            || src_width.checked_mul(src_height).and_then(|p| p.checked_mul(3)) != Some(rgb.len())
            || !fits(left, w, src_width)
            || !fits(top, h, src_height)
        {
            return Err(Error::Frame(format!(
                "a {w}×{h} window at {left},{top} does not fit a {src_width}×{src_height} image"
            )));
        }
        if let Some(m) = self.meter.as_mut()
            && let Some(window) = rgb.get(top * src_width * 3..(top + h) * src_width * 3)
        {
            m.frame(window, src_width * 3, left, w);
        }
        self.buffer.clear();
        self.buffer.reserve(w * h * 8 + 1);
        // Message: a frame follows (the helper's protocol, `encoder.swift`).
        self.buffer.push(b'F');
        for y in top..top + h {
            let start = (y * src_width + left) * 3;
            let Some(row) = rgb.get(start..start + w * 3) else {
                return Err(Error::Frame(format!("row {y} of the window is missing")));
            };
            for px in row.as_chunks::<3>().0 {
                let [r, g, b] = match &self.table {
                    Some(c) => c.apply(*px),
                    None => *px,
                };
                // 16-bit big-endian ARGB, opaque.
                self.buffer.extend_from_slice(&[0xff, 0xff]);
                self.buffer.extend_from_slice(&r.to_be_bytes());
                self.buffer.extend_from_slice(&g.to_be_bytes());
                self.buffer.extend_from_slice(&b.to_be_bytes());
            }
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(Error::Encoder("already finished".into()));
        };
        if stdin.write_all(&self.buffer).is_err() {
            // The helper stopped: its message says why.
            let message = self.stop();
            return Err(Error::Encoder(message));
        }
        self.frames += 1;
        Ok(())
    }

    /// Finishes the movie and moves it into place.
    ///
    /// # Errors
    /// When no frame was written, the encoder failed, or the file cannot be moved into place.
    pub fn finish(mut self) -> Result<PathBuf, Error> {
        // The end mark: only with it does the helper finish the movie. Input that just ends (this
        // process killed) makes it delete the file instead.
        let mut end = vec![b'E'];
        end.extend_from_slice(&self.frames.to_be_bytes());
        if let Some(stdin) = self.stdin.as_mut()
            && stdin.write_all(&end).and_then(|()| stdin.flush()).is_err()
        {
            let message = self.stop();
            return Err(Error::Encoder(message));
        }
        drop(self.stdin.take());
        let Some(mut child) = lock(&self.running.child).take() else {
            return Err(Error::Encoder("already finished (or aborted)".into()));
        };
        let mut message = String::new();
        if let Some(mut err) = child.stderr.take() {
            let _ = err.read_to_string(&mut message);
        }
        // Stays registered until dropped at the end of this call, after the move into place: an abort
        // before that deletes the partial file.
        let status = child.wait().map_err(|e| {
            let _ = std::fs::remove_file(&self.partial);
            Error::Encoder(e.to_string())
        })?;
        if !status.success() {
            let _ = std::fs::remove_file(&self.partial);
            return Err(Error::Encoder(clean(&message, status)));
        }
        // HDR10: the helper wrote MaxCLL as an upper bound (it starts before any frame); the
        // container's content light level now gets the measured values. A movie without the box
        // keeps what the helper wrote.
        if let Some(level) = self.light_level() {
            let _ = patch_content_light_level(&self.partial, level.bytes());
        }
        // The timecode track as long as the movie (the helper writes it before the frames).
        if self.settings.timecode.is_some() {
            let _ = patch_timecode_length(&self.partial, self.frames, u64::from(self.settings.fps_den));
        }
        if !self.overwrite {
            // The finished movie is never deleted here: hours of rendering.
            return place_without_replacing(&self.partial, &self.target);
        }
        std::fs::rename(&self.partial, &self.target).map_err(|e| {
            let _ = std::fs::remove_file(&self.partial);
            Error::Target {
                path: self.target.clone(),
                reason: e.to_string(),
            }
        })?;
        Ok(self.target.clone())
    }

    /// Stops the helper, deletes the partial file, returns what the helper said.
    fn stop(&mut self) -> String {
        drop(self.stdin.take());
        let mut message = String::new();
        let child = lock(&self.running.child).take();
        if let Some(mut child) = child {
            let _ = child.kill();
            if let Some(mut err) = child.stderr.take() {
                let _ = err.read_to_string(&mut message);
            }
            let status = child.wait();
            message = match status {
                Ok(s) => clean(&message, s),
                Err(e) => e.to_string(),
            };
        }
        let _ = std::fs::remove_file(&self.partial);
        self.unregister();
        message
    }

    fn unregister(&self) {
        lock(&RUNNING).retain(|r| !Arc::ptr_eq(r, &self.running));
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let unfinished = lock(&self.running.child).is_some();
        if unfinished {
            self.stop();
        }
        self.unregister();
    }
}

fn clean(message: &str, status: std::process::ExitStatus) -> String {
    let m = message.trim();
    if m.is_empty() {
        format!("stopped ({status})")
    } else {
        m.trim_start_matches("lapsewerk-encode: ").to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_frame_timecodes_keep_to_the_clock() {
        // 29.97: one hour of drop-frame timecode is 107892 frames, as the clock has it.
        assert_eq!(
            parse_timecode_df("01:00:00;00", 30_000, 1001),
            Some((107_892, true))
        );
        assert_eq!(drop_frame_time(107_892, 30), (1, 0, 0, 0));
        // The minute's first frame numbers are skipped, but every tenth minute.
        assert_eq!(drop_frame_time(1799, 30), (0, 0, 59, 29));
        assert_eq!(drop_frame_time(1800, 30), (0, 1, 0, 2));
        assert_eq!(drop_frame_time(17_982, 30), (0, 10, 0, 0));
        assert_eq!(parse_timecode_df("00:01:00;00", 30_000, 1001), None);
        assert_eq!(parse_timecode_df("00:01:00;02", 30_000, 1001), Some((1800, true)));
        // 59.94: four frame numbers.
        assert_eq!(drop_frame_time(3599, 60), (0, 0, 59, 59));
        assert_eq!(drop_frame_time(3600, 60), (0, 1, 0, 4));
        assert_eq!(parse_timecode_df("00:00:01;00", 25, 1), None);
        for c in [0, 1799, 1800, 17_981, 17_982, 107_891, 107_892, 2_000_000] {
            let (h, m, s, f) = drop_frame_time(c, 30);
            let text = format!("{h:02}:{m:02}:{s:02};{f:02}");
            assert_eq!(parse_timecode_df(&text, 30_000, 1001), Some((c, true)), "{text}");
        }
    }

    #[test]
    fn timecodes_are_read_as_frame_counts() {
        assert_eq!(parse_timecode("01:00:00:00", 25, 1), Some(90_000));
        assert_eq!(parse_timecode("00:00:01:05", 30_000, 1001), Some(35));
        assert_eq!(parse_timecode("10:00:00:00", 24, 1), Some(864_000));
        assert_eq!(parse_timecode("10:00:00;00", 30_000, 1001), None);
        assert_eq!(parse_timecode("00:00:00:25", 25, 1), None);
        assert_eq!(parse_timecode("24:00:00:00", 25, 1), None);
        assert_eq!(parse_timecode("1:2:3", 25, 1), None);
        assert_eq!(parse_timecode("aa:00:00:00", 25, 1), None);
    }

    #[test]
    fn the_content_light_level_is_written_into_the_movie() {
        let dir = std::env::temp_dir().join(format!("lw-clli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.mov");
        // ftyp, mdat (holding bytes that look like a clli box: left alone), moov with a clli box
        // deep inside (stsd → hvc1 → clli).
        let boxed = |kind: &[u8], body: &[u8]| {
            let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            b.extend_from_slice(kind);
            b.extend_from_slice(body);
            b
        };
        let fake = [0, 0, 0, 12, b'c', b'l', b'l', b'i', 1, 2, 3, 4];
        let clli = boxed(b"clli", &[0x06, 0x58, 0, 0]);
        let moov = boxed(
            b"moov",
            &boxed(b"trak", &boxed(b"hvc1", &[&[9u8; 20][..], &clli].concat())),
        );
        let file = [boxed(b"ftyp", b"qt  "), boxed(b"mdat", &fake), moov].concat();
        std::fs::write(&path, &file).unwrap();
        assert!(patch_content_light_level(&path, [0x01, 0x2c, 0x00, 0x64]).unwrap());
        let out = std::fs::read(&path).unwrap();
        assert_eq!(out.len(), file.len());
        let at = out.windows(4).rposition(|w| w == b"clli").unwrap();
        assert_eq!(out[at + 4..at + 8], [0x01, 0x2c, 0x00, 0x64]);
        // The bytes in the media data are untouched.
        assert_eq!(out[..at - 40], file[..at - 40]);
        // No moov, no box, a broken size: nothing written, no error.
        for bad in [
            boxed(b"ftyp", b"qt  "),
            [0, 0, 0, 3, b'm', b'o', b'o', b'v'].to_vec(),
            Vec::new(),
        ] {
            std::fs::write(&path, &bad).unwrap();
            assert!(!patch_content_light_level(&path, [1, 2, 3, 4]).unwrap());
            assert_eq!(std::fs::read(&path).unwrap(), bad);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn frame_rates_in_every_spelling() {
        assert_eq!(parse_fps("24.0"), Some((24, 1)));
        assert_eq!(parse_fps("47.952"), Some((48_000, 1001)));
        assert_eq!(parse_fps("119,88"), Some((120_000, 1001)));
        assert_eq!(parse_fps("12.5"), Some((12_500, 1000)));
        assert_eq!(parse_fps("-3"), None);
        assert_eq!(parse_fps("nan"), None);
    }

    fn settings(codec: Codec) -> VideoSettings {
        VideoSettings {
            width: 64,
            height: 32,
            fps_num: 25,
            fps_den: 1,
            codec,
            bitrate: None,
            transfer: Transfer::Srgb,
            timecode: None,
            drop_frame: false,
        }
    }

    /// Tests that start encoders run one at a time: they share the process-wide list of running
    /// movies, and one of them stops every movie in it (`abort_all`).
    fn serial() -> MutexGuard<'static, ()> {
        static SERIAL: Mutex<()> = Mutex::new(());
        lock(&SERIAL)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lapsewerk-video-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn parses_codecs_and_rates() {
        assert_eq!(Codec::parse("ProRes-422-HQ"), Some(Codec::ProRes422Hq));
        assert_eq!(Codec::parse("hevc"), Some(Codec::Hevc));
        assert_eq!(Codec::parse("h264"), None);
        assert_eq!(parse_fps("25"), Some((25, 1)));
        assert_eq!(parse_fps("29.97"), Some((30_000, 1001)));
        assert_eq!(parse_fps("30000/1001"), Some((30_000, 1001)));
        assert_eq!(parse_fps("0"), None);
        assert_eq!(parse_fps("25/0"), None);
        assert_eq!(parse_fps("x"), None);
    }

    #[test]
    fn rec709_re_encodes_for_a_gamma_2_4_display() {
        assert!(Transfer::Srgb.table().is_none());
        let t = Transfer::Rec709.table().unwrap();
        assert_eq!((t[0], t[65535]), (0, 65535));
        // sRGB 50 % is 21.4 % of the light; a 2.4 display needs 52.6 % for it.
        let mid = f64::from(t[32768]) / 65535.0;
        assert!((mid - 0.5262).abs() < 0.001, "{mid}");
        assert!(t.windows(2).all(|w| w[0] <= w[1]), "monotone");
    }

    #[test]
    fn hdr_transfers_map_sdr_white_where_the_standards_put_it() {
        use lapsewerk_core::hdr::{encode_frame, pq_encode};
        let white = encode_frame(&[1.0], 8.0)[0];
        let pq = Transfer::Pq { peak: 8.0 }.table().unwrap();
        let hlg = Transfer::Hlg { peak: 8.0 }.table().unwrap();
        assert!((f64::from(pq[usize::from(white)]) / 65535.0 - pq_encode(203.0)).abs() < 1e-3);
        assert!((f64::from(hlg[usize::from(white)]) / 65535.0 - 0.75).abs() < 0.006);
        assert!(Transfer::Pq { peak: 8.0 }.is_hdr() && !Transfer::Rec709.is_hdr());
        assert_eq!(Transfer::Hlg { peak: 2.0 }.arg(), "hlg");
    }

    #[test]
    fn rejects_odd_sizes_and_existing_targets() {
        let mut s = settings(Codec::Hevc);
        s.width = 63;
        assert!(matches!(
            Encoder::create(&scratch("odd.mov"), s, false),
            Err(Error::Settings(_))
        ));
        let existing = scratch("existing.mov");
        std::fs::write(&existing, b"keep").unwrap();
        assert!(matches!(
            Encoder::create(&existing, settings(Codec::Hevc), false),
            Err(Error::Target { .. })
        ));
        assert_eq!(std::fs::read(&existing).unwrap(), b"keep");
    }

    /// A stand-in for the helper speaking its protocol (`encoder.swift`): frames are written to the
    /// `--out` file as they arrive, the end mark finishes it, input ending without one deletes it.
    pub(crate) const FAKE_HELPER: &str = r#"#!/usr/bin/python3
import os, sys
a = dict(zip(sys.argv[1::2], sys.argv[2::2]))
out, size = a["--out"], int(a["--width"]) * int(a["--height"]) * 8
src, n = sys.stdin.buffer, 0
def abandon(why):
    try: os.remove(out)
    except OSError: pass
    sys.stderr.write("lapsewerk-encode: " + why + "\n"); sys.exit(1)
with open(out, "wb") as f:
    while True:
        tag = src.read(1)
        if tag == b"F":
            frame = src.read(size)
            if len(frame) < size: abandon("input ended inside frame %d" % n)
            f.write(frame); f.flush(); n += 1
        elif tag == b"E":
            sent = int.from_bytes(src.read(8), "big")
            if sent != n: abandon("end mark says %d frames, %d arrived" % (sent, n))
            break
        else:
            abandon("input ended without the end mark (Lapsewerk stopped)")
if n == 0: abandon("no frames")
"#;

    #[cfg(unix)]
    fn fake_helper() -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = scratch("fake-encode.py");
        std::fs::write(&path, FAKE_HELPER).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Waits for the stand-in helper to have created `path`.
    #[cfg(unix)]
    fn appears(path: &Path) {
        for _ in 0..200 {
            if path.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("{} never appeared", path.display());
    }

    /// One test for everything that touches the process-wide list, so parallel tests cannot see each
    /// other's movies in it.
    #[cfg(unix)]
    #[test]
    fn partial_movies_are_cleaned_up_however_writing_ends() {
        let _serial = serial();
        let helper = fake_helper();
        let rgb = vec![7u16; 64 * 32 * 3];
        let running = || lock(&RUNNING).len();

        // Finished: moved into place, nothing left registered.
        let target = scratch("finished.mov");
        let _ = std::fs::remove_file(&target);
        let mut e =
            Encoder::create_with(|| Ok(helper.clone()), &target, settings(Codec::Hevc), false).unwrap();
        e.push(&rgb, 64, 0, 0).unwrap();
        let partial = e.partial.clone();
        appears(&partial);
        assert_eq!(running(), 1);
        assert_eq!(e.finish().unwrap(), target);
        assert_eq!(std::fs::metadata(&target).unwrap().len(), 64 * 32 * 8);
        assert!(!partial.exists());
        assert_eq!(running(), 0);

        // The input ends without the end mark (what a killed Lapsewerk leaves the helper with): the
        // helper itself deletes the movie, even with nobody left to clean up.
        let orphan = scratch("orphan.mov");
        let mut e =
            Encoder::create_with(|| Ok(helper.clone()), &orphan, settings(Codec::Hevc), false).unwrap();
        e.push(&rgb, 64, 0, 0).unwrap();
        let partial = e.partial.clone();
        appears(&partial);
        let mut child = lock(&e.running.child).take().unwrap();
        drop(e.stdin.take());
        assert!(!child.wait().unwrap().success());
        assert!(!partial.exists(), "the helper left its movie behind");
        drop(e);
        assert_eq!(running(), 0);

        // Dropped unfinished: partial deleted, the earlier movie kept.
        let other = scratch("dropped-fake.mov");
        let e = Encoder::create_with(|| Ok(helper.clone()), &other, settings(Codec::Hevc), false).unwrap();
        let partial = e.partial.clone();
        drop(e);
        assert!(!partial.exists() && !other.exists());
        assert_eq!(running(), 0);

        // Interrupted (what the Ctrl-C handler does): helpers stopped, partial files deleted, movies
        // already in place kept; the encoders left over fail instead of writing anything.
        let a = scratch("interrupted-a.mov");
        let b = scratch("interrupted-b.mov");
        let mut ea = Encoder::create_with(|| Ok(helper.clone()), &a, settings(Codec::Hevc), false).unwrap();
        let eb = Encoder::create_with(|| Ok(helper.clone()), &b, settings(Codec::Hevc), false).unwrap();
        ea.push(&rgb, 64, 0, 0).unwrap();
        let (pa, pb) = (ea.partial.clone(), eb.partial.clone());
        appears(&pa);
        appears(&pb);
        assert_eq!(running(), 2);
        assert_eq!(abort_all(), 2);
        assert_eq!(running(), 0);
        assert!(!pa.exists() && !pb.exists());
        assert!(ea.push(&rgb, 64, 0, 0).is_err() || ea.push(&rgb, 64, 0, 0).is_err());
        assert!(matches!(eb.finish(), Err(Error::Encoder(_))));
        assert!(!a.exists() && !b.exists());
        assert!(target.exists());
        assert_eq!(abort_all(), 0);

        // A file appears at the target while the movie renders: kept, the movie goes beside it.
        let appeared = scratch("appeared.mov");
        let beside = scratch("appeared (2).mov");
        let _ = (std::fs::remove_file(&appeared), std::fs::remove_file(&beside));
        let mut e =
            Encoder::create_with(|| Ok(helper.clone()), &appeared, settings(Codec::Hevc), false).unwrap();
        e.push(&rgb, 64, 0, 0).unwrap();
        appears(&e.partial);
        assert_eq!(stale_partials(&appeared), vec![e.partial.clone()]);
        std::fs::write(&appeared, b"someone else's").unwrap();
        let err = e.finish().unwrap_err().to_string();
        assert!(err.contains("appeared (2).mov"), "{err}");
        assert_eq!(std::fs::read(&appeared).unwrap(), b"someone else's");
        assert_eq!(std::fs::metadata(&beside).unwrap().len(), 64 * 32 * 8);
        assert!(stale_partials(&appeared).is_empty());
        assert_eq!(running(), 0);

        // With overwrite, the target is replaced once the movie is complete.
        let mut e =
            Encoder::create_with(|| Ok(helper.clone()), &appeared, settings(Codec::Hevc), true).unwrap();
        e.push(&rgb, 64, 0, 0).unwrap();
        e.finish().unwrap();
        assert_eq!(std::fs::metadata(&appeared).unwrap().len(), 64 * 32 * 8);
    }

    #[test]
    fn a_window_far_outside_the_image_is_refused() {
        let mut e = Encoder {
            running: Arc::new(Running {
                child: Mutex::new(None),
                partial: scratch("never.mov"),
            }),
            stdin: None,
            partial: scratch("never.mov"),
            target: scratch("never-target.mov"),
            overwrite: false,
            settings: settings(Codec::Hevc),
            buffer: Vec::new(),
            frames: 0,
            table: None,
            meter: None,
        };
        let rgb = vec![0u16; 80 * 40 * 3];
        assert!(matches!(
            e.push(&rgb, 80, usize::MAX - 10, 0),
            Err(Error::Frame(_))
        ));
        assert!(matches!(e.push(&rgb, 80, 0, usize::MAX), Err(Error::Frame(_))));
        assert!(matches!(e.push(&rgb, usize::MAX, 0, 0), Err(Error::Frame(_))));
        assert_eq!(e.frames(), 0);
    }

    #[test]
    fn preflight_reports_before_any_work() {
        let existing = scratch("preflight.mov");
        std::fs::write(&existing, b"x").unwrap();
        assert!(matches!(
            preflight(&existing, settings(Codec::Hevc), false),
            Err(Error::Target { .. })
        ));
        let mut odd = settings(Codec::Hevc);
        odd.width = 3841;
        assert!(matches!(
            preflight(&scratch("new.mov"), odd, true),
            Err(Error::Settings(_))
        ));
        assert!(matches!(
            preflight(Path::new("/no/such/folder/x.mov"), settings(Codec::Hevc), false),
            Err(Error::Target { .. })
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn writes_a_movie_for_every_codec() {
        let _serial = serial();
        let (sw, sh) = (80, 40);
        let rgb: Vec<u16> = (0..sw * sh * 3).map(|i| (i * 37 % 65_536) as u16).collect();
        for codec in [
            Codec::ProRes422Hq,
            Codec::ProRes422,
            Codec::ProRes4444,
            Codec::Hevc,
        ] {
            let target = scratch(&format!("{}.mov", codec.arg()));
            let _ = std::fs::remove_file(&target);
            let mut e = Encoder::create(&target, settings(codec), false).unwrap();
            for _ in 0..5 {
                e.push(&rgb, sw, 8, 4).unwrap();
            }
            assert!(matches!(e.push(&rgb, sw, 20, 0), Err(Error::Frame(_))));
            assert_eq!(e.frames(), 5);
            let written = e.finish().unwrap();
            assert!(std::fs::metadata(&written).unwrap().len() > 1000, "{codec:?}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_helper_deletes_a_movie_whose_input_ends_without_the_end_mark() {
        let _serial = serial();
        // What a killed Lapsewerk leaves the helper with: frames, then the input simply ends.
        let target = scratch("orphan-real.mov");
        let _ = std::fs::remove_file(&target);
        let rgb = vec![30_000u16; 64 * 32 * 3];
        let mut e = Encoder::create(&target, settings(Codec::ProRes422Hq), false).unwrap();
        e.push(&rgb, 64, 0, 0).unwrap();
        e.push(&rgb, 64, 0, 0).unwrap();
        let partial = e.partial.clone();
        let mut child = lock(&e.running.child).take().unwrap();
        drop(e.stdin.take());
        assert!(!child.wait().unwrap().success());
        assert!(!partial.exists(), "the helper left its movie behind");
        assert!(!target.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn writes_hdr_movies() {
        let _serial = serial();
        let frame = lapsewerk_core::hdr::encode_frame(&vec![0.5f32; 64 * 32 * 3], 8.0);
        for (codec, transfer) in [
            (Codec::Hevc, Transfer::Pq { peak: 8.0 }),
            (Codec::ProRes422Hq, Transfer::Pq { peak: 8.0 }),
            (Codec::Hevc, Transfer::Hlg { peak: 4.0 }),
        ] {
            let target = scratch(&format!("hdr-{}-{}.mov", codec.arg(), transfer.arg()));
            let _ = std::fs::remove_file(&target);
            let mut s = settings(codec);
            s.transfer = transfer;
            let mut e = Encoder::create(&target, s, false).unwrap();
            for _ in 0..3 {
                e.push(&frame, 64, 0, 0).unwrap();
            }
            let written = e.finish().unwrap();
            assert!(
                std::fs::metadata(&written).unwrap().len() > 1000,
                "{codec:?} {transfer:?}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_unfinished_movie_leaves_nothing_behind() {
        let _serial = serial();
        let target = scratch("dropped.mov");
        let _ = std::fs::remove_file(&target);
        let rgb = vec![0u16; 64 * 32 * 3];
        let mut e = Encoder::create(&target, settings(Codec::ProRes422Hq), false).unwrap();
        e.push(&rgb, 64, 0, 0).unwrap();
        let partial = e.partial.clone();
        drop(e);
        assert!(!target.exists());
        assert!(!partial.exists());
        // No frames: an error, and still nothing.
        let e = Encoder::create(&target, settings(Codec::ProRes422Hq), false).unwrap();
        assert!(matches!(e.finish(), Err(Error::Encoder(m)) if m.contains("no frames")));
        assert!(!target.exists());
    }
}
