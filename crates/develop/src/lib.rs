//! Lapsewerk's bridge to LightCraft.
//!
//! [`Sequence::open`] imports the frames into an in-memory LightCraft session (nothing is written:
//! no library, no sidecars), which also reads each frame's XMP sidecar, so keyframes edited in
//! LightCraft come in with their settings. [`Sequence::keyframe`] turns a frame's settings into a
//! [`lapsewerk_core::keyframes::Keyframe`]; [`Sequence::frame_settings`] builds a frame's settings
//! from interpolated values plus an exposure offset; [`Sequence::render_job`] makes a detached
//! LightCraft render job for any thread. [`Sequence::write_keyframes`] writes new keyframes: XMP
//! sidecars beside frames that have none, to be edited in LightCraft.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lapsewerk_core::exposure::Exposure;
use lapsewerk_core::keyframes::{self, Curves, Interpolation, Keyframe};
use lapsewerk_core::sequence::Frame;
pub mod calibrate;
pub mod masks;
pub mod project;

use lightcraft_engine::Session;
use lightcraft_engine::catalog::{Photo, PhotoId, Source};
use lightcraft_engine::develop::WbMode;
use lightcraft_engine::files::SequenceLook;
use lightcraft_engine::pipeline::{OutputDepth, Quality};
use serde_json::json;

pub use lightcraft_engine::RenderJob;
pub use lightcraft_engine::develop::{DevelopSettings, Mask, controls};
pub use lightcraft_engine::pipeline::{DeepSamples, OutputSpace, Rendered};

/// Luminance weights of linear Rec. 2020, the space [`Sequence::render_job`] renders measuring jobs in.
pub const REC2020_LUMINANCE: [f32; 3] = [0.2627, 0.6780, 0.0593];

/// The attribute that marks an automatic keyframe in its sidecar ([`Sequence::automatic_mark`]).
const AUTOMATIC_ATTR: &str = "lapsewerk:Automatic";

/// The namespace of [`AUTOMATIC_ATTR`].
const AUTOMATIC_NS: &str = "https://github.com/t-kramarczyk/lapsewerk/ns/1.0/";

/// Where Lapsewerk's automatic mark goes in sidecar `text`: the start tag of the first
/// `rdf:Description` outside a comment (its byte range), and the mark's value there (the range of
/// what is between its quotes) with whether the namespace is declared in that tag. `None` when there
/// is no such tag, or the mark's name or namespace appear anywhere else (written another way, in a
/// comment, in another's value): then nothing is read or written.
/// The start tag of the first `rdf:Description` of sidecar `text` outside a comment (its byte
/// range), with the ranges of what is inside quotes in it.
#[allow(clippy::type_complexity)]
fn description_tag(text: &str) -> Option<(std::ops::Range<usize>, Vec<std::ops::Range<usize>>)> {
    let mut comments = Vec::new();
    let mut from = 0;
    while let Some(a) = text.get(from..).and_then(|r| r.find("<!--")).map(|a| a + from) {
        let b = text
            .get(a..)
            .and_then(|r| r.find("-->"))
            .map_or(text.len(), |b| a + b + 3);
        comments.push(a..b);
        from = b;
    }
    let commented = |i: usize| comments.iter().any(|c| c.contains(&i));
    let mut start = None;
    let mut from = 0;
    while let Some(a) = text
        .get(from..)
        .and_then(|r| r.find("<rdf:Description"))
        .map(|a| a + from)
    {
        let next = text
            .get(a + "<rdf:Description".len()..)
            .and_then(|r| r.chars().next());
        if !commented(a) && next.is_some_and(|c| c.is_whitespace() || c == '>' || c == '/') {
            start = Some(a);
            break;
        }
        from = a + 1;
    }
    let start = start?;
    let mut quote: Option<(char, usize)> = None;
    let mut quoted = Vec::new();
    for (i, c) in text.get(start..)?.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some((c, start + i + 1)),
            (Some((q, from)), c) if c == q => {
                quoted.push(from..start + i);
                quote = None;
            }
            (None, '>') => return Some((start..start + i + 1, quoted)),
            _ => {}
        }
    }
    None
}

/// The colour label of sidecar `text` (`xmp:Label`, as an attribute or an element), when it has one.
fn label_in(text: &str) -> Option<String> {
    let value = if let Some(at) = text.find("xmp:Label=\"") {
        let rest = text.get(at + "xmp:Label=\"".len()..)?;
        rest.get(..rest.find('"')?)?
    } else {
        let at = text.find("<xmp:Label>")? + "<xmp:Label>".len();
        let rest = text.get(at..)?;
        rest.get(..rest.find("</xmp:Label>")?)?
    };
    let value = value
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    (!value.trim().is_empty() && value.len() <= 256).then_some(value)
}

/// Writes `text` over sidecar `path` atomically: a new file beside it with its permissions,
/// synced, then put in its place.
fn write_sidecar_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let perms = std::fs::metadata(path)?.permissions();
    let tmp = path.with_extension(format!("xmp.{}.tmp", std::process::id()));
    let written = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.set_permissions(perms)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[allow(clippy::type_complexity)]
fn mark_in(text: &str) -> Option<(std::ops::Range<usize>, Option<(std::ops::Range<usize>, bool)>)> {
    let (tag, quoted) = description_tag(text)?;
    let tag_text = text.get(tag.clone())?;
    let pattern = format!("{AUTOMATIC_ATTR}=\"");
    // An attribute of the tag itself: after a space, not inside another attribute's quotes.
    let attr = tag_text.match_indices(&pattern).find(|(i, _)| {
        tag_text
            .get(..*i)
            .and_then(|b| b.chars().last())
            .is_some_and(char::is_whitespace)
            && !quoted.iter().any(|q| q.contains(&(tag.start + i)))
    });
    let declared = tag_text.contains("xmlns:lapsewerk=");
    // Anywhere else: not ours to touch.
    let names = text.matches(AUTOMATIC_ATTR).count();
    let spaces = text.matches("xmlns:lapsewerk").count();
    if names != usize::from(attr.is_some()) || spaces != usize::from(declared) {
        return None;
    }
    let found = match attr {
        Some((i, _)) => {
            let v = tag.start + i + pattern.len();
            let len = text.get(v..)?.find('"')?;
            Some((v..v + len, declared))
        }
        None => None,
    };
    Some((tag, found))
}

/// `text` without Lapsewerk's automatic mark ([`AUTOMATIC_ATTR`] and its namespace), as it was
/// before the mark was set.
fn without_automatic_mark(text: &str) -> String {
    let mut out = text.to_owned();
    for name in ["xmlns:lapsewerk", AUTOMATIC_ATTR] {
        let pattern = format!(" {name}=\"");
        while let Some(at) = out.find(&pattern) {
            let start = at + pattern.len();
            let Some(len) = out.get(start..).and_then(|r| r.find('"')) else {
                break;
            };
            out.replace_range(at..start + len + 1, "");
        }
    }
    out
}

/// Why a sequence could not be opened or rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No frames given.
    Empty,
    /// A file could not be found or resolved.
    Path { path: PathBuf, reason: String },
    /// LightCraft refused the import.
    Import(String),
    /// A file was not imported (not a photo LightCraft can read).
    NotImported(PathBuf),
    /// A file has the same contents as an earlier frame (LightCraft imports one of them).
    Duplicate { path: PathBuf, of: PathBuf },
    /// Frame index out of range.
    NoFrame(usize),
    /// LightCraft could not make a render job.
    Render(String),
    /// The keyframes could not be interpolated.
    Keyframes(String),
    /// A keyframe could not be saved (its XMP sidecar).
    Sidecar(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "no frames"),
            Self::Path { path, reason } => write!(f, "{}: {reason}", path.display()),
            Self::Import(e) => write!(f, "import failed: {e}"),
            Self::NotImported(p) => write!(f, "{}: not a photo LightCraft can read", p.display()),
            Self::Duplicate { path, of } => write!(
                f,
                "{}: the same file as {} (a copy?); a sequence holds each frame once",
                path.display(),
                of.display()
            ),
            Self::NoFrame(i) => write!(f, "no frame {i}"),
            Self::Render(e) => write!(f, "render: {e}"),
            Self::Keyframes(e) => write!(f, "keyframes: {e}"),
            Self::Sidecar(e) => write!(f, "keyframe not saved: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Frames tried for a camera look after the preferred one ([`Sequence::use_one_look`]): spread over the
/// sequence, ends included.
const LOOK_CANDIDATES: usize = 8;

/// The look a sequence is developed with ([`Sequence::use_one_look`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneLook {
    /// The camera look fitted to this frame's JPEG.
    Fitted(usize),
    /// None fitted: the raws' own colour matrices, or no frame tried got a camera look (a dark scene).
    Unfitted,
}

/// A sequence of frames open in a LightCraft session.
pub struct Sequence {
    session: Session,
    photos: Vec<PhotoId>,
    frames: Vec<Frame>,
    /// How every frame gets its look ([`Self::use_one_look`]).
    look: SequenceLook,
    /// Raw samples repaired in every frame ([`Self::repair_samples`]).
    defects: Arc<Vec<usize>>,
    /// Files left out because LightCraft could not read them (damaged, cut short).
    skipped: Vec<PathBuf>,
    /// Where they were: (before which frame of the sequence, how many).
    skipped_gaps: Vec<(usize, usize)>,
}

impl Sequence {
    /// Opens `paths` in this order.
    ///
    /// A file LightCraft cannot read (damaged, cut short) is left out and listed in
    /// [`Self::skipped`]: one bad frame in thousands does not stop the sequence (its place is a
    /// gap, which the gap filling covers).
    ///
    /// # Errors
    /// When the list is empty, a path cannot be resolved, a file is a copy of another, or no file
    /// is a readable photo.
    pub fn open(paths: &[PathBuf]) -> Result<Self, Error> {
        if paths.is_empty() {
            return Err(Error::Empty);
        }
        let absolute: Vec<PathBuf> = paths
            .iter()
            .map(|p| {
                p.canonicalize().map_err(|e| Error::Path {
                    path: p.clone(),
                    reason: e.to_string(),
                })
            })
            .collect::<Result<_, _>>()?;
        let mut session = Session::new().with_fs().with_system_clock();
        let list: Vec<String> = absolute
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        session
            .execute("library.import", &json!({ "paths": list }))
            .map_err(|e| Error::Import(e.to_string()))?;

        let by_path: BTreeMap<PathBuf, PhotoId> = session
            .catalog
            .photos()
            .filter_map(|p| match &p.source {
                Source::File { path } => Some((PathBuf::from(path), p.id)),
                Source::Demo { .. } => None,
            })
            .collect();
        let mut photos = Vec::with_capacity(absolute.len());
        let mut frames = Vec::with_capacity(absolute.len());
        let mut skipped = Vec::new();
        let mut skipped_gaps: Vec<(usize, usize)> = Vec::new();
        for path in &absolute {
            // A raw cut short (a copy from the card stopped): LightCraft reads its header and fills
            // the missing sensor data with black, so it is caught here and left out like one it
            // cannot read.
            let found = by_path.get(path).filter(|_| !cut_short(path));
            let Some(&id) = found else {
                // LightCraft skips a file with the same bytes as one it imported: say so, rather than
                // calling the copy unreadable.
                if let Some(of) = identical_earlier(path, &absolute) {
                    return Err(Error::Duplicate {
                        path: path.clone(),
                        of,
                    });
                }
                skipped.push(path.clone());
                match skipped_gaps.last_mut() {
                    Some((at, n)) if *at == photos.len() => *n += 1,
                    _ => skipped_gaps.push((photos.len(), 1)),
                }
                continue;
            };
            let photo = session
                .catalog
                .photo(id)
                .ok_or_else(|| Error::NotImported(path.clone()))?;
            frames.push(frame_of(path, photo));
            photos.push(id);
        }
        if photos.is_empty() {
            return Err(Error::NotImported(skipped.first().cloned().unwrap_or_default()));
        }
        Ok(Self {
            session,
            photos,
            frames,
            look: SequenceLook::Own,
            defects: Arc::new(Vec::new()),
            skipped,
            skipped_gaps,
        })
    }

    /// Files left out because they could not be read.
    #[must_use]
    pub fn skipped(&self) -> &[PathBuf] {
        &self.skipped
    }

    /// Where the left-out files were: (before which frame, how many) – gaps to fill, whatever the
    /// capture times say (photos without them too). Left out at the very start or end: no gap.
    #[must_use]
    pub fn skipped_gaps(&self) -> Vec<(usize, usize)> {
        self.skipped_gaps
            .iter()
            .copied()
            .filter(|&(at, _)| at > 0 && at < self.photos.len())
            .collect()
    }

