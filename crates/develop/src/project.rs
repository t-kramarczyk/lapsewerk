//! The project file: what an import learned about a sequence, kept beside its frames.
//!
//! `lapsewerk import <folder>` writes `lapsewerk.json` into the folder: the frames in order with
//! their capture times and camera settings, which ones are keyframes, the interval, gaps, frames out
//! of order and the camera's exposure steps (EXIF). Later runs compare it with the folder and say
//! which frames were added or removed since.
//!
//! The file is written atomically (a temporary file in the same folder, flushed to disk, then
//! renamed over the old one): an interrupted write leaves the previous project or none, never half a
//! file. Frames are stored by file name, relative to the folder, so the folder can be moved.

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use lapsewerk_core::exposure::{self, Exposure, StepGrid};
use lapsewerk_core::sequence::{self, Frame};
use serde_json::{Value, json};

/// Name of the project file in a sequence's folder.
pub const FILE_NAME: &str = "lapsewerk.json";

/// Version of the file's layout; a file of another version is not read.
pub const FORMAT: u64 = 1;

/// Largest project file read (a sequence of 100 000 frames needs about 20 MB).
const MAX_BYTES: u64 = 64 << 20;

/// One frame as the project records it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectFrame {
    /// File name in the sequence's folder.
    pub file: String,
    /// Capture time, milliseconds since 1970 (camera clock, no zone).
    pub captured_ms: Option<i64>,
    pub exposure: Option<Exposure>,
    /// Has an XMP sidecar at import.
    pub keyframe: bool,
}

/// What an import learned about a sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub frames: Vec<ProjectFrame>,
    /// Median time between frames (ms), when capture times are known and differ.
    pub interval_ms: Option<i64>,
    /// Frames after a pause of more than three intervals.
    pub gaps_before: Vec<usize>,
    /// Frames captured earlier than the frame before.
    pub out_of_order: Vec<usize>,
    /// The camera's exposure changes by EXIF: (first frame with the new settings, stops).
    pub exposure_steps: Vec<(usize, f64)>,
    /// The app's settings for this sequence (its steps, options, export), kept as they are.
    pub app: Option<Value>,
}

/// Why a project could not be written or read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectError {
    Io {
        path: PathBuf,
        reason: String,
    },
    /// The file is not a project this version understands.
    Format {
        path: PathBuf,
        reason: String,
    },
    /// A frame's file name cannot be stored (not inside the folder, or not valid UTF-8).
    FrameName(PathBuf),
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, reason } | Self::Format { path, reason } => {
                write!(f, "{}: {reason}", path.display())
            }
            Self::FrameName(p) => write!(f, "{}: not a file name a project can store", p.display()),
        }
    }
}

impl std::error::Error for ProjectError {}

/// How the folder differs from its project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// In the folder, not in the project.
    pub added: Vec<String>,
    /// In the project, not in the folder.
    pub removed: Vec<String>,
}

impl Changes {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

impl Project {
    /// The project of `frames` (all in one folder), `keyframes` being the frames with sidecars.
    ///
    /// # Errors
    /// When a frame has no file name or a name that is not UTF-8.
    pub fn of(frames: &[Frame], keyframes: &[usize]) -> Result<Self, ProjectError> {
        let timing = sequence::timing(frames);
        let exposures: Vec<Option<Exposure>> = frames.iter().map(|f| f.exposure).collect();
        let ramp = exposure::ramp_compensation(&exposures, 0, StepGrid::Thirds).unwrap_or_default();
        let steps = exposure::exposure_steps(&ramp, 0.1);
        let frames = frames
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let file = f
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| ProjectError::FrameName(f.path.clone()))?
                    .to_owned();
                Ok(ProjectFrame {
                    file,
                    captured_ms: f.captured_ms,
                    exposure: f.exposure,
                    keyframe: keyframes.contains(&i),
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            frames,
            interval_ms: timing.as_ref().map(|t| t.interval_ms).filter(|ms| *ms > 0),
            gaps_before: timing.as_ref().map(|t| t.gaps_before.clone()).unwrap_or_default(),
            out_of_order: timing.map(|t| t.out_of_order).unwrap_or_default(),
            exposure_steps: steps.iter().map(|s| (s.frame, s.stops)).collect(),
            app: None,
        })
    }

    /// The project as JSON text.
    #[must_use]
    pub fn to_json(&self) -> String {
        let frames: Vec<Value> = self
            .frames
            .iter()
            .map(|f| {
                json!({
                    "file": f.file,
                    "capturedMs": f.captured_ms,
                    "shutterS": f.exposure.map(|e| e.shutter_s),
                    "aperture": f.exposure.map(|e| e.aperture),
                    "iso": f.exposure.map(|e| e.iso),
                    "keyframe": f.keyframe,
                })
            })
            .collect();
        let steps: Vec<Value> = self
            .exposure_steps
            .iter()
            .map(|(frame, stops)| json!({ "frame": frame, "stops": stops }))
            .collect();
        let mut v = json!({
            "lapsewerk": FORMAT,
            "intervalMs": self.interval_ms,
            "gapsBefore": self.gaps_before,
            "outOfOrder": self.out_of_order,
            "exposureSteps": steps,
            "frames": frames,
        });
        if let (Some(app), Some(map)) = (&self.app, v.as_object_mut()) {
            map.insert("app".into(), app.clone());
        }
        let mut text = serde_json::to_string_pretty(&v).unwrap_or_default();
        text.push('\n');
        text
    }