    /// Develops every frame with one look from now on: the camera look fitted to `preferred`'s JPEG, else to
    /// the first of a few frames spread over the sequence that gets one, else none fitted to any frame.
    ///
    /// LightCraft fits a look to each raw's embedded JPEG when the raw has no colour matrices (Sony,
    /// Nikon, Canon …). The camera's JPEG processing changes from frame to frame (Sony DRO, auto
    /// contrast), so per-frame looks flicker and hide the camera's exposure steps; and in a dark scene the
    /// fit is accepted for some frames and refused for others, which renders those about ¾ stop apart (a
    /// night sky at ISO 800: 3 of 169 frames fitted). Raws with colour matrices of their own
    /// get no fitted look and develop the same either way.
    /// Call before rendering: sources decoded earlier keep their own look.
    ///
    /// # Errors
    /// When `preferred` is out of range or its file cannot be read or decoded.
    pub fn use_one_look(&mut self, preferred: usize) -> Result<OneLook, Error> {
        let n = self.frames.len();
        if preferred >= n {
            return Err(Error::NoFrame(preferred));
        }
        let mut candidates = vec![preferred];
        for k in 0..=LOOK_CANDIDATES {
            let i = k * (n - 1) / LOOK_CANDIDATES;
            if !candidates.contains(&i) {
                candidates.push(i);
            }
        }
        let mut chosen = OneLook::Unfitted;
        for (k, &i) in candidates.iter().enumerate() {
            let Some(path) = self.frames.get(i).map(|f| f.path.clone()) else {
                continue;
            };
            let look = std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| lightcraft_engine::files::camera_look_of(&bytes));
            match look {
                Ok(Some(look)) => {
                    self.look = SequenceLook::Shared(Arc::new(look));
                    chosen = OneLook::Fitted(i);
                    break;
                }
                Ok(None) => {}
                // The preferred frame must be readable; another frame that is not is passed over here and
                // reported when it is rendered.
                Err(reason) if k == 0 => return Err(Error::Path { path, reason }),
                Err(_) => {}
            }
        }
        if chosen == OneLook::Unfitted {
            self.look = SequenceLook::Unfitted;
        }
        self.install_loader();
        Ok(chosen)
    }

    /// Repairs the raw samples at `defects` (hot pixels, [`hot_pixels`]) in every frame rendered from now on.
    pub fn repair_samples(&mut self, defects: Vec<usize>) {
        self.defects = Arc::new(defects);
        self.install_loader();
    }

    fn install_loader(&mut self) {
        let media = &mut self.session.media;
        media.file_loader = Some(lightcraft_engine::files::fs_loader_for_sequence(
            self.look.clone(),
            self.defects.clone(),
        ));
        // Thumbnails would otherwise load with their own looks and unrepaired.
        media.reduced_loader = None;
    }

    /// Whether every frame has the same pixel size: hot pixel positions ([`hot_pixels`]) are sample
    /// positions in one size of mosaic and are wrong in frames of another (a crop mode, a second camera).
    #[must_use]
    pub fn one_size(&self) -> bool {
        let mut sizes = self
            .photos
            .iter()
            .map(|id| self.session.catalog.photo(*id).map(|p| (p.width, p.height)));
        let first = sizes.next().flatten();
        first.is_some() && sizes.all(|s| s == first)
    }

    /// LightCraft's Auto (as common raw editors have it) for `frame` developed with `settings`: exposure, contrast,
    /// highlights, shadows, whites, blacks, vibrance, saturation, as control ids and values.
    ///
    /// # Errors
    /// When `frame` is out of range or its source cannot be loaded.
    pub fn auto_tone(
        &mut self,
        frame: usize,
        settings: &DevelopSettings,
    ) -> Result<Vec<(&'static str, f64)>, Error> {
        let id = self.photo(frame)?;
        let src = self
            .session
            .source_now(id, lightcraft_engine::SourceLevel::Thumb)
            .map_err(Error::Render)?;
        let info = self.session.source_info(id);
        let a = lightcraft_engine::pipeline::auto::auto_tone(&src, &info, settings);
        Ok(vec![
            ("light.exposure", a.exposure),
            ("light.contrast", a.contrast),
            ("light.highlights", a.highlights),
            ("light.shadows", a.shadows),
            ("light.whites", a.whites),
            ("light.blacks", a.blacks),
            ("color.vibrance", a.vibrance),
            ("color.saturation", a.saturation),
        ])
    }

    /// The white balance (temperature, tint) that makes the spot at (`x`, `y`) (0..1 of the frame)
    /// neutral grey, or (no spot) the frame's grey-world balance, as LightCraft's picker and Auto do.
    ///
    /// # Errors
    /// When `frame` is out of range or its source cannot be loaded.
    pub fn white_balance(
        &mut self,
        frame: usize,
        settings: &DevelopSettings,
        spot: Option<(f64, f64)>,
    ) -> Result<(f64, f64), Error> {
        let id = self.photo(frame)?;
        let src = self
            .session
            .source_now(id, lightcraft_engine::SourceLevel::Thumb)
            .map_err(Error::Render)?;
        let info = self.session.source_info(id);
        let Some((x, y)) = spot.filter(|(x, y)| x.is_finite() && y.is_finite()) else {
            return Ok(lightcraft_engine::pipeline::auto::auto_wb(&src, &info));
        };
        // A small neighbourhood of the oriented source, averaged.
        let o = src.oriented(settings.orientation);
        let (cx, cy) = (
            (x.clamp(0.0, 1.0) * o.width as f64) as isize,
            (y.clamp(0.0, 1.0) * o.height as f64) as isize,
        );
        let mut acc = [0.0f32; 3];
        for dy in -2..=2 {
            for dx in -2..=2 {
                let c = o.get_clamped(cx + dx, cy + dy);
                for (a, v) in acc.iter_mut().zip(c) {
                    *a += v / 25.0;
                }
            }
        }
        let patch = lightcraft_raster::Rgb32f::filled(4, 4, acc);
        Ok(lightcraft_engine::pipeline::auto::auto_wb(&patch, &info))
    }

    /// The pixel size of `frame`'s original, as the catalog knows it (`None` when unknown).
    #[must_use]
    pub fn source_size(&self, frame: usize) -> Option<(usize, usize)> {
        let id = self.photos.get(frame)?;
        let p = self.session.catalog.photo(*id)?;
        let (w, h) = (usize::try_from(p.width).ok()?, usize::try_from(p.height).ok()?);
        (w > 0 && h > 0).then_some((w, h))
    }

    /// The frames, in sequence order.
    #[must_use]
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn photo(&self, frame: usize) -> Result<PhotoId, Error> {
        self.photos.get(frame).copied().ok_or(Error::NoFrame(frame))
    }

    /// The frame's settings as LightCraft has them (from its XMP sidecar, else defaults), with the
    /// white balance made explicit: an "as shot" or preset white balance becomes the Kelvin values
    /// it stands for on this frame. A sequence then keeps one white balance instead of following the
    /// camera's auto white balance from frame to frame, which flickers.
    ///
    /// # Errors
    /// When `frame` is out of range.
    pub fn settings(&self, frame: usize) -> Result<DevelopSettings, Error> {
        let id = self.photo(frame)?;
        let mut d = self
            .session
            .develop_of(id)
            .map(|d| (*d).clone())
            .unwrap_or_default();
        let info = self.session.source_info(id);
        lightcraft_engine::pipeline::local::normalize_wb(&mut d, &info);
        if d.wb.mode == WbMode::AsShot {
            d.wb.temp = info.as_shot_temp;
            d.wb.tint = info.as_shot_tint;
        }
        d.wb.mode = WbMode::Custom;
        Ok(d)
    }

    /// A keyframe on `frame` holding every numeric control of its [`Self::settings`].
    ///
    /// # Errors
    /// When `frame` is out of range.
    pub fn keyframe(&self, frame: usize) -> Result<Keyframe, Error> {
        Ok(Self::keyframe_of(frame, &self.settings(frame)?))
    }

    /// The keyframe holding every numeric control of `d`, on `frame`.
    fn keyframe_of(frame: usize, d: &DevelopSettings) -> Keyframe {
        let d = d.clone();
        let mut values = BTreeMap::new();
        for spec in controls::CONTROLS {
            if let Some(v) = controls::get(&d, spec.id) {
                values.insert(spec.id.to_owned(), v);
            }
        }
        for (id, _) in controls::indexed_instances(&d) {
            if let Some(v) = controls::get(&d, &id) {
                values.insert(id, v);
            }
        }
        values.extend(masks::values(&d));
        Keyframe { frame, values }
    }

    /// Settings for one frame: `base` (the nearest keyframe's settings, for everything that is not a
    /// number) with `values` applied, then `exposure_offset` stops added to the exposure.
    ///
    /// The offset (ramp compensation plus deflicker) is added past the ±5 stop range of the
    /// Exposure slider: a day-to-night ramp can need more, and the pipeline renders it exactly.
    #[must_use]
    pub fn frame_settings(
        base: &DevelopSettings,
        values: &BTreeMap<String, f64>,
        exposure_offset: f64,
    ) -> DevelopSettings {
        let mut d = base.clone();
        for (id, v) in values {
            if id.starts_with(masks::PREFIX) {
                masks::set(&mut d, id, *v);
            } else {
                controls::set(&mut d, id, *v);
            }
        }
        if exposure_offset.is_finite() {
            d.light.exposure += exposure_offset;
        }
        d
    }

    /// Frames with an XMP sidecar: the keyframes, edited in LightCraft (or another raw editor).
    #[must_use]
    pub fn keyframe_frames(&self) -> Vec<usize> {
        use lightcraft_engine::sidecar::{SidecarNaming, find_sidecar};
        self.frames
            .iter()
            .enumerate()
            .filter(|(_, f)| find_sidecar(&f.path.to_string_lossy(), SidecarNaming::default()).is_some())
            .map(|(i, _)| i)
            .collect()
    }

    /// What `frame`'s sidecar says about developing, as text to compare: its develop settings
    /// only (a rating, a label or a metadata date written beside them is no edit), or the whole
    /// file when it can't be read as a sidecar. `None` without a sidecar.
    #[must_use]
    pub fn sidecar_develop_text(&self, frame: usize) -> Option<String> {
        use lightcraft_engine::sidecar::parse_sidecar;
        let bytes = std::fs::read(self.sidecar(frame)?).ok()?;
        // Lapsewerk's own automatic mark is no edit.
        let text = without_automatic_mark(&String::from_utf8_lossy(&bytes));
        // Another editor's crs values beside ours count too: edited there, the keyframe is edited.
        let crs: String = text
            .lines()
            .filter(|l| l.contains("crs:") && !l.contains("xmlns:crs"))
            .map(str::trim)
            .collect::<Vec<_>>()
            .join("\n");
        Some(match parse_sidecar(&text, lightcraft_engine::crs::Target::Any) {
            // Without another editor's crs values the text is the settings alone, as before they were
            // added: a project's fingerprints of earlier versions still match (its automatic
            // keyframes stay automatic).
            Ok(d) if crs.is_empty() => format!("{:?}", d.develop),
            Ok(d) => format!("{:?}\n{crs}", d.develop),
            Err(_) => text,
        })
    }

    /// Keyframes whose sidecar holds settings this version does not know (written by a newer
    /// LightCraft): they are left out when developing, and named here, each by its path
    /// ("light.future_slider"), at most a few per keyframe.
    #[must_use]
    pub fn unknown_settings(&self) -> Vec<(usize, Vec<String>)> {
        self.keyframe_frames()
            .into_iter()
            .filter_map(|f| {
                let text = std::fs::read_to_string(self.sidecar(f)?).ok()?;
                let raw = lc_settings_json(&text)?;
                // As this version holds them (read from that sidecar) and would write them.
                let known = serde_json::to_value(self.settings(f).ok()?).ok()?;
                let mut missing = Vec::new();
                unknown_paths(&raw, &known, "", 0, &mut missing);
                (!missing.is_empty()).then_some((f, missing))
            })
            .collect()
    }

    /// Keyframes whose sidecar holds Lapsewerk's (LightCraft's) settings and crs develop
    /// values beside them: saved by another raw editor after Lapsewerk wrote it. LightCraft's own settings
    /// win, so that edit does not show; said, so it is not lost unseen.
    #[must_use]
    pub fn other_editor_beside(&self) -> Vec<usize> {
        self.keyframe_frames()
            .into_iter()
            .filter(|&f| {
                self.sidecar(f)
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .is_some_and(|t| t.contains("lc:settings") && has_crs_settings(&t))
            })
            .collect()
    }

    /// Keyframes whose sidecar holds no develop settings LightCraft can read (damaged, or written
    /// by a program whose settings it does not know): they develop with the defaults, and say so.
    #[must_use]
    pub fn unreadable_sidecars(&self) -> Vec<usize> {
        use lightcraft_engine::sidecar::parse_sidecar;
        self.keyframe_frames()
            .into_iter()
            .filter(|&f| {
                self.sidecar(f)
                    .and_then(|p| std::fs::read(p).ok())
                    .and_then(|b| {
                        parse_sidecar(&String::from_utf8_lossy(&b), lightcraft_engine::crs::Target::Any).ok()
                    })
                    .is_none_or(|d| d.develop.is_none())
            })
            .collect()
    }

    /// Makes `frames` keyframes to edit in LightCraft: each one without an XMP sidecar gets one holding
    /// the settings `plan` gives it now (the existing keyframes interpolated, no ramp or deflicker
    /// offset), so editing starts from what the video shows there and nothing changes until it is
    /// edited. Frames that already have a sidecar are left alone: they are keyframes already, possibly
    /// edited, and are never overwritten.
    ///
    /// LightCraft writes the sidecar (`lc:settings`, atomically); originals are not touched. Call
    /// [`Self::plan`] again afterwards to pick the new keyframes up.
    ///
    /// # Errors
    /// When a frame is out of range; a sidecar that cannot be written is reported in its entry.
    pub fn write_keyframes(
        &mut self,
        plan: &DevelopPlan,
        frames: &[usize],
    ) -> Result<Vec<KeyframeWrite>, Error> {
        use lightcraft_engine::catalog::Op;
        use lightcraft_engine::sidecar::{SidecarNaming, find_sidecar};
        let mut out = Vec::with_capacity(frames.len());
        for &frame in frames {
            let id = self.photo(frame)?;
            let path = self
                .frames
                .get(frame)
                .map(|f| f.path.clone())
                .ok_or(Error::NoFrame(frame))?;
            if let Some(existing) = find_sidecar(&path.to_string_lossy(), SidecarNaming::default()) {
                out.push(KeyframeWrite::Existing {
                    frame,
                    sidecar: existing,
                });
                continue;
            }
            let op = Op::SetDevelop {
                id,
                settings: Arc::new(plan.settings(frame, 0.0)),
                label: "Lapsewerk keyframe".into(),
                edited: None,
            };
            let written = self
                .session
                .catalog
                .apply(op)
                .map_err(|e| e.to_string())
                .and_then(|_| self.session.save_sidecar(id).map_err(|e| e.to_string()));
            out.push(match written {
                Ok(saved) => KeyframeWrite::Written {
                    frame,
                    sidecar: saved.path,
                },
                Err(reason) => KeyframeWrite::Failed { frame, reason },
            });
        }
        Ok(out)
    }

    /// Saves `settings` as `frame`'s keyframe: written into its XMP sidecar by LightCraft (merged into
    /// an existing one: another program's properties stay), so it is a keyframe from now on. Call
    /// [`Self::plan`] again to pick it up.
    ///
    /// # Errors
    /// When `frame` is out of range or the sidecar cannot be written.
    pub fn save_keyframe(&mut self, frame: usize, settings: DevelopSettings) -> Result<PathBuf, Error> {
        use lightcraft_engine::catalog::Op;
        let id = self.photo(frame)?;
        let op = Op::SetDevelop {
            id,
            settings: Arc::new(settings),
            label: "Lapsewerk keyframe".into(),
            edited: None,
        };
        self.session
            .catalog
            .apply(op)
            .map_err(|e| Error::Sidecar(e.to_string()))?;
        self.session
            .save_sidecar(id)
            .map(|saved| saved.path)
            .map_err(|e| Error::Sidecar(e.to_string()))
    }

    /// The XMP sidecar of `frame`, if it has one.
    #[must_use]
    pub fn sidecar(&self, frame: usize) -> Option<PathBuf> {
        use lightcraft_engine::sidecar::{SidecarNaming, find_sidecar};
        let f = self.frames.get(frame)?;
        find_sidecar(&f.path.to_string_lossy(), SidecarNaming::default())
    }

    /// The mark Lapsewerk left in `frame`'s sidecar for an automatic keyframe (`lapsewerk:Automatic`,
    /// the fingerprint of its settings when it was last set automatically), when there is one: so
    /// photos and XMP passed on without `lapsewerk.json` keep which keyframes follow the others.
    /// Read only where Lapsewerk writes it ([`mark_in`]).
    #[must_use]
    pub fn automatic_mark(&self, frame: usize) -> Option<String> {
        let text = std::fs::read_to_string(self.sidecar(frame)?).ok()?;
        let (value, _) = mark_in(&text)?.1?;
        let v = text.get(value)?;
        (!v.is_empty() && v.len() <= 128).then(|| v.to_owned())
    }

    /// Marks `frame`'s keyframe as automatic with `print` (see [`Self::automatic_mark`]): the one
    /// attribute in the start tag of the sidecar's first `rdf:Description`, nothing else in the
    /// file changed. Left alone (`Ok(false)`) whenever that can't be done for sure: the mark
    /// written some other way (as an element, in a comment, in another's value), the namespace
    /// declared elsewhere, the sidecar a link, a hard-linked or read-only file, or read back with
    /// other settings; `lapsewerk.json` then keeps the keyframe automatic alone. Written
    /// atomically, with the file's permissions.
    ///
    /// # Errors
    /// When there is no sidecar, or it can't be read or written.
    pub fn set_automatic_mark(&self, frame: usize, print: &str) -> Result<bool, Error> {
        use lightcraft_engine::sidecar::parse_sidecar;
        let path = self.sidecar(frame).ok_or(Error::NoFrame(frame))?;
        let failed = |e: &dyn std::fmt::Display| Error::Sidecar(format!("{}: {e}", path.display()));
        if print.is_empty() || print.len() > 128 || !print.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(failed(&"a fingerprint of letters and digits is expected"));
        }
        // A plain file of its own, writable: a link, a second name of the same file or a
        // read-only one is someone's arrangement, not to be replaced.
        let meta = std::fs::symlink_metadata(&path).map_err(|e| failed(&e))?;
        #[cfg(unix)]
        let shared = std::os::unix::fs::MetadataExt::nlink(&meta) > 1;
        #[cfg(not(unix))]
        let shared = false;
        if !meta.is_file() || shared || meta.permissions().readonly() || meta.len() > 64 << 20 {
            return Ok(false);
        }
        let text = std::fs::read_to_string(&path).map_err(|e| failed(&e))?;
        let Some((tag, found)) = mark_in(&text) else {
            return Ok(false);
        };
        let marked = match found {
            Some((value, _)) if text.get(value.clone()) == Some(print) => return Ok(true),
            Some((value, _)) => format!(
                "{}{print}{}",
                text.get(..value.start).unwrap_or_default(),
                text.get(value.end..).unwrap_or_default()
            ),
            None => {
                let declared = text
                    .get(tag.clone())
                    .is_some_and(|t| t.contains("xmlns:lapsewerk="));
                let at = tag.start + "<rdf:Description".len();
                let ns = if declared {
                    String::new()
                } else {
                    format!(" xmlns:lapsewerk=\"{AUTOMATIC_NS}\"")
                };
                format!(
                    "{}{ns} {AUTOMATIC_ATTR}=\"{print}\"{}",
                    text.get(..at).unwrap_or_default(),
                    text.get(at..).unwrap_or_default()
                )
            }
        };
        // Read back: the same settings, and the mark where it was put.
        let target = lightcraft_engine::crs::Target::Any;
        let same = match (parse_sidecar(&text, target), parse_sidecar(&marked, target)) {
            (Ok(a), Ok(b)) => format!("{:?}", a.develop) == format!("{:?}", b.develop),
            _ => false,
        };
        let placed = mark_in(&marked)
            .and_then(|(_, f)| f)
            .and_then(|(v, _)| marked.get(v).map(|v| v == print))
            .unwrap_or(false);
        if !same || !placed {
            return Ok(false);
        }
        let tmp = path.with_extension(format!("xmp.{}.tmp", std::process::id()));
        let written = (|| {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            f.write_all(marked.as_bytes())?;
            f.set_permissions(meta.permissions())?;
            f.sync_all()?;
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(failed(&e));
        }
        Ok(true)
    }

    /// Reads `frame`'s sidecar again (another program, such as LightCraft, changed it); whether it
    /// had one. Call [`Self::plan`] again afterwards.
    ///
    /// # Errors
    /// When `frame` is out of range or the sidecar cannot be read.
    pub fn reload_keyframe(&mut self, frame: usize) -> Result<bool, Error> {
        let id = self.photo(frame)?;
        let op = self
            .session
            .read_sidecar_op(id)
            .map_err(|e| Error::Sidecar(e.to_string()))?;
        let Some((op, _)) = op else { return Ok(false) };
        self.session
            .catalog
            .apply(op)
            .map_err(|e| Error::Sidecar(e.to_string()))?;
        Ok(true)
    }

    /// How every frame is developed: the keyframes' numeric controls interpolated over the sequence,
    /// everything else (profile, masks, crop) from the nearest keyframe at or before the frame. A
    /// sequence without keyframes is developed with frame 0's settings throughout.
    ///
    /// # Errors
    /// When the keyframes cannot be interpolated.
    pub fn plan(&self, mode: Interpolation) -> Result<DevelopPlan, Error> {
        self.plan_of(mode, self.keyframe_frames())
    }

    /// Auto transition: the keyframes in `fill` (made, but never edited)
    /// are set to what the other keyframes give them, interpolated, and saved so: an edited day
    /// and night then blend through them instead of being pulled back to the settings the
    /// in-between keyframes had when they were made. Nothing happens without another keyframe.
    ///
    /// # Errors
    /// When the keyframes cannot be interpolated or a sidecar cannot be written.
    pub fn auto_transition(&mut self, mode: Interpolation, fill: &[usize]) -> Result<Vec<usize>, Error> {
        self.auto_transition_along(mode, fill, None)
    }

    /// [`Self::auto_transition`], the blend following `light` (the scene's brightness per frame,
    /// in stops) where given: between two edited keyframes a frame gets the settings of where the
    /// light stands, not of where its number stands, so a holy grail's look turns in the blue
    /// hour, where the light changes most.
    ///
    /// # Errors
    /// As [`Self::auto_transition`].
    pub fn auto_transition_along(
        &mut self,
        mode: Interpolation,
        fill: &[usize],
        light: Option<&[f64]>,
    ) -> Result<Vec<usize>, Error> {
        let keep: Vec<usize> = self
            .keyframe_frames()
            .into_iter()
            .filter(|f| !fill.contains(f))
            .collect();
        if keep.is_empty() {
            return Ok(Vec::new());
        }
        let plan = self.plan_of(mode, keep.clone())?;
        // How far the light has come at each frame: the summed change, so it only grows.
        let travelled: Option<Vec<f64>> = light.filter(|l| l.len() == self.len()).map(|l| {
            let mut sum = 0.0;
            let mut out = Vec::with_capacity(l.len());
            let mut last = l.first().copied().filter(|v| v.is_finite());
            for v in l {
                if let (Some(a), true) = (last, v.is_finite()) {
                    sum += (v - a).abs();
                }
                if v.is_finite() {
                    last = Some(*v);
                }
                out.push(sum);
            }
            out
        });
        let mut done = Vec::new();
        for &f in fill {
            if f >= self.len() {
                continue;
            }
            // Only between edited keyframes: one before the first edited or
            // after the last keeps its own look (an edited night alone would make the whole day
            // as bright as the night, burnt out).
            if !keep.iter().any(|k| *k < f) || !keep.iter().any(|k| *k > f) {
                continue;
            }
            let at = match (
                &travelled,
                keep.iter().rev().find(|k| **k < f),
                keep.iter().find(|k| **k > f),
            ) {
                (Some(c), Some(&a), Some(&b)) => {
                    let (ca, cb, cf) = (
                        c.get(a).copied().unwrap_or(0.0),
                        c.get(b).copied().unwrap_or(0.0),
                        c.get(f).copied().unwrap_or(0.0),
                    );
                    // Under a third of a stop between them: by frame, the light says nothing.
                    if cb - ca > 1.0 / 3.0 {
                        let t = ((cf - ca) / (cb - ca)).clamp(0.0, 1.0);
                        let g = a as f64 + t * (b - a) as f64;
                        (g.round() as usize).clamp(a, b)
                    } else {
                        f
                    }
                }
                _ => f,
            };
            // A colour label LightCraft does not know (another editor's German "Rot") would be lost in
            // its rewrite: kept, and put back if it went.
            let label = self
                .sidecar(f)
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|t| label_in(&t));
            self.save_keyframe(f, plan.settings(at, 0.0))?;
            if let Some(label) = label {
                let _ = self.keep_label(f, &label);
            }
            done.push(f);
        }
        Ok(done)
    }

    /// Whether `frame`'s sidecar may be written over in place: a plain file of its own (no link,
    /// no second name), writable. A read-only, linked or hard-linked sidecar is someone's
    /// arrangement: the auto transition leaves it alone (its keyframe counts as edited).
    #[must_use]
    pub fn sidecar_rewritable(&self, frame: usize) -> bool {
        let Some(path) = self.sidecar(frame) else {
            return true;
        };
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return false;
        };
        #[cfg(unix)]
        let shared = std::os::unix::fs::MetadataExt::nlink(&meta) > 1;
        #[cfg(not(unix))]
        let shared = false;
        meta.is_file() && !shared && !meta.permissions().readonly()
    }

    /// Puts `label` back into `frame`'s sidecar as `xmp:Label` when a rewrite lost it (an attribute
    /// of the first `rdf:Description`, nothing else changed, the settings read back the same).
    fn keep_label(&self, frame: usize, label: &str) -> Result<bool, Error> {
        use lightcraft_engine::sidecar::parse_sidecar;
        let path = self.sidecar(frame).ok_or(Error::NoFrame(frame))?;
        let failed = |e: &dyn std::fmt::Display| Error::Sidecar(format!("{}: {e}", path.display()));
        let text = std::fs::read_to_string(&path).map_err(|e| failed(&e))?;
        if text.contains("xmp:Label") {
            return Ok(false);
        }
        let Some((tag, _)) = description_tag(&text) else {
            return Ok(false);
        };
        let declared_here = text.get(tag.clone()).is_some_and(|t| t.contains("xmlns:xmp="));
        // The xmp prefix declared on another element only: not ours to rearrange.
        if !declared_here && text.contains("xmlns:xmp=") {
            return Ok(false);
        }
        let ns = if declared_here {
            String::new()
        } else {
            " xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"".to_owned()
        };
        let escaped = label
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('"', "&quot;");
        let at = tag.start + "<rdf:Description".len();
        let new = format!(
            "{}{ns} xmp:Label=\"{escaped}\"{}",
            text.get(..at).unwrap_or_default(),
            text.get(at..).unwrap_or_default()
        );
        let target = lightcraft_engine::crs::Target::Any;
        let same = match (parse_sidecar(&text, target), parse_sidecar(&new, target)) {
            (Ok(a), Ok(b)) => format!("{:?}", a.develop) == format!("{:?}", b.develop),
            _ => false,
        };
        if !same {
            return Ok(false);
        }
        write_sidecar_atomically(&path, &new).map_err(|e| failed(&e))?;
        Ok(true)
    }

    fn plan_of(&self, mode: Interpolation, mut frames: Vec<usize>) -> Result<DevelopPlan, Error> {
        if frames.is_empty() {
            frames.push(0);
        }
        let mut bases = Vec::with_capacity(frames.len());
        for &f in &frames {
            let mut d = self.settings(f)?;
            // A crop switched off but kept in the file (HasCrop False beside its edges):
            // taken, as established time-lapse tools take it, so the crop blends towards it like any other.
            if d.crop.geometry.is_identity()
                && let Some(text) = self.sidecar(f).and_then(|p| std::fs::read_to_string(p).ok())
                && let Some(size) = self.source_size(f)
                && let Some(g) = crop_switched_off(&text, size)
            {
                d.crop.geometry = g;
            }
            bases.push((f, d));
        }
        // Masks blend by name, whatever their order in each keyframe: every keyframe gets all of
        // them in one order, those it lacks with no effect (so they fade in or out).
        let mut settings: Vec<DevelopSettings> = bases.iter().map(|(_, d)| d.clone()).collect();
        masks::align(&mut settings);
        for ((_, d), aligned) in bases.iter_mut().zip(settings) {
            *d = aligned;
        }
        let keys: Vec<Keyframe> = bases.iter().map(|(f, d)| Self::keyframe_of(*f, d)).collect();
        let curves =
            keyframes::interpolate(&keys, self.len(), mode).map_err(|e| Error::Keyframes(e.to_string()))?;
        Ok(DevelopPlan {
            keyframes: frames,
            bases,
            curves,
            look: Vec::new(),
            crop_fit: None,
        })
    }

    /// A render job for `frame` with `settings`, fitting `max_w` × `max_h`, as linear float
    /// Rec. 2020 (for measuring) or 16-bit display-encoded `space` (for export).
    ///
    /// The job is detached from the session and cache-free: run it on any thread, any number at a
    /// time (each holds one decoded frame while it runs).
    ///
    /// # Errors
    /// When `frame` is out of range or LightCraft cannot make the job.
    pub fn render_job(
        &mut self,
        frame: usize,
        settings: DevelopSettings,
        max_w: usize,
        max_h: usize,
        output: Output,
    ) -> Result<RenderJob, Error> {
        let id = self.photo(frame)?;
        // Only a plain file is read: a photo turned into a pipe or a device since the series was
        // opened would never end (the app hung on it).
        if let Some(f) = self.frames.get(frame)
            && !std::fs::metadata(&f.path).is_ok_and(|m| m.is_file())
        {
            return Err(Error::Path {
                path: f.path.clone(),
                reason: "is no longer a plain file (a pipe, a device, or gone)".into(),
            });
        }
        let (space, depth) = match output {
            Output::LinearRec2020 => (OutputSpace::Rec2020, OutputDepth::F32Linear),
            Output::Display16(space) => (space, OutputDepth::U16),
            Output::HdrRec2020 => (OutputSpace::Rec2020, OutputDepth::F32Hdr),
            Output::Preview8 => (OutputSpace::Srgb, OutputDepth::U8),
        };
        let mut job = self
            .session
            .export_job(id, max_w.max(1), max_h.max(1), space, depth)
            .map_err(Error::Render)?;
        // LightCraft loads anything over 2560 px at the sensor's full size (33 MP for the A7 IV) and
        // develops all of it; a video frame needs a third of that. Load at the size the output needs
        // instead (its source level's rule, `source_edge_needed`, with the actual output size). The
        // job keeps the original's size in `info.native_long`, so noise reduction and sharpening
        // still measure their radii in the original's pixels.
        // Measured on the reference sequence (4K, A/B against the full source): loading exactly the
        // size needed costs 3 % edge contrast (the loader shrinks with a box filter); 25 % more and the
        // pipeline's own resampling does the last step: 1 % sharper than the full source, 8 % faster.
        // 1080p renders 4.6 times faster (the loader may bin the mosaic, never below the size needed).
        // (`LAPSEWERK_FULL_SOURCE=1` turns this off, to compare renders.)
        let needed = self.source_edge_needed(id, &settings, max_w.max(max_h));
        if std::env::var_os("LAPSEWERK_FULL_SOURCE").is_none()
            && let lightcraft_engine::media::SourceRef::File { max_edge, .. } = &mut job.source
            && needed < *max_edge
        {
            *max_edge = needed;
        }
        // Sky masks: the sky found in this frame by its own light (works at dusk and at night,
        // where LightCraft's own sky heuristic finds nothing). Its sky parts become a segmentation
        // LightCraft samples (a "sky" prompt holding the grid), for this render only: what is
        // saved stays a sky mask, found anew in every frame.
        let mut settings = settings;
        if has_sky_mask(&settings)
            && let Some(seg) = self.sky_seg(id, &settings)
        {
            use lightcraft_engine::develop::MaskShape;
            for c in settings.masks.iter_mut().flat_map(|m| m.components.iter_mut()) {
                if matches!(c.shape, MaskShape::Sky) {
                    c.shape = MaskShape::Prompt {
                        text: "sky".into(),
                        seg: Some(seg.clone()),
                        detail: Vec::new(),
                        edge: 0.0,
                    };
                }
            }
        }
        job.settings = Arc::new(settings);
        job.cache = None;
        job.view_cache = None;
        job.stages = None;
        // Full quality even for measuring: draft renders skip refinements that change brightness.
        job.request.quality = Quality::Full;
        Ok(job)
    }

    /// The sky of photo `id` as a segmentation grid over the upright image (what LightCraft's
    /// prompt masks sample), found on its thumbnail ([`lapsewerk_core::skymask`]).
    fn sky_seg(
        &mut self,
        id: PhotoId,
        settings: &DevelopSettings,
    ) -> Option<lightcraft_engine::develop::SegMask> {
        /// Grid side: 512 × 512 over the frame (about 8 px a cell in 4K), bilinear in between: the
        /// matte's edge follows the ridge at the thumbnail's resolution.
        const SIDE: usize = 512;
        let src = self
            .session
            .source_now(id, lightcraft_engine::SourceLevel::Thumb)
            .ok()?;
        let o = src.oriented(settings.orientation);
        let flat: Vec<f32> = o.data.iter().flatten().copied().collect();
        let m = lapsewerk_core::skymask::find_sky(&flat, o.width, o.height, SIDE, SIDE)?;
        // 0..255 as logits: sigmoid(logit) gives the matte back (0 and 255 as ±8, clearly out).
        let logits: Vec<f32> = m
            .data
            .iter()
            .map(|&v| {
                let p = (f32::from(v) / 255.0).clamp(1e-3, 1.0 - 1e-3);
                (p / (1.0 - p)).ln().clamp(-8.0, 8.0)
            })
            .collect();
        Some(lightcraft_engine::develop::SegMask::from_logits(SIDE, &logits))
    }

    /// Long edge of the source to load for an output fitting `out_long` pixels: what the output shows
    /// of the photo (more than `out_long` when cropped), 25 % over, never binned below it.
    fn source_edge_needed(&self, id: PhotoId, settings: &DevelopSettings, out_long: usize) -> usize {
        let Some(p) = self.session.catalog.photo(id) else {
            return usize::MAX;
        };
        let (w, h) = (f64::from(p.width.max(1)), f64::from(p.height.max(1)));
        let r = settings.crop.geometry.rect;
        let shown = (r.width() * w).max(r.height() * h);
        if !(shown.is_finite() && shown >= 1.0) {
            return usize::MAX;
        }
        let needed = (out_long as f64 * w.max(h) / shown).ceil() + 2.0;
        if !(needed.is_finite() && needed < 1e9) {
            return usize::MAX;
        }
        no_binning_below((needed * 1.25) as usize, p.width.max(p.height) as usize)
    }
}

/// Whether `settings` has a mask with a sky part.
fn has_sky_mask(settings: &DevelopSettings) -> bool {
    settings.masks.iter().any(|m| {
        m.components
            .iter()
            .any(|c| matches!(c.shape, lightcraft_engine::develop::MaskShape::Sky))
    })
}

/// What [`Sequence::write_keyframes`] did for one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyframeWrite {
    /// A new sidecar was written.
    Written { frame: usize, sidecar: PathBuf },
    /// The frame has a sidecar already; it was left as it is.
    Existing { frame: usize, sidecar: PathBuf },
    /// The sidecar could not be written.
    Failed { frame: usize, reason: String },
}

/// An earlier file in `paths` with exactly the bytes of `path` (compared only on the way to an error).
fn identical_earlier(path: &Path, paths: &[PathBuf]) -> Option<PathBuf> {
    let len = std::fs::metadata(path).ok()?.len();
    let bytes = std::fs::read(path).ok()?;
    paths
        .iter()
        .take_while(|p| p.as_path() != path)
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() == len))
        .find(|p| std::fs::read(p).is_ok_and(|b| b == bytes))
        .cloned()
}

/// Every frame's develop settings, from [`Sequence::plan`].
#[derive(Debug, Clone)]
pub struct DevelopPlan {
    keyframes: Vec<usize>,
    /// Keyframe → its full settings, by frame.
    bases: Vec<(usize, DevelopSettings)>,
    curves: Curves,
    /// Control values added to every frame ([`DevelopPlan::with_look`]).
    look: Vec<(String, f64)>,
    /// The video's aspect and the photos' size as shown ([`DevelopPlan::with_crop_fit`]).
    crop_fit: Option<(f64, (f64, f64))>,
}