    /// Reads a project from JSON text; `path` is only for messages.
    ///
    /// # Errors
    /// When the text is not a project of [`FORMAT`] or a value is out of range.
    pub fn from_json(text: &str, path: &Path) -> Result<Self, ProjectError> {
        let bad = |reason: &str| ProjectError::Format {
            path: path.to_path_buf(),
            reason: reason.to_owned(),
        };
        let v: Value = serde_json::from_str(text).map_err(|e| bad(&format!("not JSON ({e})")))?;
        match v.get("lapsewerk").and_then(Value::as_u64) {
            Some(FORMAT) => {}
            Some(other) => {
                return Err(bad(&format!(
                    "project format {other}; this version reads {FORMAT}"
                )));
            }
            None => return Err(bad("not a Lapsewerk project")),
        }
        let indices = |key: &str| -> Result<Vec<usize>, ProjectError> {
            match v.get(key) {
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|x| x.as_u64().and_then(|x| usize::try_from(x).ok()))
                    .collect::<Option<_>>()
                    .ok_or_else(|| bad(&format!("{key}: frame numbers expected"))),
                Some(_) => Err(bad(&format!("{key}: a list expected"))),
            }
        };
        let frames = v
            .get("frames")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("no frames"))?
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let file = f
                    .get("file")
                    .and_then(Value::as_str)
                    .filter(|s| is_plain_name(s))
                    .ok_or_else(|| bad(&format!("frame {i}: no plain file name")))?
                    .to_owned();
                let num = |k: &str| f.get(k).and_then(Value::as_f64);
                let exposure = match (num("shutterS"), num("aperture"), num("iso")) {
                    (Some(s), Some(a), Some(iso)) => Some(
                        Exposure::new(s, a, iso)
                            .map_err(|_| bad(&format!("frame {i}: impossible exposure")))?,
                    ),
                    _ => None,
                };
                Ok(ProjectFrame {
                    file,
                    captured_ms: f.get("capturedMs").and_then(Value::as_i64),
                    exposure,
                    keyframe: f.get("keyframe").and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let n = frames.len();
        let (gaps_before, out_of_order) = (indices("gapsBefore")?, indices("outOfOrder")?);
        let steps = match v.get("exposureSteps") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a
                .iter()
                .map(|s| {
                    let frame = s.get("frame")?.as_u64().and_then(|x| usize::try_from(x).ok())?;
                    let stops = s.get("stops")?.as_f64().filter(|x| x.is_finite())?;
                    Some((frame, stops))
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| bad("exposureSteps: frame and stops expected"))?,
            Some(_) => return Err(bad("exposureSteps: a list expected")),
        };
        if gaps_before
            .iter()
            .chain(&out_of_order)
            .chain(steps.iter().map(|(f, _)| f))
            .any(|&i| i >= n)
        {
            return Err(bad("a frame number past the last frame"));
        }
        Ok(Self {
            frames,
            interval_ms: v.get("intervalMs").and_then(Value::as_i64).filter(|ms| *ms > 0),
            gaps_before,
            out_of_order,
            exposure_steps: steps,
            app: v.get("app").filter(|a| a.is_object()).cloned(),
        })
    }

    /// Writes the project into `folder` as [`FILE_NAME`], atomically; returns its path.
    ///
    /// # Errors
    /// When the folder cannot be written.
    pub fn save(&self, folder: &Path) -> Result<PathBuf, ProjectError> {
        let path = folder.join(FILE_NAME);
        write_atomic(&path, self.to_json().as_bytes()).map_err(|e| ProjectError::Io {
            path: path.clone(),
            reason: e.to_string(),
        })?;
        Ok(path)
    }

    /// The project in `folder`, `None` when it has none.
    ///
    /// # Errors
    /// When the file exists but cannot be read or is not a project.
    pub fn load(folder: &Path) -> Result<Option<Self>, ProjectError> {
        use std::io::Read as _;
        let path = folder.join(FILE_NAME);
        let io = |e: std::io::Error| ProjectError::Io {
            path: path.clone(),
            reason: e.to_string(),
        };
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(e)),
        };
        let mut text = String::new();
        file.take(MAX_BYTES + 1).read_to_string(&mut text).map_err(io)?;
        if text.len() as u64 > MAX_BYTES {
            return Err(ProjectError::Format {
                path,
                reason: "larger than any project".into(),
            });
        }
        Self::from_json(&text, &path).map(Some)
    }

    /// How `files` (the folder's frames now) differ from the project's frames, by file name.
    #[must_use]
    pub fn changes(&self, files: &[PathBuf]) -> Changes {
        use std::collections::BTreeSet;
        let now: BTreeSet<String> = files
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_owned))
            .collect();
        let then: BTreeSet<String> = self.frames.iter().map(|f| f.file.clone()).collect();
        Changes {
            added: now.difference(&then).cloned().collect(),
            removed: then.difference(&now).cloned().collect(),
        }
    }
}