impl DevelopPlan {
    /// The keyframes (frame 0 alone when the sequence has none).
    #[must_use]
    pub fn keyframes(&self) -> &[usize] {
        &self.keyframes
    }

    /// The plan with `look` (control id, value) added to every frame's settings: a look for a
    /// sequence nobody edited, on top of what its keyframes say.
    #[must_use]
    pub fn with_look(mut self, look: &[(&str, f64)]) -> Self {
        self.look = look.iter().map(|(id, v)| ((*id).to_string(), *v)).collect();
        self
    }

    /// The plan framed for a video of `aspect` (width / height) as established time-lapse tools frame a cropped
    /// sequence: every frame's crop (or the whole photo, where a keyframe has none) cut to the
    /// video's shape about its centre ([`video_crop`]), so frames of different crops still make
    /// one movie. `photo` is the photos' size as the catalog has it ([`Sequence::source_size`]). A
    /// plan without any crop is left alone (its frames are placed by the crop position as before).
    #[must_use]
    pub fn with_crop_fit(mut self, aspect: f64, photo: (usize, usize)) -> Self {
        let cropped = self.bases.iter().any(|(_, d)| !d.crop.geometry.is_identity());
        self.crop_fit = (cropped && aspect.is_finite() && aspect > 0.0 && photo.0 > 0 && photo.1 > 0)
            .then_some((aspect, (photo.0 as f64, photo.1 as f64)));
        self
    }

    /// Settings for `frame` with `exposure_offset` stops (ramp compensation, deflicker) added.
    #[must_use]
    pub fn settings(&self, frame: usize, exposure_offset: f64) -> DevelopSettings {
        let before = self.bases.iter().rev().find(|(f, _)| *f <= frame);
        let base = before
            .or_else(|| self.bases.first())
            .map(|(_, d)| d.clone())
            .unwrap_or_default();
        let values: BTreeMap<String, f64> = self
            .curves
            .iter()
            .filter_map(|(id, v)| v.get(frame).map(|x| (id.clone(), *x)))
            .collect();
        let mut d = Sequence::frame_settings(&base, &values, exposure_offset);
        // Brush strokes cannot be interpolated: between two keyframes painted differently, the
        // strokes before fade out and the strokes after fade in.
        if let (Some((a, _)), Some((b, next))) = (before, self.bases.iter().find(|(f, _)| *f > frame)) {
            let w = (frame - a) as f64 / (b - a).max(1) as f64;
            masks::crossfade_brushes(&mut d, next, w);
            // The crop moves from one keyframe's to the next one's (as established time-lapse tools move it):
            // its edges and straightening, frame by frame.
            let (c, n) = (d.crop.geometry, next.crop.geometry);
            if c != n {
                let mix = |x: f64, y: f64| x + (y - x) * w;
                d.crop.geometry = lightcraft_geom::CropGeometry {
                    rect: lightcraft_geom::Rect::new(
                        mix(c.rect.x0, n.rect.x0),
                        mix(c.rect.y0, n.rect.y0),
                        mix(c.rect.x1, n.rect.x1),
                        mix(c.rect.y1, n.rect.y1),
                    ),
                    angle: mix(c.angle, n.angle),
                };
            }
            // Point curves (LightCraft's tone curve) are no number: blended between the two
            // keyframes rather than taken from the one before (an S-curve jumped at the next).
            let c = &mut d.curve;
            let n = &next.curve;
            c.master = blend_points(&c.master, &n.master, w);
            c.red = blend_points(&c.red, &n.red, w);
            c.green = blend_points(&c.green, &n.green, w);
            c.blue = blend_points(&c.blue, &n.blue, w);
        }
        for (id, v) in &self.look {
            if let Some(now) = controls::get(&d, id) {
                controls::set(&mut d, id, now + v);
            }
        }
        if let Some((aspect, (w, h))) = self.crop_fit {
            let (w, h) = if d.orientation.swaps_axes() {
                (h, w)
            } else {
                (w, h)
            };
            d.crop.geometry = video_crop(d.crop.geometry, w, h, aspect);
        }
        d
    }
}

/// `crop` (of a `w` × `h` photo, as turned; uncropped: the whole photo) cut to the shape `aspect`
/// (width / height) about its centre: the part of it a video of that shape shows, as established time-lapse tools do
/// frames a crop.
#[must_use]
pub fn video_crop(
    crop: lightcraft_geom::CropGeometry,
    w: f64,
    h: f64,
    aspect: f64,
) -> lightcraft_geom::CropGeometry {
    use lightcraft_geom::{CropGeometry, Rect};
    let r = crop.rect_px(w, h);
    let (cw, ch) = (r.width(), r.height());
    if !(cw.is_finite() && ch.is_finite() && cw > 0.0 && ch > 0.0 && w > 0.0 && h > 0.0) {
        return crop;
    }
    let (nw, nh) = if cw / ch > aspect {
        (ch * aspect, ch)
    } else {
        (cw, cw / aspect)
    };
    if (nw - cw).abs() < 1e-6 * w && (nh - ch).abs() < 1e-6 * h {
        return crop;
    }
    // Smaller about the same centre: still on the (straightened) photo.
    CropGeometry {
        rect: Rect::from_center(r.center(), nw, nh).scale(1.0 / w, 1.0 / h),
        angle: crop.angle,
    }
}

/// The crop a sidecar of another raw editor keeps while it is switched off (`crs:HasCrop` False beside
/// `crs:CropLeft` … `crs:CropAngle` other than the whole photo), in LightCraft's frame. The other editor's
/// edges are on the stored image, before the orientation the sidecar names (`tiff:Orientation`);
/// `shown` is the photo's size as LightCraft shows it (turned). `None` when the crop is on
/// (LightCraft reads it then), absent or the whole photo.
fn crop_switched_off(text: &str, shown: (usize, usize)) -> Option<lightcraft_geom::CropGeometry> {
    use lightcraft_geom::{Affine, CropGeometry, Orientation, Point, Rect};
    if !value_of(text, "crs:HasCrop")?.eq_ignore_ascii_case("false") {
        return None;
    }
    let num = |name: &str| {
        value_of(text, name)
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
    };
    let (x0, y0, x1, y1) = (
        num("crs:CropLeft")?.clamp(0.0, 1.0),
        num("crs:CropTop")?.clamp(0.0, 1.0),
        num("crs:CropRight")?.clamp(0.0, 1.0),
        num("crs:CropBottom")?.clamp(0.0, 1.0),
    );
    let crs_angle = num("crs:CropAngle").unwrap_or(0.0).clamp(-45.0, 45.0);
    let whole = x0 < 1e-6 && y0 < 1e-6 && x1 > 1.0 - 1e-6 && y1 > 1.0 - 1e-6 && crs_angle == 0.0;
    if whole || x1 - x0 < 0.05 || y1 - y0 < 0.05 {
        return None;
    }
    let orientation = Orientation::from_exif(num("tiff:Orientation").map_or(1, |v| v as u16));
    // The stored image's shape: as the sidecar names it, else the shown one turned back.
    let w = match (num("tiff:ImageWidth"), num("tiff:ImageLength")) {
        (Some(w), Some(h)) if w > 0.0 && h > 0.0 => w / h,
        _ if orientation.swaps_axes() => shown.1 as f64 / shown.0.max(1) as f64,
        _ => shown.0 as f64 / shown.1.max(1) as f64,
    };
    if !(w.is_finite() && w > 0.0) {
        return None;
    }
    // As LightCraft turns a crop that is on: the corners through the orientation, then
    // the straightening (which a mirror turns the other way).
    let (ow, oh) = if orientation.swaps_axes() {
        (1.0, w)
    } else {
        (w, 1.0)
    };
    let angle = if orientation.to_parts().0 {
        crs_angle
    } else {
        -crs_angle
    };
    let to_rotated = Affine::rotate_about(angle.to_radians(), Point::new(ow / 2.0, oh / 2.0));
    let corner = |x: f64, y: f64| {
        let (u, v) = orientation.map(x * w, y, w, 1.0);
        to_rotated.apply(Point::new(u, v))
    };
    let r = Rect::from_points(corner(x0, y0), corner(x1, y1));
    let g = CropGeometry {
        rect: Rect::new(r.x0 / ow, r.y0 / oh, r.x1 / ow, r.y1 / oh),
        angle,
    };
    g.is_within_image(ow, oh).then_some(g)
}

/// The value of `name` (`prefix:Name`) in an XMP packet: an attribute (`name="v"`) or an element
/// (`<name>v</name>`).
fn value_of(text: &str, name: &str) -> Option<String> {
    let attr = format!("{name}=\"");
    if let Some(i) = text.find(&attr) {
        let rest = text.get(i + attr.len()..)?;
        return Some(rest.get(..rest.find('"')?)?.trim().to_owned());
    }
    let open = format!("<{name}>");
    let i = text.find(&open)?;
    let rest = text.get(i + open.len()..)?;
    Some(rest.get(..rest.find('<')?)?.trim().to_owned())
}

/// A point curve `w` of the way (0..1) from `a` to `b`: both read at every point either has (and
/// the ends), straight between their points, and mixed. An empty curve is the straight line.
/// Exactly `a` at 0 and `b` at 1.
fn blend_points<P: Copy + PartialEq + PointXy>(a: &[P], b: &[P], w: f64) -> Vec<P> {
    if w <= 0.0 || a == b {
        return a.to_vec();
    }
    if w >= 1.0 {
        return b.to_vec();
    }
    let Some(template) = a.first().or_else(|| b.first()).copied() else {
        return Vec::new();
    };
    // y of a curve at x: straight between its points (sorted by x), the line y = x when empty.
    let at = |pts: &[P], x: f64| -> f64 {
        let mut sorted: Vec<(f64, f64)> = pts.iter().map(|p| (p.x(), p.y())).collect();
        sorted.sort_by(|p, q| p.0.total_cmp(&q.0));
        let Some(&(x0, y0)) = sorted.first() else {
            return x;
        };
        if x <= x0 {
            return y0;
        }
        for pair in sorted.windows(2) {
            if let [(xa, ya), (xb, yb)] = pair
                && x <= *xb
            {
                let t = if xb - xa > 1e-12 {
                    (x - xa) / (xb - xa)
                } else {
                    1.0
                };
                return ya + t * (yb - ya);
            }
        }
        sorted.last().map_or(x, |p| p.1)
    };
    let knots = |pts: &[P]| -> Vec<f64> {
        let mut xs: Vec<f64> = pts.iter().map(|p| p.x()).filter(|x| x.is_finite()).collect();
        xs.sort_by(f64::total_cmp);
        xs
    };
    let same_knots = {
        let (ka, kb) = (knots(a), knots(b));
        // A curve against none (the straight line): read at the curve's own points, so it ends
        // exactly on the keyframe's curve (densely read, a hundredth of a stop off in the dark).
        a.len() < 2
            || b.len() < 2
            || (ka.len() == kb.len() && ka.iter().zip(&kb).all(|(p, q)| (p - q).abs() < 1e-6))
    };
    let mut xs: Vec<f64> = a.iter().chain(b).map(|p| p.x()).chain([0.0, 1.0]).collect();
    if same_knots {
        xs.retain(|x| x.is_finite());
        xs.sort_by(f64::total_cmp);
        xs.dedup_by(|p, q| (*p - *q).abs() < 1e-6);
        return xs
            .into_iter()
            .map(|x| {
                let y = at(a, x) * (1.0 - w) + at(b, x) * w;
                template.with_xy(x, y)
            })
            .collect();
    }
    // Different points: each curve as LightCraft draws it (its spline), read densely and mixed,
    // so a frame beside a keyframe looks like the keyframe (read straight between the points, a
    // curve beside its keyframe was another curve: a blink of 0.2 stops there).
    let spline = |pts: &[P]| {
        if pts.len() < 2 {
            lightcraft_color::spline::MonotoneCurve::identity()
        } else {
            let p: Vec<(f64, f64)> = pts.iter().map(|q| (q.x(), q.y())).collect();
            lightcraft_color::spline::MonotoneCurve::new(&p)
        }
    };
    let (sa, sb) = (spline(a), spline(b));
    const DENSE: u32 = 32;
    xs.extend((0..=DENSE).map(|k| f64::from(k) / f64::from(DENSE)));
    xs.retain(|x| x.is_finite() && (0.0..=1.0).contains(x));
    xs.sort_by(f64::total_cmp);
    xs.dedup_by(|p, q| (*p - *q).abs() < 1e-6);
    xs.into_iter()
        .map(|x| {
            let y = sa.eval(x) * (1.0 - w) + sb.eval(x) * w;
            template.with_xy(x, y.clamp(0.0, 1.0))
        })
        .collect()
}

/// A curve point's coordinates (LightCraft's point type, without naming it here).
trait PointXy {
    fn x(&self) -> f64;
    fn y(&self) -> f64;
    #[must_use]
    fn with_xy(self, x: f64, y: f64) -> Self;
}

impl PointXy for lightcraft_geom::Point {
    fn x(&self) -> f64 {
        self.x
    }
    fn y(&self) -> f64 {
        self.y
    }
    fn with_xy(self, x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// The `lc:settings` JSON in a sidecar's text, its XML escapes undone.
fn lc_settings_json(text: &str) -> Option<serde_json::Value> {
    let start = text.find("<lc:settings>")? + "<lc:settings>".len();
    let len = text.get(start..)?.find("</lc:settings>")?;
    let body = text.get(start..start + len)?;
    let body = body
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    serde_json::from_str(&body).ok()
}

/// Keys of `raw` (as written) that `known` (as this version reads and writes them) lacks, as
/// paths; a key this version leaves out while it holds its default is no stranger. At most 20.
fn unknown_paths(
    raw: &serde_json::Value,
    known: &serde_json::Value,
    at: &str,
    depth: usize,
    out: &mut Vec<String>,
) {
    const LEFT_OUT_AT_DEFAULT: [&str; 12] = [
        "camera_local_tone",
        "curve",
        "denoise_on",
        "hdr",
        "name",
        "pressure",
        "process",
        "profile",
        "raw_details",
        "rect",
        "refine",
        "scale",
    ];
    let (Some(raw), Some(known)) = (raw.as_object(), known.as_object()) else {
        return;
    };
    if depth > 8 {
        return;
    }
    for (k, v) in raw {
        if out.len() >= 20 {
            return;
        }
        let path = if at.is_empty() {
            k.clone()
        } else {
            format!("{at}.{k}")
        };
        match known.get(k) {
            Some(kv) => unknown_paths(v, kv, &path, depth + 1, out),
            None if LEFT_OUT_AT_DEFAULT.contains(&k.as_str()) => {}
            None => out.push(path),
        }
    }
}

/// Copies the controls `ids` (and the masks, with `masks`) of `from` into `to`; the rest of `to` stays.
pub fn copy_settings(from: &DevelopSettings, to: &mut DevelopSettings, ids: &[&str], masks: bool) {
    for id in ids {
        if let Some(v) = controls::get(from, id) {
            controls::set(to, id, v);
        }
    }
    if masks {
        to.masks.clone_from(&from.masks);
    }
}

/// Copies the masks of `from` at `which` (indexes) into `to`: each replaces the mask of the same
/// name there (the first not replaced yet), or is added; `to`'s other masks stay. (Masks blend
/// between keyframes by name, [`masks::align`].)
pub fn copy_masks(from: &DevelopSettings, to: &mut DevelopSettings, which: &[usize]) {
    let mut replaced = vec![false; to.masks.len()];
    for &i in which {
        let Some(m) = from.masks.get(i) else { continue };
        let same = to
            .masks
            .iter()
            .enumerate()
            .position(|(k, t)| t.name == m.name && !replaced.get(k).copied().unwrap_or(true));
        match same {
            Some(k) => {
                if let (Some(slot), Some(r)) = (to.masks.get_mut(k), replaced.get_mut(k)) {
                    *slot = m.clone();
                    *r = true;
                }
            }
            None => {
                to.masks.push(m.clone());
                replaced.push(true);
            }
        }
    }
}

/// A source size (`max_edge`) for which LightCraft's loader never bins the raw mosaic below `needed`
/// pixels on the long edge (`long`: the raw's).
///
/// The loader bins by k (block means: fast, and clean for previews) when the result is at least 90 %
/// of `max_edge` (`lightcraft_engine::files::bin_factor`). For a video frame that would mean scaling
/// up by as much as 11 %. For every k that would land between 90 % and 100 % of `needed`, the size
/// asked for is raised just past the point where the loader accepts k.
#[must_use]
pub fn no_binning_below(needed: usize, long: usize) -> usize {
    let mut max_edge = needed;
    for k in [2usize, 3, 4, 6, 8] {
        let binned = long / k;
        if binned < needed && binned >= max_edge.saturating_mul(9) / 10 {
            // (max_edge · 9 / 10 > binned: the loader rejects k)
            max_edge = binned * 10 / 9 + 2;
        }
    }
    max_edge
}

/// Where a render's time went ([`run_timed`]).
#[derive(Debug, Clone, Default)]
pub struct RenderTiming {
    /// Reading and decoding the file (raw decode, demosaic, scaling to the size needed).
    pub load: std::time::Duration,
    /// Developing the decoded picture.
    pub develop: std::time::Duration,
    /// Why the GPU was not used, when it was tried and failed.
    pub gpu_fallback: Option<String>,
}

/// Runs `job` like [`RenderJob::run`] (minus caches, which Lapsewerk's jobs never have), timing
/// the load and the develop step separately; `gpu: false` develops on the CPU.
pub fn run_timed(job: RenderJob, gpu: bool) -> (Result<Rendered, String>, RenderTiming) {
    let mut t = RenderTiming::default();
    let started = std::time::Instant::now();
    let source = job.source.load_source();
    t.load = started.elapsed();
    let source = match source {
        Ok(s) => s,
        Err(e) => return (Err(e), t),
    };
    let src = source.image_for(job.settings.denoise_amount());
    let info = source.info_or(job.info);
    let started = std::time::Instant::now();
    let (rendered, fallback) =
        lightcraft_engine::media::develop_reporting(&src, &info, &job.settings, &job.request, None, gpu);
    t.develop = started.elapsed();
    t.gpu_fallback = fallback;
    (Ok(rendered), t)
}

/// Time to read a file and to decode its raw data (before demosaicing), for `lapsewerk bench`.
#[must_use]
pub fn raw_decode_timing(path: &Path) -> Option<(std::time::Duration, std::time::Duration)> {
    let started = std::time::Instant::now();
    let bytes = std::fs::read(path).ok()?;
    let read = started.elapsed();
    let started = std::time::Instant::now();
    let raw = lightcraft_raw::decode(&bytes).ok()?;
    let _ = raw.normalized().ok()?;
    Some((read, started.elapsed()))
}

/// What a render job produces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Output {
    /// Linear float Rec. 2020 (3 channels), for measuring brightness.
    LinearRec2020,
    /// 16-bit display-encoded RGB in this space, for video export.
    Display16(OutputSpace),
    /// 8-bit sRGB in `Rendered::image`, for previews on screen.
    Preview8,
    /// Linear float Rec. 2020 with HDR headroom (SDR white 1, up to the settings' `hdr.peak()`;
    /// turn `hdr.enabled` on in the settings), for HDR video export.
    HdrRec2020,
}

fn frame_of(path: &Path, photo: &Photo) -> Frame {
    Frame {
        path: path.to_path_buf(),
        captured_ms: photo.captured.as_deref().and_then(parse_local_ms),
        exposure: exact_exposure(path),
    }
}

/// Bytes read from the start of a file for its metadata; almost every format keeps EXIF there.
const HEADER_BYTES: u64 = 4 << 20;

/// The camera settings from the file's EXIF, exactly as recorded.
///
/// Not the catalog's `Meta::shutter`: that is display text, rounded (1.6 s and 1.3 s read `2"` and
/// `1"`), which would turn every third-stop step of a ramp into a whole stop.
#[must_use]
pub fn exact_exposure(path: &Path) -> Option<Exposure> {
    use std::io::Read;
    let read = |limit: Option<u64>| -> Option<Vec<u8>> {
        let file = std::fs::File::open(path).ok()?;
        let mut bytes = Vec::new();
        match limit {
            Some(n) => file.take(n).read_to_end(&mut bytes).ok()?,
            None => (&file).read_to_end(&mut bytes).ok()?,
        };
        Some(bytes)
    };
    let from = |bytes: &[u8]| {
        let m = lightcraft_meta::try_extract(bytes).ok()?;
        Exposure::new(m.exposure_time?, m.f_number?, f64::from(m.iso?)).ok()
    };
    let header = read(Some(HEADER_BYTES))?;
    let whole_file = (header.len() as u64) < HEADER_BYTES;
    from(&header).or_else(|| {
        if whole_file {
            None
        } else {
            read(None).as_deref().and_then(from)
        }
    })
}

/// Whether a TIFF-based raw (ARW, NEF, DNG, CR2, ORF, RW2, PEF …) is cut short: its image data
/// (strips or tiles, and the embedded preview) reach past the end of the file. Only IFD0's chain and
/// its sub-IFDs are read; a file that is not TIFF-based, or not read cleanly, is not called cut short.
#[must_use]
pub fn cut_short(path: &Path) -> bool {
    tiff_data_end(path).is_some_and(|(end, len)| end > len)
}

/// The end of a TIFF file's image data and the file's length.
fn tiff_data_end(path: &Path) -> Option<(u64, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok().filter(std::fs::Metadata::is_file)?.len();
    let mut at = |offset: u64, n: usize| -> Option<Vec<u8>> {
        if offset.checked_add(n as u64)? > len {
            return None;
        }
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut b = vec![0; n];
        file.read_exact(&mut b).ok()?;
        Some(b)
    };
    let head = at(0, 8)?;
    let little = match head.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |b: &[u8], i: usize| -> Option<u16> {
        let v: [u8; 2] = b.get(i..i + 2)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(v)
        } else {
            u16::from_be_bytes(v)
        })
    };
    let u32_at = |b: &[u8], i: usize| -> Option<u32> {
        let v: [u8; 4] = b.get(i..i + 4)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(v)
        } else {
            u32::from_be_bytes(v)
        })
    };
    // The values of one entry (SHORT or LONG), inline or where it points.
    let values = |entry: &[u8], at: &mut dyn FnMut(u64, usize) -> Option<Vec<u8>>| -> Option<Vec<u64>> {
        let (kind, count) = (u16_at(entry, 2)?, u32_at(entry, 4)? as usize);
        let size = match kind {
            3 => 2,
            4 | 13 => 4,
            _ => return None,
        };
        if count == 0 || count > 1 << 20 {
            return None;
        }
        let data = if size * count <= 4 {
            entry.get(8..12)?.to_vec()
        } else {
            at(u64::from(u32_at(entry, 8)?), size * count)?
        };
        (0..count)
            .map(|i| {
                if size == 2 {
                    u16_at(&data, i * 2).map(u64::from)
                } else {
                    u32_at(&data, i * 4).map(u64::from)
                }
            })
            .collect()
    };
    let mut queue = vec![u64::from(u32_at(&head, 4)?)];
    let mut seen = std::collections::BTreeSet::new();
    let mut end = 0u64;
    let mut pairs = 0usize;
    while let Some(ifd) = queue.pop() {
        if ifd < 8 || !seen.insert(ifd) || seen.len() > 64 {
            continue;
        }
        let n = usize::from(u16_at(&at(ifd, 2)?, 0)?);
        if n == 0 || n > 1000 {
            return None;
        }
        let entries = at(ifd + 2, n * 12 + 4)?;
        let mut tags: BTreeMap<u16, Vec<u64>> = BTreeMap::new();
        for e in entries.get(..n * 12)?.as_chunks::<12>().0 {
            let tag = u16_at(e, 0)?;
            if matches!(tag, 273 | 279 | 324 | 325 | 513 | 514 | 330)
                && let Some(v) = values(e, &mut at)
            {
                tags.insert(tag, v);
            }
        }
        for (offsets, counts) in [(273, 279), (324, 325), (513, 514)] {
            if let (Some(o), Some(c)) = (tags.get(&offsets), tags.get(&counts))
                && o.len() == c.len()
            {
                pairs += 1;
                end = o.iter().zip(c).map(|(o, c)| o + c).fold(end, u64::max);
            }
        }
        queue.extend(tags.get(&330).into_iter().flatten());
        let next = u64::from(u32_at(&entries, n * 12)?);
        if next != 0 {
            queue.push(next);
        }
    }
    (pairs > 0).then_some((end, len))
}