/// A file name without folders: what a project may name a frame (never `..`, never a path).
fn is_plain_name(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\', '\0'])
}

/// Writes `data` to `path` so that `path` holds the old contents or the new, never part of them:
/// a temporary file beside it, flushed to disk, renamed over it, and the folder flushed.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = path
        .file_name()
        .map_or_else(|| "project".into(), |n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        // A file planted at the temporary name (a link elsewhere) is removed, never written through.
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return written;
    }
    // The rename itself is durable once the folder is flushed (not possible on every system).
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(&dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(name: &str, ms: Option<i64>, shutter: f64) -> Frame {
        Frame {
            path: PathBuf::from("/seq").join(name),
            captured_ms: ms,
            exposure: Exposure::new(shutter, 8.0, 100.0).ok(),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lapsewerk-project-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample() -> Project {
        let frames = vec![
            frame("a.arw", Some(0), 1.0 / 100.0),
            frame("b.arw", Some(5000), 1.0 / 100.0),
            frame("c.arw", Some(10_000), 1.0 / 50.0),
            frame("d.arw", Some(60_000), 1.0 / 50.0),
            frame("e.arw", None, 1.0 / 50.0),
        ];
        Project::of(&frames, &[0, 3]).unwrap()
    }

    #[test]
    fn learns_timing_steps_and_keyframes() {
        let p = sample();
        assert_eq!(p.interval_ms, Some(5000));
        assert_eq!(p.gaps_before, vec![3]);
        assert_eq!(p.exposure_steps.len(), 1);
        assert_eq!(p.exposure_steps[0].0, 2);
        assert!(
            (p.exposure_steps[0].1 - 1.0).abs() < 1e-9,
            "{:?}",
            p.exposure_steps
        );
        let keys: Vec<bool> = p.frames.iter().map(|f| f.keyframe).collect();
        assert_eq!(keys, [true, false, false, true, false]);
        assert_eq!(p.frames[4].captured_ms, None);
    }

    #[test]
    fn round_trips_through_the_file_and_sees_changes() {
        let dir = scratch("roundtrip");
        let mut p = sample();
        p.app = Some(json!({"step": 3, "keepLight": 0.3}));
        assert_eq!(Project::load(&dir).unwrap(), None);
        let path = p.save(&dir).unwrap();
        assert_eq!(path, dir.join(FILE_NAME));
        assert_eq!(Project::load(&dir).unwrap(), Some(p.clone()));
        // Saved again: replaced, and no temporary file left.
        p.save(&dir).unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![FILE_NAME.to_owned()]);

        let now: Vec<PathBuf> = ["a.arw", "b.arw", "d.arw", "e.arw", "f.arw"]
            .iter()
            .map(|n| dir.join(n))
            .collect();
        let c = p.changes(&now);
        assert_eq!(c.added, vec!["f.arw".to_owned()]);
        assert_eq!(c.removed, vec!["c.arw".to_owned()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_what_is_not_a_project() {
        let at = Path::new("x/lapsewerk.json");
        let err = |t: &str| Project::from_json(t, at).unwrap_err().to_string();
        assert!(err("{").contains("not JSON"));
        assert!(err("{}").contains("not a Lapsewerk project"));
        assert!(err(r#"{"lapsewerk": 2, "frames": []}"#).contains("format 2"));
        assert!(err(r#"{"lapsewerk": 1}"#).contains("no frames"));
        assert!(
            err(r#"{"lapsewerk": 1, "frames": [{"file": "../etc/passwd"}]}"#).contains("plain file name")
        );
        assert!(err(r#"{"lapsewerk": 1, "frames": [{"file": "a/b.arw"}]}"#).contains("plain file name"));
        assert!(
            err(r#"{"lapsewerk": 1, "frames": [{"file": "a"}], "gapsBefore": [1]}"#)
                .contains("past the last")
        );
        assert!(
            err(r#"{"lapsewerk": 1, "frames": [{"file": "a", "shutterS": -1, "aperture": 8, "iso": 100}]}"#)
                .contains("impossible exposure")
        );
        let ok = Project::from_json(r#"{"lapsewerk": 1, "frames": [{"file": "a.jpg"}]}"#, at).unwrap();
        assert_eq!(ok.frames.len(), 1);
        assert_eq!(ok.interval_ms, None);
    }

    #[test]
    fn a_project_that_is_too_large_is_not_read() {
        let dir = scratch("large");
        let f = std::fs::File::create(dir.join(FILE_NAME)).unwrap();
        f.set_len(MAX_BYTES + 10).unwrap();
        assert!(matches!(Project::load(&dir), Err(ProjectError::Format { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