/// Linear sensor brightness of a raw file in stops: the mean log2 of its normalized samples
/// (black 0, white 1), leaving out clipped and black samples. No demosaic, white balance, look or tone
/// curve touches it, so it shows what the sensor recorded: what camera exposure steps are checked
/// against ([`lapsewerk_core::exposure::verified_compensation`]). `None` for files that are not raws or
/// have too few usable samples.
#[must_use]
pub fn raw_brightness(path: &Path) -> Option<f64> {
    // A plain file of a photo's size at most (a pipe would never end).
    std::fs::metadata(path)
        .ok()
        .filter(|m| m.is_file() && m.len() <= 1 << 30)?;
    let bytes = std::fs::read(path).ok()?;
    lightcraft_raw::probe(&bytes)?;
    let raw = lightcraft_raw::decode(&bytes).ok()?;
    drop(bytes);
    let n = raw.normalized().ok()?;
    // About a million samples is plenty for a mean; an odd stride alternates over the mosaic colours.
    let stride = (n.data.len() / 1_000_000).max(1) | 1;
    let (mut sum, mut used, mut seen) = (0.0f64, 0usize, 0usize);
    for &v in n.data.iter().step_by(stride) {
        seen += 1;
        if v > 1e-4 && v < 0.98 {
            sum += f64::from(v.log2());
            used += 1;
        }
    }
    (used * 100 >= seen && used > 0).then(|| sum / used as f64)
}

/// Lone bright raw samples of one frame ([`lapsewerk_core::defects::spikes`]): positions in the raw's
/// sample data, with the mosaic's width and height (lists from frames of another size do not
/// combine). `None` for files that are not single-sample mosaics with a colour filter.
#[must_use]
pub fn raw_spikes(path: &Path, min_excess: f32) -> Option<(usize, usize, Vec<usize>)> {
    let bytes = std::fs::read(path).ok()?;
    lightcraft_raw::probe(&bytes)?;
    let raw = lightcraft_raw::decode(&bytes).ok()?;
    drop(bytes);
    let cfa = raw.cfa.as_ref()?;
    if raw.cpp != 1 {
        return None;
    }
    let lightcraft_raw::RawData::U16(data) = &raw.data else {
        return None;
    };
    let colour = |x: usize, y: usize| cfa.color_at(x, y);
    let a = raw.active_area;
    let m = lapsewerk_core::defects::Mosaic {
        width: raw.width,
        height: raw.height,
        data,
        colour: &colour,
        black: raw.black.mean(),
        white: raw.white.iter().copied().fold(0.0, f32::max),
        area: (a.x, a.y, a.width, a.height),
    };
    Some((
        raw.width,
        raw.height,
        lapsewerk_core::defects::spikes(&m, min_excess),
    ))
}

/// Hot pixels of a sequence: lone bright raw samples ([`raw_spikes`]) found in at least half of the
/// `frames` checked (pick frames with the longest exposures, where hot pixels show, spread out so
/// stars have moved between them). Returns the positions and how many frames could be checked.
#[must_use]
pub fn hot_pixels(paths: &[&Path], min_excess: f32) -> (Vec<usize>, usize) {
    let found: Vec<(usize, usize, Vec<usize>)> =
        paths.iter().filter_map(|p| raw_spikes(p, min_excess)).collect();
    // Only frames of the most common mosaic size (a sequence from one camera mode).
    let mut sizes: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for (w, h, _) in &found {
        *sizes.entry((*w, *h)).or_default() += 1;
    }
    let Some((&size, _)) = sizes.iter().max_by_key(|(_, n)| **n) else {
        return (Vec::new(), 0);
    };
    let lists: Vec<Vec<usize>> = found
        .into_iter()
        .filter(|(w, h, _)| (*w, *h) == size)
        .map(|(_, _, l)| l)
        .collect();
    (lapsewerk_core::defects::persistent(&lists, 0.5), lists.len())
}

/// `YYYY-MM-DDTHH:MM:SS[.fff…]` (anything after the seconds' fraction, such as a zone, ignored) as
/// milliseconds since 1970-01-01T00:00:00, read as if it were UTC.
#[must_use]
pub fn parse_local_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    if b.len() < 19 || b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') || b.get(13) != Some(&b':') {
        return None;
    }
    if !matches!(b.get(10), Some(b'T' | b' ')) {
        return None;
    }
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut ms = 0;
    if b.get(19) == Some(&b'.') {
        let digits: String = s
            .get(20..)?
            .chars()
            .take_while(char::is_ascii_digit)
            .take(3)
            .collect();
        if !digits.is_empty() {
            let scale = 10i64.pow(3 - digits.len() as u32);
            ms = digits.parse::<i64>().ok()? * scale;
        }
    }
    let days = days_from_civil(y, mo, d);
    Some(((days * 24 + h) * 60 + mi) * 60_000 + sec * 1000 + ms)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Whether an XMP text holds crs develop values (as other raw editors write them) (`crs:Exposure2012="…"` as an
/// attribute or `<crs:Exposure2012>` as an element), not just the namespace and version marks.
fn has_crs_settings(text: &str) -> bool {
    const MARKS: [&str; 3] = ["Version", "ProcessVersion", "HasSettings"];
    text.match_indices("crs:").any(|(at, _)| {
        if text
            .get(..at)
            .is_some_and(|before| before.ends_with("xmlns:") || before.ends_with("</"))
        {
            return false;
        }
        let rest = text.get(at + 4..).unwrap_or("");
        let name: String = rest.chars().take_while(char::is_ascii_alphanumeric).collect();
        !name.is_empty() && !MARKS.contains(&name.as_str())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(unix)]
    fn a_photo_turned_into_a_pipe_is_refused_not_waited_for() {
        let paths = frames("fifo", 2);
        let mut seq = super::Sequence::open(&paths).unwrap();
        std::fs::remove_file(&paths[1]).unwrap();
        let made = std::process::Command::new("mkfifo").arg(&paths[1]).status();
        if !made.is_ok_and(|s| s.success()) {
            return;
        }
        let settings = seq.settings(1).unwrap();
        let e = seq
            .render_job(1, settings, 64, 64, super::Output::Preview8)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(e.contains("no longer a plain file"), "{e}");
        assert!(super::raw_brightness(&paths[1]).is_none());
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn crs_values_count_marks_do_not() {
        let marks = r#"xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Version="17.0" crs:ProcessVersion="15.4" crs:HasSettings="True""#;
        assert!(!super::has_crs_settings(marks));
        assert!(super::has_crs_settings(&format!(
            "{marks} crs:Exposure2012=\"+0.50\""
        )));
        assert!(super::has_crs_settings("<crs:Temperature>5600</crs:Temperature>"));
        assert!(!super::has_crs_settings("<lc:settings>{}</lc:settings>"));
    }

    use super::*;

    #[test]
    fn parses_capture_times() {
        assert_eq!(parse_local_ms("1970-01-01T00:00:00"), Some(0));
        assert_eq!(parse_local_ms("1970-01-02 00:00:01.5"), Some(86_401_500));
        assert_eq!(parse_local_ms("2000-03-01T00:00:00+02:00"), Some(951_868_800_000));
        assert_eq!(
            parse_local_ms("2025-09-11T19:23:45.123456"),
            Some(1_757_618_625_123)
        );
        assert_eq!(parse_local_ms("2025-13-11T19:23:45"), None);
        assert_eq!(parse_local_ms("2025-09-11"), None);
        assert_eq!(parse_local_ms("2025-09-11T19:2x:45"), None);
        assert_eq!(parse_local_ms("ääää-09-11T19:23:45"), None);
    }

    #[test]
    fn never_binned_below_the_needed_size() {
        // The loader's rule: bin by k when long / k >= max_edge · 9 / 10.
        let accepted = |long: usize, max_edge: usize| {
            [8usize, 6, 4, 3, 2]
                .into_iter()
                .find(|&k| long / k >= max_edge * 9 / 10)
        };
        for long in [6000usize, 7008, 8256, 9504] {
            for needed in (200..long).step_by(37) {
                let max_edge = no_binning_below(needed, long);
                assert!(max_edge >= needed);
                if let Some(k) = accepted(long, max_edge) {
                    assert!(
                        long / k >= needed,
                        "long {long}, needed {needed}: binned by {k} to {}",
                        long / k
                    );
                }
            }
        }
        assert_eq!(no_binning_below(3847, 7008), 3895);
        assert_eq!(no_binning_below(1922, 7008), 1948);
        assert_eq!(no_binning_below(1700, 7008), 1700);
    }

    /// A folder of `n` copies of a small JPEG, unique to `name` and this process.
    fn frames(name: &str, n: usize) -> Vec<PathBuf> {
        let dir = std::env::temp_dir().join(format!("lapsewerk-develop-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (0..n)
            .map(|i| {
                let p = dir.join(format!("f{i:03}.jpg"));
                // LightCraft imports identical files once: bytes after the JPEG's end make each one unique.
                let mut bytes = include_bytes!("../tests/data/frame.jpg").to_vec();
                bytes.extend_from_slice(format!("frame {i}").as_bytes());
                std::fs::write(&p, bytes).unwrap();
                p
            })
            .collect()
    }

    #[test]
    fn a_sidecar_without_crs_values_prints_as_in_earlier_versions() {
        // Projects keep the fingerprints of their automatic keyframes: without crs values the
        // text is the settings alone, as before (else every automatic keyframe became "edited").
        let paths = frames("legacy-print", 2);
        let mut seq = Sequence::open(&paths).unwrap();
        let d = seq.settings(0).unwrap();
        seq.save_keyframe(0, d).unwrap();
        let text = seq.sidecar_develop_text(0).unwrap();
        let parsed = lightcraft_engine::sidecar::parse_sidecar(
            &std::fs::read_to_string(seq.sidecar(0).unwrap()).unwrap(),
            lightcraft_engine::crs::Target::Any,
        )
        .unwrap();
        assert_eq!(text, format!("{:?}", parsed.develop));
        // Another editor's crs values beside: they count.
        let side = seq.sidecar(0).unwrap();
        let xmp = std::fs::read_to_string(&side).unwrap();
        let edited = xmp.replacen(
            "<rdf:Description ",
            "<rdf:Description crs:Exposure2012=\"+0.50\" ",
            1,
        );
        std::fs::write(&side, edited).unwrap();
        assert_ne!(seq.sidecar_develop_text(0).unwrap(), text);
    }

    #[test]
    fn settings_a_newer_lightcraft_wrote_are_named() {
        let paths = frames("unknown-settings", 2);
        let mut seq = Sequence::open(&paths).unwrap();
        let mut d = seq.settings(0).unwrap();
        // Values away from their defaults: written, and known.
        d.light.exposure = 0.3;
        seq.save_keyframe(0, d).unwrap();
        assert!(seq.unknown_settings().is_empty(), "{:?}", seq.unknown_settings());
        let side = seq.sidecar(0).unwrap();
        let xmp = std::fs::read_to_string(&side).unwrap();
        let start = xmp.find("<lc:settings>").unwrap() + "<lc:settings>".len();
        let edited = format!(
            "{}{{\"future_tool\":{{\"amount\":1}},{}",
            &xmp[..start],
            &xmp[start + 1..]
        )
        .replacen("\"light\":{", "\"light\":{\"future_slider\":5,", 1);
        std::fs::write(&side, edited).unwrap();
        let unknown = seq.unknown_settings();
        assert_eq!(unknown.len(), 1, "{unknown:?}");
        let (frame, names) = &unknown[0];
        assert_eq!(*frame, 0);
        assert!(names.contains(&"future_tool".to_owned()), "{names:?}");
        assert!(names.contains(&"light.future_slider".to_owned()), "{names:?}");
    }

    #[test]
    fn writes_new_keyframes_and_never_touches_existing_sidecars() {
        let paths = frames("keyframes", 4);
        // Frame 1 has a sidecar from another program: not LightCraft's, and it must stay as it is.
        let foreign = paths[1].with_extension("xmp");
        let theirs = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:other="http://example.com/other/1.0/" other:Note="someone's"/></rdf:RDF></x:xmpmeta>"#;
        std::fs::write(&foreign, theirs).unwrap();
        let originals: Vec<Vec<u8>> = paths.iter().map(|p| std::fs::read(p).unwrap()).collect();

        let mut seq = Sequence::open(&paths).unwrap();
        let plan = seq.plan(Interpolation::Smooth).unwrap();
        let done = seq.write_keyframes(&plan, &[0, 1, 3]).unwrap();
        assert!(
            matches!(&done[0], KeyframeWrite::Written { frame: 0, sidecar } if sidecar == &paths[0].canonicalize().unwrap().with_extension("xmp"))
        );
        assert!(matches!(&done[1], KeyframeWrite::Existing { frame: 1, .. }));
        assert!(matches!(&done[2], KeyframeWrite::Written { frame: 3, .. }));
        assert_eq!(std::fs::read_to_string(&foreign).unwrap(), theirs);
        let xmp = std::fs::read_to_string(paths[3].with_extension("xmp")).unwrap();
        assert!(xmp.contains("lc:settings"), "{xmp}");
        for (p, before) in paths.iter().zip(&originals) {
            assert_eq!(&std::fs::read(p).unwrap(), before, "original changed");
        }

        // Again: nothing is overwritten.
        let again = seq.write_keyframes(&plan, &[0, 3]).unwrap();
        assert!(again.iter().all(|w| matches!(w, KeyframeWrite::Existing { .. })));
        assert!(matches!(seq.write_keyframes(&plan, &[4]), Err(Error::NoFrame(4))));

        // Opened anew, the written frames are keyframes with the settings they were given.
        let reopened = Sequence::open(&paths).unwrap();
        assert_eq!(reopened.keyframe_frames(), vec![0, 1, 3]);
        let new_plan = reopened.plan(Interpolation::Smooth).unwrap();
        let (a, b) = (plan.settings(3, 0.0), new_plan.settings(3, 0.0));
        assert!((a.light.exposure - b.light.exposure).abs() < 1e-9);
        assert!(
            (a.wb.temp - b.wb.temp).abs() < 1e-6,
            "{} vs {}",
            a.wb.temp,
            b.wb.temp
        );
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn a_saved_keyframe_holds_its_settings() {
        let paths = frames("save", 3);
        let mut seq = Sequence::open(&paths).unwrap();
        let plan = seq.plan(Interpolation::Smooth).unwrap();
        let mut edited = plan.settings(2, 0.0);
        assert!(controls::set(&mut edited, "light.exposure", 1.25));
        assert!(controls::set(&mut edited, "light.contrast", 30.0));
        let path = seq.save_keyframe(2, edited).unwrap();
        assert!(path.ends_with("f002.xmp"));
        assert!(matches!(
            seq.save_keyframe(7, DevelopSettings::default()),
            Err(Error::NoFrame(7))
        ));
        let reopened = Sequence::open(&paths).unwrap();
        assert_eq!(reopened.keyframe_frames(), vec![2]);
        let k = reopened.plan(Interpolation::Smooth).unwrap().settings(2, 0.0);
        assert_eq!(controls::get(&k, "light.exposure"), Some(1.25));
        assert_eq!(controls::get(&k, "light.contrast"), Some(30.0));
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn a_graduated_filter_fades_between_keyframes() {
        let paths = frames("mask", 5);
        let mut seq = Sequence::open(&paths).unwrap();
        let plan = seq.plan(Interpolation::Linear).unwrap();
        // Frame 0: a sky filter, two stops down; frame 4: none.
        let mut first = plan.settings(0, 0.0);
        first.masks.push(masks::sky_gradient(0.0, 0.5, -2.0));
        seq.save_keyframe(0, first).unwrap();
        seq.save_keyframe(4, plan.settings(4, 0.0)).unwrap();
        let reopened = Sequence::open(&paths).unwrap();
        let plan = reopened.plan(Interpolation::Linear).unwrap();
        let exposure = |f: usize| plan.settings(f, 0.0).masks.first().map(|m| m.adjust.exposure);
        assert_eq!(exposure(0), Some(-2.0), "the mask is kept in the sidecar");
        let mid = exposure(2).unwrap();
        assert!((mid + 1.0).abs() < 1e-6, "{mid}");
        assert_eq!(exposure(4), Some(0.0));
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn a_sky_gradient_darkens_the_top_only() {
        let paths = frames("maskrender", 1);
        let mut seq = Sequence::open(&paths).unwrap();
        let plan = seq.plan(Interpolation::Linear).unwrap();
        let plain = plan.settings(0, 0.0);
        let mut graded = plain.clone();
        graded.masks.push(masks::sky_gradient(0.0, 0.5, -2.0));
        let mut render = |d: DevelopSettings| {
            let r = seq.render_job(0, d, 240, 240, Output::Preview8).unwrap().run();
            let img = r.rendered.unwrap().image;
            (img.width, img.height, img.data)
        };
        let (a, b) = (render(plain), render(graded));
        // Mean of a row band (fraction of the height), all channels.
        let band = |img: &(usize, usize, Vec<[u8; 4]>), from: f64, to: f64| {
            let (w, h, data) = img;
            let (y0, y1) = ((*h as f64 * from) as usize, (*h as f64 * to) as usize);
            let px: Vec<f64> = (y0..y1)
                .flat_map(|y| (0..*w).map(move |x| y * w + x))
                .filter_map(|i| data.get(i))
                .map(|p| f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2]))
                .collect();
            px.iter().sum::<f64>() / px.len().max(1) as f64
        };
        assert!(band(&b, 0.0, 0.1) < band(&a, 0.0, 0.1) * 0.8, "the top is darker");
        assert!(
            (band(&b, 0.7, 1.0) - band(&a, 0.7, 1.0)).abs() < 2.0,
            "the bottom is not"
        );
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn every_mask_kind_renders_and_acts() {
        let paths = frames("maskkinds", 1);
        let mut seq = Sequence::open(&paths).unwrap();
        let plan = seq.plan(Interpolation::Linear).unwrap();
        let plain = plan.settings(0, 0.0);
        let mut render = |d: DevelopSettings| {
            let r = seq.render_job(0, d, 160, 160, Output::Preview8).unwrap().run();
            r.rendered.unwrap().image.data
        };
        let base = render(plain.clone());
        for kind in [
            masks::Kind::Linear,
            masks::Kind::Radial,
            masks::Kind::Sky,
            masks::Kind::Luminance,
        ] {
            let mut d = plain.clone();
            let mut m = masks::new_mask(kind, "m");
            m.adjust.exposure = -2.0;
            // The brightness range over everything, so the test image surely has some.
            if kind == masks::Kind::Luminance {
                d.masks.push(m);
                assert!(masks::set(&mut d, "mask.0.lumLo", 0.0));
            } else {
                d.masks.push(m);
            }
            let out = render(d);
            assert_eq!(out.len(), base.len());
            let changed = out.iter().zip(&base).filter(|(a, b)| a != b).count();
            if kind != masks::Kind::Sky {
                assert!(
                    changed > out.len() / 50,
                    "{kind:?}: {changed} of {} pixels changed",
                    out.len()
                );
            }
        }
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    /// A landscape JPEG: bright blue sky above `horizon` (0..1 down), dark ground below.
    fn landscape(path: &Path, horizon: f64) {
        landscape_in(path, horizon, [150, 190, 245], [30, 45, 25]);
    }

    /// A landscape JPEG of these sky and ground colours (sRGB).
    fn landscape_in(path: &Path, horizon: f64, sky_rgb: [u8; 3], ground_rgb: [u8; 3]) {
        use lightcraft_codecs::encode::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
        let (w, h) = (192usize, 128usize);
        let mut px = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for _ in 0..w {
                let sky = (y as f64) < horizon * h as f64;
                px.extend_from_slice(if sky { &sky_rgb } else { &ground_rgb });
            }
        }
        let img = EncodeImage::new(w as u32, h as u32, 3, Samples::U8(&px));
        let jpeg = encode_jpeg(&img, 95, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
        std::fs::write(path, jpeg).unwrap();
    }

    #[test]
    fn the_sky_mask_follows_each_frame() {
        let dir = std::env::temp_dir().join(format!("lapsewerk-develop-sky-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The horizon moves down from frame 0 to frame 1 (a tilt, or the camera's scene changing).
        let paths: Vec<PathBuf> = [0.35, 0.6]
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let p = dir.join(format!("s{i}.jpg"));
                landscape(&p, *h);
                p
            })
            .collect();
        let mut seq = Sequence::open(&paths).unwrap();
        let plan = seq.plan(Interpolation::Linear).unwrap();
        // Mean brightness of rows 42-52 % down, with and without a sky mask two stops down.
        let mut band = |frame: usize, sky: bool| {
            let mut d = plan.settings(frame, 0.0);
            if sky {
                let mut m = masks::new_mask(masks::Kind::Sky, "sky");
                m.adjust.exposure = -2.0;
                d.masks.push(m);
            }
            let img = seq
                .render_job(frame, d, 192, 192, Output::Preview8)
                .unwrap()
                .run()
                .rendered
                .unwrap()
                .image;
            let (y0, y1) = (img.height * 42 / 100, img.height * 52 / 100);
            let px: Vec<f64> = (y0..y1)
                .flat_map(|y| (0..img.width).map(move |x| (y, x)))
                .filter_map(|(y, x)| img.data.get(y * img.width + x))
                .map(|p| f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2]))
                .collect();
            px.iter().sum::<f64>() / px.len().max(1) as f64
        };
        // Frame 0: those rows are ground, the mask leaves them; frame 1: they are sky, darkened.
        let (ground, ground_masked) = (band(0, false), band(0, true));
        let (sky, sky_masked) = (band(1, false), band(1, true));
        assert!(
            (ground - ground_masked).abs() < ground * 0.1 + 3.0,
            "ground {ground} → {ground_masked}"
        );
        assert!(sky_masked < sky * 0.85, "sky {sky} → {sky_masked}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn masks_blend_by_name_not_by_order() {
        let paths = frames("maskorder", 5);
        let mut seq = Sequence::open(&paths).unwrap();
        let named = |name: &str, ev: f64| {
            let mut m = masks::new_mask(masks::Kind::Linear, name);
            m.adjust.exposure = ev;
            m
        };
        // Keyframe 0: [sky -1, ground +1]; keyframe 4: the same masks the other way round, and a
        // third only there.
        let mut first = seq.settings(0).unwrap();
        first.masks = vec![named("sky", -1.0), named("ground", 1.0)];
        seq.save_keyframe(0, first).unwrap();
        let mut last = seq.settings(4).unwrap();
        last.masks = vec![named("ground", 1.0), named("sky", -1.0), named("sun", 2.0)];
        seq.save_keyframe(4, last).unwrap();
        let plan = seq.plan(Interpolation::Linear).unwrap();
        let ev = |d: &DevelopSettings, name: &str| {
            d.masks.iter().find(|m| m.name == name).map(|m| m.adjust.exposure)
        };
        let mid = plan.settings(2, 0.0);
        // Each mask keeps its own exposure through the middle (by order, sky and ground would meet at 0).
        assert!(
            (ev(&mid, "sky").unwrap() + 1.0).abs() < 1e-9,
            "{:?}",
            ev(&mid, "sky")
        );
        assert!((ev(&mid, "ground").unwrap() - 1.0).abs() < 1e-9);
        // The sun fades in: none at the first keyframe, half way at the middle, full at the last.
        assert!(ev(&plan.settings(0, 0.0), "sun").unwrap().abs() < 1e-9);
        assert!((ev(&mid, "sun").unwrap() - 1.0).abs() < 1e-9);
        assert!((ev(&plan.settings(4, 0.0), "sun").unwrap() - 2.0).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn untouched_keyframes_follow_the_edited_ones() {
        let paths = frames("autotransition", 5);
        let mut seq = Sequence::open(&paths).unwrap();
        // Keyframes 0, 2, 4 made; then 0 and 4 edited (day +1, night -1), 2 left as made.
        for f in [0, 2, 4] {
            let d = seq.settings(f).unwrap();
            seq.save_keyframe(f, d).unwrap();
        }
        let mut day = seq.settings(0).unwrap();
        day.light.exposure = 1.0;
        seq.save_keyframe(0, day).unwrap();
        let mut night = seq.settings(4).unwrap();
        night.light.exposure = -1.0;
        seq.save_keyframe(4, night).unwrap();
        // Without the auto transition, frame 2 pulls the middle back to 0 (it is anyway); with
        // it, frame 2 takes the blend of its neighbours and the curve runs straight through.
        assert_eq!(seq.auto_transition(Interpolation::Linear, &[2]).unwrap(), [2]);
        let plan = seq.plan(Interpolation::Linear).unwrap();
        assert!(plan.settings(2, 0.0).light.exposure.abs() < 1e-9);
        assert!((plan.settings(1, 0.0).light.exposure - 0.5).abs() < 1e-9);
        assert!((plan.settings(3, 0.0).light.exposure + 0.5).abs() < 1e-9);
        // Night edited to -2: the untouched middle follows to -0.5.
        let mut night = seq.settings(4).unwrap();
        night.light.exposure = -2.0;
        seq.save_keyframe(4, night).unwrap();
        seq.auto_transition(Interpolation::Linear, &[2]).unwrap();
        let plan = seq.plan(Interpolation::Linear).unwrap();
        assert!((plan.settings(2, 0.0).light.exposure + 0.5).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn point_curves_blend_between_keyframes() {
        // An S-curve at the first keyframe, none at the last: halfway, half the S; at the
        // keyframes themselves, exactly theirs (it jumped at the next keyframe before).
        let paths = frames("curve-blend", 5);
        let mut seq = Sequence::open(&paths).unwrap();
        let mut first = seq.settings(0).unwrap();
        first.curve.master = vec![
            lightcraft_geom::Point::new(0.0, 0.0),
            lightcraft_geom::Point::new(0.25, 0.15),
            lightcraft_geom::Point::new(0.75, 0.85),
            lightcraft_geom::Point::new(1.0, 1.0),
        ];
        seq.save_keyframe(0, first.clone()).unwrap();
        let last = seq.settings(4).unwrap();
        seq.save_keyframe(4, last).unwrap();
        let plan = seq.plan(Interpolation::Linear).unwrap();
        let curve = |f: usize| plan.settings(f, 0.0).curve.master;
        assert_eq!(curve(0), first.curve.master);
        let y =
            |pts: &[lightcraft_geom::Point], x: f64| pts.iter().find(|p| (p.x - x).abs() < 1e-9).map(|p| p.y);
        assert!(
            (y(&curve(2), 0.25).unwrap() - 0.20).abs() < 1e-9,
            "{:?}",
            curve(2)
        );
        assert!((y(&curve(2), 0.75).unwrap() - 0.80).abs() < 1e-9);
        assert!(ToneCurveIdentity::is(&curve(4)));
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn a_curve_beside_its_keyframe_looks_like_the_keyframe() {
        // Different points (an S with 4, a gentler one with 3): a frame a hair from the S
        // keyframe is drawn (LightCraft's spline) as the S, everywhere, not only at its points.
        use lightcraft_color::spline::MonotoneCurve;
        use lightcraft_geom::Point;
        let s = vec![
            Point::new(0.0, 0.0),
            Point::new(0.2, 0.08),
            Point::new(0.8, 0.92),
            Point::new(1.0, 1.0),
        ];
        let g = vec![Point::new(0.0, 0.0), Point::new(0.35, 0.4), Point::new(1.0, 1.0)];
        let draw = |pts: &[Point]| MonotoneCurve::new(&pts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>());
        let near = blend_points(&s, &g, 0.01);
        let (want, got) = (draw(&s), draw(&near));
        let worst = (0..=100)
            .map(|k| f64::from(k) / 100.0)
            .map(|x| (want.eval(x) - got.eval(x)).abs())
            .fold(0.0f64, f64::max);
        assert!(worst < 0.01, "{worst}");
    }

    /// Whether a point curve is the straight line.
    struct ToneCurveIdentity;
    impl ToneCurveIdentity {
        fn is(pts: &[lightcraft_geom::Point]) -> bool {
            pts.is_empty() || pts.iter().all(|p| (p.x - p.y).abs() < 1e-9)
        }
    }

    #[test]
    fn only_keyframes_between_edited_ones_follow() {
        // Only the night edited: the day's automatic keyframe keeps its own look (it took the
        // night's +2 and burnt the whole day out).
        let paths = frames("between-only", 5);
        let mut seq = Sequence::open(&paths).unwrap();
        for f in [0, 2, 4] {
            let d = seq.settings(f).unwrap();
            seq.save_keyframe(f, d).unwrap();
        }
        let day = seq.settings(0).unwrap().light.exposure;
        let mut night = seq.settings(4).unwrap();
        night.light.exposure = 2.0;
        seq.save_keyframe(4, night).unwrap();
        let done = seq.auto_transition(Interpolation::Linear, &[0, 2]).unwrap();
        assert!(done.is_empty(), "{done:?}");
        assert!((seq.settings(0).unwrap().light.exposure - day).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn the_transition_follows_the_light() {
        let paths = frames("along-light", 5);
        let mut seq = Sequence::open(&paths).unwrap();
        for f in [0, 1, 4] {
            let d = seq.settings(f).unwrap();
            seq.save_keyframe(f, d).unwrap();
        }
        let mut day = seq.settings(0).unwrap();
        day.light.exposure = 0.0;
        seq.save_keyframe(0, day).unwrap();
        let mut night = seq.settings(4).unwrap();
        night.light.exposure = -2.0;
        seq.save_keyframe(4, night).unwrap();
        // The light falls by 4 stops between frames 0 and 1, then hardly: frame 1 already stands
        // at (nearly) the night's settings, not at a quarter of the way.
        let light = [0.0, -4.0, -4.1, -4.2, -4.3];
        seq.auto_transition_along(Interpolation::Linear, &[1], Some(&light))
            .unwrap();
        let e = seq.settings(1).unwrap().light.exposure;
        assert!(e < -1.5, "{e}");
        // By frame number: a quarter of the way.
        seq.auto_transition(Interpolation::Linear, &[1]).unwrap();
        let e = seq.settings(1).unwrap().light.exposure;
        assert!((e + 0.5).abs() < 1e-6, "{e}");
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn a_damaged_sidecar_is_told() {
        let paths = frames("unreadable-sidecar", 3);
        {
            let mut seq = Sequence::open(&paths).unwrap();
            let d = seq.settings(0).unwrap();
            seq.save_keyframe(0, d).unwrap();
        }
        std::fs::write(paths[2].with_extension("xmp"), b"<x:xmpmeta broken").unwrap();
        let seq = Sequence::open(&paths).unwrap();
        assert_eq!(seq.keyframe_frames(), [0, 2]);
        assert_eq!(seq.unreadable_sidecars(), [2]);
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn masks_are_copied_one_by_one() {
        let named = |name: &str, ev: f64| {
            let mut m = masks::new_mask(masks::Kind::Linear, name);
            m.adjust.exposure = ev;
            m
        };
        let from = DevelopSettings {
            masks: vec![named("Himmel", -1.0), named("Sonne", 0.5), named("Neu", 0.2)],
            ..Default::default()
        };
        let mut to = DevelopSettings {
            masks: vec![named("Sonne", 0.0), named("Wasser", 0.3)],
            ..Default::default()
        };
        // The sun and the new one: the sun replaced in its place, the new one added, water kept.
        copy_masks(&from, &mut to, &[1, 2, 9]);
        let got: Vec<(&str, f64)> = to
            .masks
            .iter()
            .map(|m| (m.name.as_str(), m.adjust.exposure))
            .collect();
        assert_eq!(got, [("Sonne", 0.5), ("Wasser", 0.3), ("Neu", 0.2)]);
    }

    /// A little-endian TIFF: IFD0 with a strip at 100 of `strip` bytes; `len` bytes long.
    fn tiff(strip: u32, len: usize) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[..8].copy_from_slice(&[b'I', b'I', 42, 0, 8, 0, 0, 0]);
        b[8..10].copy_from_slice(&2u16.to_le_bytes());
        let entry = |tag: u16, v: u32| {
            let mut e = Vec::new();
            e.extend(tag.to_le_bytes());
            e.extend(4u16.to_le_bytes());
            e.extend(1u32.to_le_bytes());
            e.extend(v.to_le_bytes());
            e
        };
        b[10..22].copy_from_slice(&entry(273, 100));
        b[22..34].copy_from_slice(&entry(279, strip));
        b
    }

    #[test]
    fn a_crop_switched_off_in_another_editor_is_still_read() {
        let xmp = |has: &str| {
            format!(
                r#"<rdf:Description tiff:Orientation="8" tiff:ImageWidth="7008" tiff:ImageLength="4672" crs:HasCrop="{has}" crs:CropTop="0" crs:CropLeft="0.075" crs:CropBottom="0.8" crs:CropRight="0.877" crs:CropAngle="0"/>"#
            )
        };
        // Turned (orientation 8): the stored 3:2 photo upright; left/right become bottom/top.
        let g = crop_switched_off(&xmp("False"), (4672, 7008)).unwrap();
        assert!((g.rect.x0 - 0.0).abs() < 1e-9 && (g.rect.x1 - 0.8).abs() < 1e-9);
        assert!((g.rect.y0 - 0.123).abs() < 1e-9 && (g.rect.y1 - 0.925).abs() < 1e-9);
        // On: LightCraft reads it; the whole photo: nothing to take.
        assert!(crop_switched_off(&xmp("True"), (4672, 7008)).is_none());
        let whole = r#"<x crs:HasCrop="False" crs:CropTop="0" crs:CropLeft="0" crs:CropBottom="1" crs:CropRight="1"/>"#;
        assert!(crop_switched_off(whole, (6000, 4000)).is_none());
        // As elements.
        let el = "<crs:HasCrop>False</crs:HasCrop><crs:CropTop>0.1</crs:CropTop><crs:CropLeft>0.2</crs:CropLeft><crs:CropBottom>0.9</crs:CropBottom><crs:CropRight>0.8</crs:CropRight>";
        let g = crop_switched_off(el, (6000, 4000)).unwrap();
        assert!((g.rect.x0 - 0.2).abs() < 1e-9 && (g.rect.y1 - 0.9).abs() < 1e-9);
    }

    #[test]
    fn every_frame_gets_the_video_shape() {
        use lightcraft_geom::{CropGeometry, Rect};
        let shape = |g: CropGeometry, w: f64, h: f64| {
            let r = g.rect_px(w, h);
            r.width() / r.height()
        };
        // Uncropped and too wide even when as tall as the photo: cut at the sides, centred.
        let g = video_crop(CropGeometry::default(), 4000.0, 6000.0, 9.0 / 16.0);
        assert!((shape(g, 4000.0, 6000.0) - 9.0 / 16.0).abs() < 1e-9);
        assert!((g.rect.center().x - 0.5).abs() < 1e-9 && (g.rect.height() - 1.0).abs() < 1e-9);
        // Taller than the video: cut at the top and bottom.
        let tall = CropGeometry {
            rect: Rect::new(0.4, 0.0, 0.6, 1.0),
            angle: 0.0,
        };
        let g = video_crop(tall, 4000.0, 6000.0, 9.0 / 16.0);
        assert!((shape(g, 4000.0, 6000.0) - 9.0 / 16.0).abs() < 1e-9);
        assert!((g.rect.width() - 0.2).abs() < 1e-9);
        // A plan without any crop is left alone.
        let plan = DevelopPlan {
            keyframes: vec![0],
            bases: vec![(0, DevelopSettings::default())],
            curves: Curves::default(),
            look: Vec::new(),
            crop_fit: None,
        }
        .with_crop_fit(9.0 / 16.0, (6000, 4000));
        assert!(plan.settings(0, 0.0).crop.geometry.is_identity());
    }

    #[test]
    fn a_raw_cut_short_is_found_by_its_data_past_the_end() {
        let dir = std::env::temp_dir().join(format!("lw-cut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.arw");
        std::fs::write(&p, tiff(900, 1000)).unwrap();
        assert!(!cut_short(&p));
        std::fs::write(&p, tiff(900, 600)).unwrap();
        assert!(cut_short(&p));
        // Not TIFF: not judged.
        std::fs::write(&p, b"FUJIFILMCCD-RAW 0201").unwrap();
        assert!(!cut_short(&p));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_photo_is_left_out_not_fatal() {
        let paths = frames("damaged", 3);
        let bytes = std::fs::read(&paths[1]).unwrap();
        std::fs::write(&paths[1], &bytes[..bytes.len() / 3]).unwrap();
        let seq = Sequence::open(&paths).unwrap();
        assert_eq!(seq.len(), 2);
        assert_eq!(seq.skipped().len(), 1);
        // Its place: before frame 1 of the two left, one photo.
        assert_eq!(seq.skipped_gaps(), [(1, 1)]);
        assert!(seq.skipped()[0].ends_with(paths[1].file_name().unwrap()));
        // Nothing readable at all: an error.
        assert!(Sequence::open(&paths[1..2]).is_err());
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn the_sky_mask_finds_a_dusk_sky() {
        // A deep-blue dusk sky (about 4.5 stops below white) over near-black land: LightCraft's own
        // heuristic selects nothing this dark; the matte from the frame's own light does.
        let dir = std::env::temp_dir().join(format!("lapsewerk-develop-dusk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dusk.jpg");
        landscape_in(&path, 0.5, [34, 44, 84], [3, 4, 3]);
        let mut seq = Sequence::open(std::slice::from_ref(&path)).unwrap();
        let plan = seq.plan(Interpolation::Linear).unwrap();
        let mut band = |sky: bool, from: usize, to: usize| {
            let mut d = plan.settings(0, 0.0);
            if sky {
                let mut m = masks::new_mask(masks::Kind::Sky, "sky");
                m.adjust.exposure = -1.5;
                d.masks.push(m);
            }
            let img = seq
                .render_job(0, d, 192, 192, Output::Preview8)
                .unwrap()
                .run()
                .rendered
                .unwrap()
                .image;
            let (y0, y1) = (img.height * from / 100, img.height * to / 100);
            let px: Vec<f64> = (y0..y1)
                .flat_map(|y| (0..img.width).map(move |x| (y, x)))
                .filter_map(|(y, x)| img.data.get(y * img.width + x))
                .map(|p| f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2]))
                .collect();
            px.iter().sum::<f64>() / px.len().max(1) as f64
        };
        let (sky, sky_masked) = (band(false, 5, 40), band(true, 5, 40));
        let (land, land_masked) = (band(false, 60, 95), band(true, 60, 95));
        assert!(sky_masked < sky * 0.85, "sky {sky} → {sky_masked}");
        assert!(
            (land - land_masked).abs() < land * 0.1 + 3.0,
            "land {land} → {land_masked}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sidecar_changed_elsewhere_is_read_again() {
        let paths = frames("reload", 3);
        let mut seq = Sequence::open(&paths).unwrap();
        assert_eq!(seq.sidecar(1), None);
        let plan = seq.plan(Interpolation::Linear).unwrap();
        seq.save_keyframe(1, plan.settings(1, 0.0)).unwrap();
        assert!(seq.sidecar(1).is_some());
        // Another program (LightCraft) edits the keyframe and saves its sidecar.
        let mut other = Sequence::open(&paths).unwrap();
        let mut d = other.settings(1).unwrap();
        controls::set(&mut d, "effects.clarity", 42.0);
        other.save_keyframe(1, d).unwrap();
        assert_eq!(
            controls::get(&seq.settings(1).unwrap(), "effects.clarity"),
            Some(0.0)
        );
        assert!(seq.reload_keyframe(1).unwrap());
        assert_eq!(
            controls::get(&seq.settings(1).unwrap(), "effects.clarity"),
            Some(42.0)
        );
        assert!(!seq.reload_keyframe(0).unwrap(), "no sidecar");
        assert!(seq.reload_keyframe(9).is_err());
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn auto_tone_and_white_balance_give_usable_values() {
        let paths = frames("auto", 1);
        let mut seq = Sequence::open(&paths).unwrap();
        let d = seq.settings(0).unwrap();
        let auto = seq.auto_tone(0, &d).unwrap();
        assert_eq!(auto.len(), 8);
        for (id, v) in &auto {
            let spec = controls::find(id).unwrap();
            assert!(v.is_finite() && *v >= spec.min && *v <= spec.max, "{id} {v}");
        }
        for spot in [None, Some((0.5, 0.5)), Some((f64::NAN, 2.0)), Some((-1.0, 9.0))] {
            let (t, tint) = seq.white_balance(0, &d, spot).unwrap();
            assert!(t.is_finite() && tint.is_finite(), "{spot:?}: {t} {tint}");
        }
        assert!(seq.auto_tone(5, &d).is_err());
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn copies_only_what_is_asked() {
        let mut from = DevelopSettings::default();
        controls::set(&mut from, "light.exposure", 1.0);
        controls::set(&mut from, "effects.clarity", 30.0);
        from.masks.push(masks::sky_gradient(0.0, 0.5, -1.0));
        let mut to = DevelopSettings::default();
        controls::set(&mut to, "light.exposure", -0.5);
        copy_settings(&from, &mut to, &["effects.clarity", "no.such"], false);
        assert_eq!(controls::get(&to, "effects.clarity"), Some(30.0));
        assert_eq!(controls::get(&to, "light.exposure"), Some(-0.5), "exposure stays");
        assert!(to.masks.is_empty());
        copy_settings(&from, &mut to, &[], true);
        assert_eq!(to.masks, from.masks);
    }

    #[test]
    fn a_copied_frame_is_named_as_a_copy() {
        let paths = frames("duplicate", 3);
        std::fs::copy(&paths[0], &paths[2]).unwrap();
        let err = Sequence::open(&paths).err().unwrap();
        assert_eq!(
            err,
            Error::Duplicate {
                path: paths[2].canonicalize().unwrap(),
                of: paths[0].canonicalize().unwrap()
            }
        );
        assert!(err.to_string().contains("the same file as"));
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    #[test]
    fn frame_settings_offsets_past_the_slider_range() {
        let base = DevelopSettings::default();
        let mut values = BTreeMap::new();
        values.insert("light.exposure".to_owned(), 4.0);
        let d = Sequence::frame_settings(&base, &values, 3.5);
        assert!((d.light.exposure - 7.5).abs() < 1e-9);
        let d = Sequence::frame_settings(&base, &values, f64::NAN);
        assert!((d.light.exposure - 4.0).abs() < 1e-9);
    }
}

#[cfg(test)]
mod send_probe {
    fn is_send<T: Send>() {}
    /// The app keeps the sequence on a worker thread and runs render jobs on others.
    #[test]
    fn sequence_and_jobs_move_between_threads() {
        is_send::<super::Sequence>();
        is_send::<super::DevelopPlan>();
        is_send::<lightcraft_engine::RenderJob>();
    }
}
