//! The analysis of a sequence, kept beside its photos (`.lapsewerk-analysis.json`): opening a
//! project again, or rendering it from the command line, uses it instead of measuring every photo
//! once more (minutes for a long series). It holds for the photos (names, sizes, times changed),
//! the keyframes' settings and the measuring settings it was made with; anything else changed
//! and it is measured anew.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use lapsewerk_core::exposure::StepCheck;
use lapsewerk_develop::{DevelopPlan, Sequence};

use crate::measure::Pass;
use crate::{Analysis, MeasureSettings, Reporter};

/// The file beside the photos.
pub const FILE: &str = ".lapsewerk-analysis.json";

/// Bumped when what an analysis holds or means changes: older files are measured anew.
const VERSION: u64 = 5;

/// A fingerprint of a photo's bytes: all of them up to 1 MB, else 64 pieces of 16 KB
/// spread over the file (a raw's sensor data included; about 1 MB read). A photo changed in
/// place (same size and time, a tool that keeps them; a copy brought back from a backup) is
/// told apart, without reading whole raws each time a project opens.
fn ends_print(path: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    const WHOLE: u64 = 1024 * 1024;
    const PIECE: u64 = 16 * 1024;
    const PIECES: u64 = 64;
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map_or(0, |m| m.len());
    let mut bytes = Vec::with_capacity(WHOLE.min(len) as usize);
    if len <= WHOLE {
        let _ = file.read_to_end(&mut bytes);
    } else {
        let step = (len - PIECE) / (PIECES - 1);
        for k in 0..PIECES {
            if file.seek(SeekFrom::Start(k * step)).is_ok() {
                let _ = (&mut file).take(PIECE).read_to_end(&mut bytes);
            }
        }
    }
    lapsewerk_video::frames::fingerprint_bytes(&bytes)
}

/// Which build of Lapsewerk measured (its commit): another build (an update, a fix in the
/// measuring) measures anew, even with the same version number. App and command line of one
/// build share it, and so each other's analyses.
fn build_print() -> &'static str {
    env!("LAPSEWERK_BUILD")
}

/// What the analysis depends on, as a fingerprint.
fn key(seq: &Sequence, plan: &DevelopPlan, opts: &MeasureSettings) -> String {
    key_with(&photos_text(seq, opts), plan)
}

/// [`key`] from the photos' part ([`photos_text`]) and the keyframes.
fn key_with(photos: &str, plan: &DevelopPlan) -> String {
    let mut text = photos.to_owned();
    for &k in plan.keyframes() {
        text.push_str(&format!(" {k}:{:?}", plan.settings(k, 0.0)));
    }
    lapsewerk_video::frames::fingerprint(&text)
}

/// Each frame's development as it is measured (its settings and measuring offset), as
/// fingerprints: a keyframe changed outside changes only the frames between its neighbours.
fn frame_keys(seq: &Sequence, plan: &DevelopPlan) -> Vec<String> {
    // The steps only, not the headroom all frames share (see `measuring_steps`).
    let local = crate::measuring_steps(seq);
    (0..seq.len())
        .map(|i| {
            // As it is measured (without the tone curve and contrast): a change to those alone
            // measures nothing again. The exposure apart, after a `|`: a frame whose exposure
            // alone moved a little keeps its measurement, moved by as much (see `EXPOSURE_KEPT`).
            let mut d = crate::measure::for_measuring(plan.settings(i, 0.0));
            let exposure = d.light.exposure;
            d.light.exposure = 0.0;
            format!(
                "{}|{exposure:.9}",
                lapsewerk_video::frames::fingerprint(&format!(
                    "{d:?} {:.9}",
                    local.get(i).copied().unwrap_or(0.0)
                ))
            )
        })
        .collect()
}

/// How far (stops) a frame's exposure alone may move brighter and its measurement be kept, moved
/// by as much: the keyframes' smooth curve moves the frames before a keyframe edited at the
/// night's end too (by a little: 296 of 300 frames were measured again for it). Measured without
/// the tone curve, brightness follows exposure; brighter than this a bright part could burn out.
const EXPOSURE_KEPT: f64 = 0.3;

/// The same, for a frame with nothing at white when it was measured (under a thousandth of it):
/// brighter by a stop burns nothing out that the measurement would miss.
const EXPOSURE_KEPT_UNCLIPPED: f64 = 1.0;

/// The same, darker: nothing can burn out; the measurement follows exposure exactly (HG, the night
/// 1.3 stops darker: kept and moved agree with measuring again to 0.0001 stops).
const EXPOSURE_KEPT_DARKER: f64 = 6.0;

/// Which frames are measured again, and by how much (stops) each kept one's exposure moved.
fn compare_frames(new: &[String], old: &[String], clipped: &[Option<f32>]) -> (Vec<bool>, Vec<f64>) {
    let split = |k: &str| -> (String, Option<f64>) {
        match k.split_once('|') {
            Some((a, e)) => (a.to_owned(), e.parse().ok()),
            None => (k.to_owned(), None),
        }
    };
    new.iter()
        .zip(old)
        .enumerate()
        .map(|(i, (a, b))| {
            let ((ka, ea), (kb, eb)) = (split(a), split(b));
            let unclipped = clipped.get(i).copied().flatten().is_some_and(|c| c < 0.001);
            let kept = |d: f64| {
                if d <= 0.0 {
                    -d <= EXPOSURE_KEPT_DARKER
                } else if unclipped {
                    d <= EXPOSURE_KEPT_UNCLIPPED
                } else {
                    d <= EXPOSURE_KEPT
                }
            };
            match (ea, eb) {
                (Some(ea), Some(eb)) if ka == kb && kept(ea - eb) => (false, ea - eb),
                _ => (a != b, 0.0),
            }
        })
        .unzip()
}

/// What the analysis depends on besides the keyframes: the version, the measuring settings and
/// the photos.
fn photos_text(seq: &Sequence, opts: &MeasureSettings) -> String {
    let mut text = setup_text(opts);
    for p in photo_prints(seq) {
        text.push(' ');
        text.push_str(&p);
    }
    text
}

/// The version and the measuring settings an analysis was made with.
fn setup_text(opts: &MeasureSettings) -> String {
    format!(
        "v{VERSION} {} {} edge {} reference {:?} stabilize {}",
        env!("CARGO_PKG_VERSION"),
        build_print(),
        opts.edge,
        opts.reference,
        opts.stabilize
    )
}

/// Each photo by its name, size and bytes, not where it is or when it was touched: a series
/// moved or copied elsewhere keeps its analysis; a photo changed in it does not.
fn photo_prints(seq: &Sequence) -> Vec<String> {
    seq.frames()
        .iter()
        .map(|f| {
            let meta = std::fs::metadata(&f.path).ok();
            let len = meta.as_ref().map_or(0, std::fs::Metadata::len);
            // A large file (a raw) is read in pieces only: its time counts too, so a raw rewritten
            // between the pieces is measured anew (moving keeps the time; a plain copy measures anew).
            let changed = if len > 1024 * 1024 {
                meta.and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs())
            } else {
                0
            };
            let name = f
                .path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            format!("{name}:{len}:{changed}:{}", ends_print(&f.path))
        })
        .collect()
}

/// Which photos changed since the analysis `cached` (by frame), when it was measured the same way
/// (version, settings) from as many photos; `None` otherwise (all are measured again then).
fn changed_photos(cached: &Value, seq: &Sequence, opts: &MeasureSettings) -> Option<Vec<bool>> {
    let setup = lapsewerk_video::frames::fingerprint(&setup_text(opts));
    if cached.get("setup").and_then(Value::as_str) != Some(setup.as_str()) {
        return None;
    }
    let before = cached.get("photoPrints")?.as_array()?;
    let now = photo_prints(seq);
    if before.len() != now.len() {
        return None;
    }
    // By name too: a photo renamed or put in another place is another series.
    let name = |p: &str| p.split(':').next().unwrap_or_default().to_owned();
    if now
        .iter()
        .zip(before)
        .any(|(n, b)| b.as_str().map(name) != Some(name(n)))
    {
        return None;
    }
    Some(
        now.iter()
            .zip(before)
            .map(|(n, b)| b.as_str() != Some(n.as_str()))
            .collect(),
    )
}

/// Why an analysis kept from before no longer holds for the photos (said before measuring all).
fn why_measured_again(cached: &Value, seq: &Sequence, opts: &MeasureSettings) -> String {
    let setup = lapsewerk_video::frames::fingerprint(&setup_text(opts));
    if cached
        .get("setup")
        .and_then(Value::as_str)
        .is_some_and(|s| s != setup)
    {
        return "measuring settings or Lapsewerk's version changed since the last analysis".to_owned();
    }
    let Some(before) = cached.get("photoPrints").and_then(Value::as_array) else {
        return "the photos or Lapsewerk changed since the last analysis".to_owned();
    };
    let now = photo_prints(seq);
    if before.len() != now.len() {
        return format!("{} photos now, {} at the last analysis", now.len(), before.len());
    }
    let changed: Vec<usize> = now
        .iter()
        .zip(before)
        .enumerate()
        .filter(|(_, (n, b))| b.as_str() != Some(n.as_str()))
        .map(|(i, _)| i)
        .collect();
    match (changed.first(), changed.last()) {
        (Some(a), Some(b)) => format!(
            "photos changed since the last analysis: {} (frames {}-{})",
            changed.len(),
            a + 1,
            b + 1
        ),
        _ => "measuring settings or Lapsewerk's version changed since the last analysis".to_owned(),
    }
}

/// [`crate::analyse`], or the analysis kept in `folder` when it was made from the same photos,
/// keyframes and settings; a new one is kept there (best effort: a read-only folder only means
/// measuring again next time).
///
/// # Errors
/// As [`crate::analyse`].
pub fn analyse_cached(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    opts: &MeasureSettings,
    rep: &dyn Reporter,
    folder: &Path,
) -> Result<Analysis, String> {
    let photos_text = photos_text(seq, opts);
    let key = key_with(&photos_text, plan);
    let photos = lapsewerk_video::frames::fingerprint(&photos_text);
    let path = folder.join(FILE);
    let cached = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    if let Some(a) = cached
        .as_ref()
        .filter(|v| v.get("key").and_then(Value::as_str) == Some(key.as_str()))
        .and_then(from_json)
        .filter(|a| a.as_shot.stops.len() == seq.len())
    {
        rep.note("analysis:      as measured before (the photos and keyframes are unchanged)");
        return Ok(a);
    }
    let frames = frame_keys(seq, plan);
    let sidecars = sidecar_prints(seq, plan);
    // The same photos, keyframes changed (in LightCraft, another raw editor or by hand): only the frames
    // whose development changed are measured again.
    // Some photos changed (retouched, replaced), measured the same way as then: only those (and
    // the frames whose development changed) are measured again.
    let photos_changed = cached.as_ref().and_then(|v| changed_photos(v, seq, opts));
    let earlier = cached
        .as_ref()
        .filter(|v| {
            v.get("photos").and_then(Value::as_str) == Some(photos.as_str()) || photos_changed.is_some()
        })
        .and_then(|v| {
            let old: Vec<String> = v
                .get("frames")?
                .as_array()?
                .iter()
                .map(|f| f.as_str().map(str::to_owned))
                .collect::<Option<_>>()?;
            Some((from_json(v)?, old))
        })
        .filter(|(a, old)| old.len() == seq.len() && a.as_shot.stops.len() == seq.len());
    let a = match earlier {
        Some((old_a, old)) if !opts.stabilize => {
            let (mut again, moved) = compare_frames(&frames, &old, &old_a.as_shot.clipped);
            let photos_again = photos_changed.as_ref().map(|c| {
                for (a, c) in again.iter_mut().zip(c) {
                    *a |= *c;
                }
                c.iter().filter(|c| **c).count()
            });
            // Kept frames whose exposure alone moved: their measurements moved by as much.
            let mut old_a = old_a;
            for (i, d) in moved.iter().enumerate().filter(|(_, d)| **d != 0.0) {
                let p = &mut old_a.as_shot;
                for list in [&mut p.stops, &mut p.perceived] {
                    if let Some(Some(v)) = list.get_mut(i) {
                        *v += d;
                    }
                }
                if let Some(tiles) = p.tiles.get_mut(i) {
                    for t in tiles.iter_mut().flatten() {
                        *t += d;
                    }
                }
            }
            let count = again.iter().filter(|a| **a).count();
            let first = again.iter().position(|a| *a).unwrap_or(0);
            let last = again.iter().rposition(|a| *a).unwrap_or(0);
            let which = changed_keyframes(cached.as_ref(), &sidecars, folder);
            let shifted = moved.iter().filter(|d| **d != 0.0).count();
            let mut line = if let Some(p) = photos_again.filter(|p| *p > 0) {
                let a = photos_changed
                    .as_ref()
                    .and_then(|c| c.iter().position(|c| *c))
                    .unwrap_or(0);
                let b = photos_changed
                    .as_ref()
                    .and_then(|c| c.iter().rposition(|c| *c))
                    .unwrap_or(0);
                format!(
                    "analysis:      photos changed since the last analysis: {p} (frames {}-{}): {count} of {} frames measured again ({}-{}), the others kept",
                    a + 1,
                    b + 1,
                    seq.len(),
                    first + 1,
                    last + 1
                )
            } else if count == 0 && shifted > 0 {
                // Only the exposure moved: the measurements move by as much.
                format!(
                    "analysis:      keyframes changed since the last analysis, in exposure only: {shifted} frame(s)' measurements moved by as much, nothing measured again"
                )
            } else if count == 0 {
                // Only what is not measured changed (the tone curve, contrast, shadows, highlights): nothing to measure.
                "analysis:      keyframes changed since the last analysis, only in what is not measured (tone curve, contrast, shadows, highlights): nothing measured again".to_owned()
            } else if count == seq.len() {
                // Every frame changed (a brighter look gives every frame more room to measure in).
                format!(
                    "analysis:      keyframes changed since the last analysis: all {count} frames measured again (the change reaches every frame)"
                )
            } else {
                format!(
                    "analysis:      keyframes changed since the last analysis: {count} of {} frames measured again ({}-{}), the others kept",
                    seq.len(),
                    first + 1,
                    last + 1
                )
            };
            // Which keyframe was edited, at the end (the app reads the numbers before it).
            if !which.is_empty() {
                let list: Vec<String> = which.iter().map(|f| (f + 1).to_string()).collect();
                line.push_str(&format!(
                    "; edited: keyframe{} {}",
                    if which.len() == 1 { "" } else { "s" },
                    list.join(", ")
                ));
                // Read-only or linked sidecars count as edited (never written over): said so.
                let kept: Vec<String> = which
                    .iter()
                    .filter(|f| !seq.sidecar_rewritable(**f))
                    .map(|f| (f + 1).to_string())
                    .collect();
                if !kept.is_empty() {
                    line.push_str(&format!(
                        " ({} read-only or linked: kept as they are, they don't follow the others)",
                        kept.join(", ")
                    ));
                }
            }
            rep.note(&line);
            crate::analyse_reusing(seq, plan, opts, rep, Some((&old_a.as_shot, &again)))?
        }
        _ => {
            if let Some(c) = cached.as_ref() {
                rep.note(&format!(
                    "analysis:      {}: all frames measured again",
                    why_measured_again(c, seq, opts)
                ));
            }
            crate::analyse(seq, plan, opts, rep)?
        }
    };
    let mut v = to_json(&a);
    if let Some(o) = v.as_object_mut() {
        o.insert("key".into(), Value::String(key));
        o.insert("photos".into(), Value::String(photos));
        o.insert(
            "setup".into(),
            Value::String(lapsewerk_video::frames::fingerprint(&setup_text(opts))),
        );
        o.insert(
            "photoPrints".into(),
            Value::Array(photo_prints(seq).into_iter().map(Value::String).collect()),
        );
        o.insert(
            "frames".into(),
            Value::Array(frames.into_iter().map(Value::String).collect()),
        );
        o.insert(
            "sidecars".into(),
            Value::Array(sidecars.into_iter().map(|(f, p)| json!([f, p])).collect()),
        );
    }
    // Written whole or not at all.
    let tmp = folder.join(format!("{FILE}.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, v.to_string()).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(a)
}

/// Keeps the analysis made with `before` for `after` when the two develop every frame alike, as
/// after writing keyframes that hold what the plan already gave there: nothing measured changed.
pub fn carry_over(
    seq: &Sequence,
    before: &DevelopPlan,
    after: &DevelopPlan,
    opts: &MeasureSettings,
    folder: &Path,
) {
    let alike = (0..seq.len()).all(|f| {
        same_numbers(
            &format!("{:?}", before.settings(f, 0.0)),
            &format!("{:?}", after.settings(f, 0.0)),
        )
    });
    if !alike {
        return;
    }
    let path = folder.join(FILE);
    let Some(mut v) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return;
    };
    if v.get("key").and_then(Value::as_str) != Some(key(seq, before, opts).as_str()) {
        return;
    }
    if let Some(o) = v.as_object_mut() {
        o.insert("key".into(), Value::String(key(seq, after, opts)));
        o.insert(
            "frames".into(),
            Value::Array(frame_keys(seq, after).into_iter().map(Value::String).collect()),
        );
    }
    let tmp = folder.join(format!("{FILE}.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, v.to_string()).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Each keyframe with its sidecar's print ([`sidecar_print`]), to name the ones edited since.
fn sidecar_prints(seq: &Sequence, plan: &DevelopPlan) -> Vec<(usize, String)> {
    plan.keyframes()
        .iter()
        .map(|&k| (k, sidecar_print(seq, k)))
        .collect()
}

/// The keyframes whose sidecar changed since the analysis `cached` (new ones too), leaving out
/// the automatic ones that only followed (their new sidecar noted in the project file).
fn changed_keyframes(cached: Option<&Value>, now: &[(usize, String)], folder: &Path) -> Vec<usize> {
    let Some(before) = cached.and_then(|v| v.get("sidecars")).and_then(Value::as_array) else {
        return Vec::new();
    };
    let before: Vec<(usize, &str)> = before
        .iter()
        .filter_map(|e| {
            let f = usize::try_from(e.get(0)?.as_u64()?).ok()?;
            Some((f, e.get(1)?.as_str()?))
        })
        .collect();
    let followed: Vec<(usize, String)> = lapsewerk_develop::project::Project::load(folder)
        .ok()
        .flatten()
        .and_then(|p| p.app)
        .map(|app| untouched_in(&app))
        .unwrap_or_default();
    now.iter()
        .filter(|(f, p)| !before.iter().any(|(b, q)| b == f && q == p))
        .filter(|(f, p)| !followed.iter().any(|(u, q)| u == f && q == p))
        .map(|(f, _)| *f)
        .collect()
}

/// Adds to `untouched` the keyframes whose sidecar carries Lapsewerk's automatic mark with its
/// settings as they are now ([`lapsewerk_develop::Sequence::automatic_mark`]): photos and XMP
/// passed on without `lapsewerk.json` keep which keyframes follow the others. One edited since
/// (anywhere) no longer matches its mark. Returns whether any was added.
pub fn with_marked(seq: &Sequence, untouched: &mut Vec<(usize, String)>) -> bool {
    let mut added = false;
    for f in seq.keyframe_frames() {
        if untouched.iter().any(|(u, _)| *u == f) {
            continue;
        }
        let print = sidecar_print(seq, f);
        if !print.is_empty() && seq.automatic_mark(f).is_some_and(|m| m == print) {
            untouched.push((f, print));
            added = true;
        }
    }
    untouched.sort_by_key(|(f, _)| *f);
    added
}

/// Marks each of the `untouched` keyframes as automatic in its own sidecar, with its settings'
/// fingerprint as they are now (see [`with_marked`]). Called only for sidecars Lapsewerk has just
/// written itself (made, or set again by the auto transition): others are never changed. A
/// sidecar that can't be marked for sure keeps its keyframe automatic in `lapsewerk.json` alone.
pub fn mark_untouched(seq: &Sequence, untouched: &[(usize, String)]) {
    for (f, _) in untouched {
        let print = sidecar_print(seq, *f);
        if !print.is_empty() {
            let _ = seq.set_automatic_mark(*f, &print);
        }
    }
}

/// A fingerprint of `frame`'s sidecar as it is now (empty without one): a keyframe whose sidecar
/// changed since it was made has been edited (in the app, in LightCraft or by hand).
#[must_use]
pub fn sidecar_print(seq: &Sequence, frame: usize) -> String {
    seq.sidecar_develop_text(frame)
        .map(|t| lapsewerk_video::frames::fingerprint(&t))
        .unwrap_or_default()
}

/// Notes `made` (keyframes just written, never edited) in the project file's untouched keyframes,
/// with their sidecars' fingerprints, so the app's auto transition lets them follow the edited
/// ones. Makes the project file when there is none.
///
/// # Errors
/// When the project file can't be read or written.
pub fn note_untouched(seq: &Sequence, made: &[usize], folder: &Path) -> Result<(), String> {
    use lapsewerk_develop::project::Project;
    if made.is_empty() {
        return Ok(());
    }
    let mut project = match Project::load(folder).map_err(|e| e.to_string())? {
        Some(p) => p,
        None => Project::of(seq.frames(), &seq.keyframe_frames()).map_err(|e| e.to_string())?,
    };
    let mut app = project
        .app
        .take()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let Some(map) = app.as_object_mut() else {
        return Err("the project file's app settings are not an object".into());
    };
    let mut list: Vec<Value> = map
        .get("untouchedKeyframes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for &f in made {
        let frame_of = |e: &Value| {
            e.as_array()
                .and_then(|p| p.first())
                .or(Some(e))
                .and_then(Value::as_u64)
        };
        list.retain(|e| frame_of(e) != Some(f as u64));
        list.push(serde_json::json!([f, sidecar_print(seq, f)]));
    }
    map.insert("untouchedKeyframes".into(), Value::Array(list));
    project.app = Some(app);
    project.save(folder).map_err(|e| e.to_string())?;
    let marked: Vec<(usize, String)> = made.iter().map(|&f| (f, String::new())).collect();
    mark_untouched(seq, &marked);
    Ok(())
}

/// Auto transition: the `untouched` keyframes (made, never edited; each with its sidecar's
/// fingerprint) follow the edited ones. One whose sidecar changed since (edited in the app, in
/// LightCraft, in another raw editor or by hand) is edited now: it leaves the list and is never written
/// over. The others are set again when the edited keyframes differ from `seen` (what they held
/// when the untouched ones last followed them); the new `seen` comes back then.
///
/// # Errors
/// When the keyframes cannot be interpolated or a sidecar cannot be written.
pub fn follow_edited(
    seq: &mut Sequence,
    untouched: &mut Vec<(usize, String)>,
    seen: &str,
    light: Option<&[f64]>,
) -> Result<Option<String>, String> {
    // A light of another length (another series' analysis) is none: by frame, and said so.
    let light = light.filter(|l| l.len() == seq.len());
    // Those marked automatic in their own sidecars (passed on without lapsewerk.json).
    with_marked(seq, untouched);
    untouched.retain_mut(|(f, stored)| match seq.sidecar(*f) {
        None => false,
        Some(_) if stored.is_empty() => {
            *stored = sidecar_print(seq, *f);
            true
        }
        Some(_) => *stored == sidecar_print(seq, *f),
    });
    // A read-only, linked or hard-linked sidecar is someone's arrangement: never written over, so
    // its keyframe counts as edited (it stays in the list, so it can be said every time).
    let active: Vec<usize> = untouched
        .iter()
        .map(|(f, _)| *f)
        .filter(|f| seq.sidecar_rewritable(*f))
        .collect();
    if active.is_empty() {
        return Ok(None);
    }
    let mut edited: String = seq
        .keyframe_frames()
        .into_iter()
        .filter(|f| !active.contains(f))
        .map(|f| format!("{f}:{} ", sidecar_print(seq, f)))
        .collect();
    // Along the light once it is known: then once more.
    if !edited.is_empty() && light.is_some() {
        edited.push_str("light");
    }
    if edited.is_empty() || edited == seen {
        return Ok(None);
    }
    seq.auto_transition_along(lapsewerk_core::keyframes::Interpolation::Smooth, &active, light)
        .map_err(|e| e.to_string())?;
    for (f, stored) in untouched.iter_mut() {
        *stored = sidecar_print(seq, *f);
    }
    mark_untouched(seq, untouched);
    Ok(Some(edited))
}

/// The scene's brightness per frame from the analysis kept in `folder`, whatever it was made
/// with, when it covers `n` frames.
#[must_use]
pub fn kept_scene(folder: &Path, n: usize) -> Option<Vec<f64>> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(folder.join(FILE)).ok()?).ok()?;
    list(&v, "scene").filter(|s| s.len() == n && s.iter().all(|x| x.is_nan() || x.abs() <= 64.0))
}

/// The untouched keyframes the project file in `folder` lists (see [`follow_edited`]).
#[must_use]
pub fn untouched_in(app: &Value) -> Vec<(usize, String)> {
    app.get("untouchedKeyframes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| match e {
                    // [frame, fingerprint of its sidecar]; a bare frame (an earlier version): its
                    // sidecar as it is now is taken for untouched.
                    Value::Array(p) => Some((
                        usize::try_from(p.first()?.as_u64()?).ok()?,
                        p.get(1)?.as_str().unwrap_or_default().to_owned(),
                    )),
                    v => Some((usize::try_from(v.as_u64()?).ok()?, String::new())),
                })
                .take(10_000)
                .collect()
        })
        .unwrap_or_default()
}

/// The keyframes edited by hand (in the app, LightCraft or another raw editor): those with a sidecar that
/// neither the project file in `folder` nor a mark in the sidecar notes as made automatically.
#[must_use]
pub fn edited_keyframes_in(seq: &Sequence, folder: &Path) -> Vec<usize> {
    use lapsewerk_develop::project::Project;
    let mut untouched = match Project::load(folder) {
        Ok(Some(p)) => p.app.as_ref().map(untouched_in).unwrap_or_default(),
        _ => Vec::new(),
    };
    with_marked(seq, &mut untouched);
    seq.keyframe_frames()
        .into_iter()
        .filter(|f| !untouched.iter().any(|(u, _)| u == f))
        .collect()
}

/// The automatic keyframes (never edited) whose sidecars are read-only or linked: never written
/// over, so they keep their look and do not follow the edited ones (said, not left silent).
#[must_use]
pub fn untouched_kept(seq: &Sequence, untouched: &[(usize, String)]) -> Vec<usize> {
    let mut all = untouched.to_vec();
    with_marked(seq, &mut all);
    let mut kept: Vec<usize> = all
        .iter()
        .map(|(f, _)| *f)
        .filter(|f| seq.sidecar(*f).is_some() && !seq.sidecar_rewritable(*f))
        .collect();
    kept.sort_unstable();
    kept.dedup();
    kept
}

/// [`untouched_kept`] for the keyframes the project file in `folder` notes as never edited.
#[must_use]
pub fn untouched_kept_in(seq: &Sequence, folder: &Path) -> Vec<usize> {
    use lapsewerk_develop::project::Project;
    let untouched = match Project::load(folder) {
        Ok(Some(p)) => p.app.as_ref().map(untouched_in).unwrap_or_default(),
        _ => Vec::new(),
    };
    untouched_kept(seq, &untouched)
}

/// The CLI's auto transition, as the app does it on opening: the untouched keyframes in the
/// project file follow the edited ones, and the file notes their new sidecars. The frames that
/// followed.
///
/// # Errors
/// As [`follow_edited`], or when the project file can't be written.
pub fn follow_edited_in(seq: &mut Sequence, folder: &Path) -> Result<Vec<usize>, String> {
    use lapsewerk_develop::project::Project;
    let Ok(Some(mut project)) = Project::load(folder) else {
        // No project file (photos and XMP passed on): the keyframes marked automatic in their
        // sidecars follow the edited ones all the same.
        let mut untouched = Vec::new();
        let light = kept_scene(folder, seq.len());
        return Ok(match follow_edited(seq, &mut untouched, "", light.as_deref())? {
            Some(_) => untouched
                .iter()
                .map(|(f, _)| *f)
                .filter(|f| seq.sidecar_rewritable(*f))
                .collect(),
            None => Vec::new(),
        });
    };
    let mut app = project
        .app
        .take()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let before = untouched_in(&app);
    let mut untouched = before.clone();
    // The scene's light from the analysis kept here (any: the light hardly depends on the looks).
    let light = kept_scene(folder, seq.len());
    // What the edited keyframes held when the others last followed them (the app's too): unchanged,
    // nothing is written again (a measurement a hair different must not move the keyframes and so
    // call for measuring again).
    let seen = app
        .get("editedSeen")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let followed = follow_edited(seq, &mut untouched, &seen, light.as_deref())?;
    if untouched == before && followed.is_none() {
        return Ok(Vec::new());
    }
    if let Some(map) = app.as_object_mut() {
        map.insert(
            "untouchedKeyframes".into(),
            Value::Array(untouched.iter().map(|(f, p)| serde_json::json!([f, p])).collect()),
        );
        if let Some(seen) = &followed {
            map.insert("editedSeen".into(), Value::String(seen.clone()));
        }
    }
    project.app = Some(app);
    project.save(folder).map_err(|e| e.to_string())?;
    Ok(if followed.is_some() {
        untouched
            .iter()
            .map(|(f, _)| *f)
            .filter(|f| seq.sidecar_rewritable(*f))
            .collect()
    } else {
        Vec::new()
    })
}

/// Two settings written out alike, their numbers alike to 1e-9 (interpolation leaves 1e-14 behind).
fn same_numbers(a: &str, b: &str) -> bool {
    let split = |s: &str| -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut num = false;
        for c in s.chars() {
            let n = c.is_ascii_digit() || c == '.' || c == '-' || c == 'e';
            if n != num && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            num = n;
            cur.push(c);
        }
        out.push(cur);
        out
    };
    let (x, y) = (split(a), split(b));
    x.len() == y.len()
        && x.iter().zip(&y).all(|(p, q)| {
            p == q
                || matches!((p.parse::<f64>(), q.parse::<f64>()), (Ok(m), Ok(n)) if (m - n).abs() <= 1e-9 * m.abs().max(n.abs()).max(1.0))
        })
}

/// The corrections kept beside the photos.
pub const CORRECTIONS_FILE: &str = ".lapsewerk-corrections.json";

/// [`crate::correct`], or the corrections kept in `folder` when they were worked out from the same
/// photos, keyframes and measuring settings with the same deflicker, passes, day to night and hand
/// curve (they are all the offsets depend on). With dust, regional deflicker or a verification
/// asked for, always worked out anew.
///
/// # Errors
/// As [`crate::correct`].
pub fn correct_cached(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    a: &Analysis,
    opts: &MeasureSettings,
    render: &crate::RenderArgs,
    rep: &dyn Reporter,
    folder: &Path,
) -> Result<crate::Corrected, String> {
    if render.dust || render.regional.is_some() || render.verify {
        return crate::correct(seq, plan, a, opts, render, rep);
    }
    let key = format!(
        "{} {:?} {} {:?} {:?}{}{} {:.4}",
        key(seq, plan, opts),
        render.deflicker,
        render.passes,
        render.keep_light,
        render.brightness,
        if render.auto_brightness { " auto" } else { "" },
        if render.base_look { " look" } else { "" },
        // The video's shape: a crop wider than it is made taller (`framed_plan`).
        render.video.width as f64 / render.video.height.max(1) as f64
    );
    let key = lapsewerk_video::frames::fingerprint(&key);
    let path = folder.join(CORRECTIONS_FILE);
    let kept = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(|v| v.get("key").and_then(Value::as_str) == Some(key.as_str()));
    if let Some(v) = kept
        && let (Some(offsets), Some(brightening)) = (list(&v, "offsets"), list(&v, "brightening"))
        && offsets.len() == seq.len()
        && offsets
            .iter()
            .chain(&brightening)
            .all(|o| o.is_finite() && o.abs() <= 64.0)
        && let Some(size) = v.get("size").and_then(Value::as_array).and_then(|s| {
            Some((
                usize::try_from(s.first()?.as_u64()?).ok()?,
                usize::try_from(s.get(1)?.as_u64()?).ok()?,
            ))
        })
        // Written whole by Lapsewerk: a file cut short or edited by hand is worked out anew.
        && v.get("check").and_then(Value::as_str) == Some(corrections_check(&offsets, &brightening, size).as_str())
    {
        rep.note("corrections:   as worked out before (same photos, keyframes and settings)");
        let mut r = render.clone();
        crate::upright(&mut r, size);
        return Ok(crate::Corrected {
            offsets,
            brightening,
            second: None,
            regional: None,
            verified: None,
            verified_seen: None,
            dust: None,
            size,
            render: r,
        });
    }
    let c = crate::correct(seq, plan, a, opts, render, rep)?;
    let v = json!({
        "key": key,
        "offsets": nums(&c.offsets),
        "brightening": nums(&c.brightening),
        "size": [c.size.0, c.size.1],
        "check": corrections_check(&c.offsets, &c.brightening, c.size),
    });
    let tmp = folder.join(format!("{CORRECTIONS_FILE}.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, v.to_string()).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(c)
}

/// A fingerprint of the kept corrections themselves.
fn corrections_check(offsets: &[f64], brightening: &[f64], size: (usize, usize)) -> String {
    lapsewerk_video::frames::fingerprint(&format!("{offsets:?} {brightening:?} {size:?}"))
}

fn nums(v: &[f64]) -> Value {
    Value::Array(v.iter().map(|x| num(*x)).collect())
}

fn num(x: f64) -> Value {
    serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)
}

fn opts(v: &[Option<f64>]) -> Value {
    Value::Array(v.iter().map(|x| x.map_or(Value::Null, num)).collect())
}

fn to_json(a: &Analysis) -> Value {
    let p = &a.as_shot;
    json!({
        "exifRamp": nums(&a.exif_ramp),
        "ramp": nums(&a.ramp),
        "checks": a.checks.iter().map(|c| json!([c.frame, num(c.reported), c.measured.map_or(Value::Null, num), num(c.used)])).collect::<Vec<_>>(),
        "compensated": opts(&a.compensated),
        "corrections": nums(&a.corrections),
        "motion": a.motion.as_ref().map(|m| m.iter().map(|(x, y)| json!([num(*x), num(*y)])).collect::<Vec<_>>()),
        "scene": nums(&a.scene),
        "videoTrend": nums(&a.video_trend),
        "edits": nums(&a.edits),
        "asShot": {
            "stops": opts(&p.stops),
            "raw": opts(&p.raw),
            "failures": p.failures.iter().map(|(i, e)| json!([i, e])).collect::<Vec<_>>(),
            "perceived": opts(&p.perceived),
            "tiles": p.tiles.iter().map(|t| opts(t)).collect::<Vec<_>>(),
            "sizes": p.sizes.iter().map(|s| s.map_or(Value::Null, |(w, h)| json!([w, h]))).collect::<Vec<_>>(),
            "clipped": p.clipped.iter().map(|c| c.map_or(Value::Null, |c| num(f64::from(c)))).collect::<Vec<_>>(),
            "seconds": p.time.as_secs_f64(),
        },
    })
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

fn list(v: &Value, k: &str) -> Option<Vec<f64>> {
    Some(v.get(k)?.as_array()?.iter().map(f).collect())
}

fn opt_list(v: &Value, k: &str) -> Option<Vec<Option<f64>>> {
    Some(v.get(k)?.as_array()?.iter().map(Value::as_f64).collect())
}

fn from_json(v: &Value) -> Option<Analysis> {
    let s = v.get("asShot")?;
    let checks = v
        .get("checks")?
        .as_array()?
        .iter()
        .map(|c| {
            let c = c.as_array()?;
            Some(StepCheck {
                frame: usize::try_from(c.first()?.as_u64()?).ok()?,
                reported: f(c.get(1)?),
                measured: c.get(2)?.as_f64(),
                used: f(c.get(3)?),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let motion = match v.get("motion") {
        Some(Value::Array(m)) => Some(
            m.iter()
                .map(|p| {
                    let p = p.as_array()?;
                    Some((f(p.first()?), f(p.get(1)?)))
                })
                .collect::<Option<Vec<_>>>()?,
        ),
        _ => None,
    };
    let as_shot = Pass {
        stops: opt_list(s, "stops")?,
        raw: opt_list(s, "raw")?,
        failures: s
            .get("failures")?
            .as_array()?
            .iter()
            .map(|e| {
                let e = e.as_array()?;
                Some((
                    usize::try_from(e.first()?.as_u64()?).ok()?,
                    e.get(1)?.as_str()?.to_owned(),
                ))
            })
            .collect::<Option<Vec<_>>>()?,
        perceived: opt_list(s, "perceived")?,
        tiles: s
            .get("tiles")?
            .as_array()?
            .iter()
            .map(|t| Some(t.as_array()?.iter().map(Value::as_f64).collect()))
            .collect::<Option<Vec<_>>>()?,
        thumbs: Vec::new(),
        sizes: s
            .get("sizes")?
            .as_array()?
            .iter()
            .map(|p| {
                let p = p.as_array()?;
                Some((
                    usize::try_from(p.first()?.as_u64()?).ok()?,
                    usize::try_from(p.get(1)?.as_u64()?).ok()?,
                ))
            })
            .collect(),
        clipped: s
            .get("clipped")?
            .as_array()?
            .iter()
            .map(|c| c.as_f64().map(|c| c as f32))
            .collect(),
        time: Duration::from_secs_f64(s.get("seconds")?.as_f64()?.max(0.0)),
    };
    let a = Analysis {
        exif_ramp: list(v, "exifRamp")?,
        ramp: list(v, "ramp")?,
        checks,
        as_shot,
        compensated: opt_list(v, "compensated")?,
        corrections: list(v, "corrections")?,
        motion,
        scene: list(v, "scene")?,
        video_trend: list(v, "videoTrend")?,
        edits: list(v, "edits")?,
    };
    plausible(&a).then_some(a)
}

/// Whether a read analysis holds what a measurement can: brightness within ±64 stops (a camera
/// covers about 30). A file damaged or edited by hand is measured anew instead of drawn or used.
fn plausible(a: &Analysis) -> bool {
    let fine = |v: f64| v.is_nan() || v.abs() <= 64.0;
    let lists = [
        &a.exif_ramp,
        &a.ramp,
        &a.corrections,
        &a.scene,
        &a.video_trend,
        &a.edits,
    ];
    let options = [
        &a.as_shot.stops,
        &a.as_shot.raw,
        &a.as_shot.perceived,
        &a.compensated,
    ];
    lists.iter().all(|l| l.iter().all(|v| fine(*v)))
        && options.iter().all(|l| l.iter().flatten().all(|v| fine(*v)))
        && a.as_shot.tiles.iter().flatten().flatten().all(|v| fine(*v))
        && a.checks
            .iter()
            .all(|c| fine(c.reported) && fine(c.used) && c.measured.is_none_or(fine))
}

#[cfg(test)]
mod tests {
    #[test]
    fn numbers_alike_despite_interpolation_noise() {
        assert!(super::same_numbers(
            "a { x: 50.0, y: true }",
            "a { x: 50.00000000000001, y: true }"
        ));
        assert!(!super::same_numbers("a { x: 50.0 }", "a { x: 50.1 }"));
        assert!(!super::same_numbers("a { x: 50.0 }", "b { x: 50.0 }"));
        assert!(!super::same_numbers("v: Some(1.0)", "v: None"));
    }

    use super::*;

    #[test]
    fn untouched_keyframes_follow_and_edited_ones_are_never_written_over() {
        let dir = std::env::temp_dir().join(format!("lapsewerk-studio-follow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<std::path::PathBuf> = (0..5)
            .map(|i| {
                let p = dir.join(format!("f{i:03}.jpg"));
                let mut bytes = include_bytes!("../../develop/tests/data/frame.jpg").to_vec();
                bytes.extend_from_slice(format!("frame {i}").as_bytes());
                std::fs::write(&p, bytes).unwrap();
                p
            })
            .collect();
        let mut seq = Sequence::open(&paths).unwrap();
        for f in [0, 2, 4] {
            let d = seq.settings(f).unwrap();
            seq.save_keyframe(f, d).unwrap();
        }
        // Made by `analyze --write-keyframes`: all three untouched, noted in the project file.
        note_untouched(&seq, &[0, 2, 4], &dir).unwrap();
        // Day and night edited elsewhere (LightCraft): they leave the list, the middle follows.
        let mut day = seq.settings(0).unwrap();
        day.light.exposure = 1.0;
        seq.save_keyframe(0, day).unwrap();
        let mut night = seq.settings(4).unwrap();
        night.light.exposure = -2.0;
        seq.save_keyframe(4, night).unwrap();
        assert_eq!(follow_edited_in(&mut seq, &dir).unwrap(), [2]);
        let middle = seq.settings(2).unwrap().light.exposure;
        assert!(
            (middle + 0.5).abs() < 0.2,
            "the middle follows day and night: {middle}"
        );
        // Now the middle is edited by hand: it is never written over again.
        let mut mine = seq.settings(2).unwrap();
        mine.light.exposure = 0.7;
        seq.save_keyframe(2, mine).unwrap();
        let mut night = seq.settings(4).unwrap();
        night.light.exposure = -3.0;
        seq.save_keyframe(4, night).unwrap();
        assert!(follow_edited_in(&mut seq, &dir).unwrap().is_empty());
        assert!((seq.settings(2).unwrap().light.exposure - 0.7).abs() < 1e-9);
        let app = lapsewerk_develop::project::Project::load(&dir)
            .unwrap()
            .unwrap()
            .app
            .unwrap();
        assert!(untouched_in(&app).is_empty());
        // A rating written into an untouched keyframe's sidecar (4 stars marks keyframes in
        // another raw editor) is no edit: it keeps following.
        note_untouched(&seq, &[2], &dir).unwrap();
        let sidecar = seq.sidecar(2).unwrap();
        let text = std::fs::read_to_string(&sidecar).unwrap();
        let rated = text.replacen("<rdf:Description ", "<rdf:Description xmp:Rating=\"4\" ", 1);
        assert_ne!(rated, text);
        std::fs::write(&sidecar, rated).unwrap();
        let mut night = seq.settings(4).unwrap();
        night.light.exposure = -1.0;
        seq.save_keyframe(4, night).unwrap();
        assert_eq!(follow_edited_in(&mut seq, &dir).unwrap(), [2]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn automatic_keyframes_stay_automatic_with_photos_and_xmp_alone() {
        let dir = std::env::temp_dir().join(format!("lapsewerk-studio-marks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<std::path::PathBuf> = (0..5)
            .map(|i| {
                let p = dir.join(format!("f{i:03}.jpg"));
                let mut bytes = include_bytes!("../../develop/tests/data/frame.jpg").to_vec();
                bytes.extend_from_slice(format!("frame {i}").as_bytes());
                std::fs::write(&p, bytes).unwrap();
                p
            })
            .collect();
        let mut seq = Sequence::open(&paths).unwrap();
        for f in [0, 2, 4] {
            let d = seq.settings(f).unwrap();
            seq.save_keyframe(f, d).unwrap();
        }
        let before: Vec<String> = [0, 2, 4].iter().map(|&f| sidecar_print(&seq, f)).collect();
        note_untouched(&seq, &[0, 2, 4], &dir).unwrap();
        // Each sidecar carries its mark; the mark is no edit (the fingerprints stay).
        for (i, f) in [0, 2, 4].into_iter().enumerate() {
            assert_eq!(seq.automatic_mark(f).as_deref(), Some(before[i].as_str()));
            assert_eq!(sidecar_print(&seq, f), before[i]);
        }
        let text = std::fs::read_to_string(seq.sidecar(2).unwrap()).unwrap();
        assert_eq!(text.matches("lapsewerk:Automatic=").count(), 1, "{text}");
        // Passed on without lapsewerk.json, day and night edited in LightCraft (which keeps the
        // mark of another program): the middle still follows them.
        std::fs::remove_file(dir.join("lapsewerk.json")).unwrap();
        let mut day = seq.settings(0).unwrap();
        day.light.exposure = 1.0;
        seq.save_keyframe(0, day).unwrap();
        let mut night = seq.settings(4).unwrap();
        night.light.exposure = -2.0;
        seq.save_keyframe(4, night).unwrap();
        assert_eq!(follow_edited_in(&mut seq, &dir).unwrap(), [2]);
        assert!((seq.settings(2).unwrap().light.exposure + 0.5).abs() < 0.2);
        // Its mark follows its new settings; the edited ones' marks no longer match.
        assert_eq!(seq.automatic_mark(2), Some(sidecar_print(&seq, 2)));
        let mut marked = Vec::new();
        with_marked(&seq, &mut marked);
        assert_eq!(marked.iter().map(|(f, _)| *f).collect::<Vec<_>>(), [2]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_automatic_mark_touches_nothing_it_cannot_place_for_sure() {
        let dir = std::env::temp_dir().join(format!("lapsewerk-studio-marksafe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("f000.jpg");
        std::fs::write(&photo, include_bytes!("../../develop/tests/data/frame.jpg")).unwrap();
        let mut seq = Sequence::open(std::slice::from_ref(&photo)).unwrap();
        let d = seq.settings(0).unwrap();
        seq.save_keyframe(0, d).unwrap();
        let sidecar = seq.sidecar(0).unwrap();
        let clean = std::fs::read_to_string(&sidecar).unwrap();
        let print = sidecar_print(&seq, 0);
        let with_rights = clean.replacen(
            "</rdf:Description>",
            "<dc:rights><rdf:Alt><rdf:li xml:lang=\"x-default\">(c) Someone</rdf:li></rdf:Alt></dc:rights></rdf:Description>",
            1,
        );
        // Written some other way, or the name elsewhere: the file stays as it was, byte for byte.
        let others = [
            with_rights.replacen(
                "</rdf:Description>",
                "<lapsewerk:Automatic xmlns:lapsewerk=\"x\">abc</lapsewerk:Automatic></rdf:Description>",
                1,
            ),
            format!("<!-- lapsewerk:Automatic=\"old\" -->\n{with_rights}"),
            with_rights.replacen(
                "<rdf:Description ",
                "<rdf:Description my:note='Notiz lapsewerk:Automatic=\"x1\" Ende' ",
                1,
            ),
            with_rights.replacen(
                "<rdf:Description ",
                "<rdf:Description xmp:Label=\"lapsewerk:Automatic x\" ",
                1,
            ),
            with_rights
                .replacen("<rdf:Description ", "<rdf:Description xmlns:lapsewerk=\"x\" ", 1)
                .replacen(
                    "</rdf:Description>",
                    "<rdf:Description xmlns:lapsewerk=\"y\"/></rdf:Description>",
                    1,
                ),
        ];
        for text in &others {
            std::fs::write(&sidecar, text).unwrap();
            assert!(!seq.set_automatic_mark(0, &print).unwrap(), "{text}");
            assert_eq!(&std::fs::read_to_string(&sidecar).unwrap(), text);
        }
        // Read-only: left alone, its permissions too.
        std::fs::write(&sidecar, &with_rights).unwrap();
        let writable = std::fs::metadata(&sidecar).unwrap().permissions();
        let mut ro = writable.clone();
        ro.set_readonly(true);
        std::fs::set_permissions(&sidecar, ro).unwrap();
        assert!(!seq.set_automatic_mark(0, &print).unwrap());
        assert!(std::fs::metadata(&sidecar).unwrap().permissions().readonly());
        std::fs::set_permissions(&sidecar, writable).unwrap();
        // A plain sidecar: only the mark is added; the rights and the settings stay.
        assert!(seq.set_automatic_mark(0, &print).unwrap());
        let marked = std::fs::read_to_string(&sidecar).unwrap();
        assert!(marked.contains("(c) Someone"));
        let added = format!(
            " xmlns:lapsewerk=\"https://github.com/t-kramarczyk/lapsewerk/ns/1.0/\" lapsewerk:Automatic=\"{print}\""
        );
        assert_eq!(
            marked.replacen(&added, "", 1),
            with_rights,
            "nothing but the mark added"
        );
        assert_eq!(seq.automatic_mark(0).as_deref(), Some(print.as_str()));
        assert_eq!(sidecar_print(&seq, 0), print);
        // Set again with another print: only its value changes, the namespace once.
        assert!(seq.set_automatic_mark(0, "abc123").unwrap());
        let again = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(again.matches("xmlns:lapsewerk=").count(), 1);
        assert_eq!(again.replace("abc123", &print), marked);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_auto_transition_keeps_labels_and_leaves_protected_sidecars_alone() {
        let dir = std::env::temp_dir().join(format!("lapsewerk-studio-protect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<std::path::PathBuf> = (0..5)
            .map(|i| {
                let p = dir.join(format!("f{i:03}.jpg"));
                let mut bytes = include_bytes!("../../develop/tests/data/frame.jpg").to_vec();
                bytes.extend_from_slice(format!("frame {i}").as_bytes());
                std::fs::write(&p, bytes).unwrap();
                p
            })
            .collect();
        let mut seq = Sequence::open(&paths).unwrap();
        for f in [0, 1, 2, 4] {
            let d = seq.settings(f).unwrap();
            seq.save_keyframe(f, d).unwrap();
        }
        note_untouched(&seq, &[0, 1, 2, 4], &dir).unwrap();
        // Frame 2 carries a label LightCraft does not know; frame 1 is read-only.
        let two = seq.sidecar(2).unwrap();
        let text = std::fs::read_to_string(&two).unwrap();
        let labelled = text.replacen("<rdf:Description ", "<rdf:Description xmp:Label=\"Rot\" ", 1);
        assert_ne!(labelled, text);
        std::fs::write(&two, &labelled).unwrap();
        let one = seq.sidecar(1).unwrap();
        let before_one = std::fs::read(&one).unwrap();
        let mut ro = std::fs::metadata(&one).unwrap().permissions();
        ro.set_readonly(true);
        std::fs::set_permissions(&one, ro).unwrap();
        // Day and night edited: the others follow, but frame 1 is never written over.
        let mut day = seq.settings(0).unwrap();
        day.light.exposure = 1.0;
        seq.save_keyframe(0, day).unwrap();
        let mut night = seq.settings(4).unwrap();
        night.light.exposure = -2.0;
        seq.save_keyframe(4, night).unwrap();
        assert_eq!(follow_edited_in(&mut seq, &dir).unwrap(), [2]);
        assert_eq!(
            std::fs::read(&one).unwrap(),
            before_one,
            "read-only sidecar written over"
        );
        assert_eq!(
            untouched_kept_in(&seq, &dir),
            [1],
            "the read-only one is said as kept"
        );
        assert!(std::fs::metadata(&one).unwrap().permissions().readonly());
        let after = std::fs::read_to_string(&two).unwrap();
        assert!(after.contains("xmp:Label=\"Rot\""), "{after}");
        assert!((seq.settings(2).unwrap().light.exposure + 0.5).abs() < 0.3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_keyframe_changed_outside_measures_only_its_frames_again() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Count(AtomicUsize, std::sync::Mutex<Vec<String>>);
        impl crate::Reporter for Count {
            fn progress(&self, step: crate::Step, _: usize, total: usize) {
                if step == crate::Step::Measure {
                    self.0.store(total, Ordering::Relaxed);
                }
            }
            fn note(&self, line: &str) {
                self.1.lock().unwrap().push(line.to_owned());
            }
        }
        let dir = std::env::temp_dir().join(format!("lapsewerk-studio-again-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<std::path::PathBuf> = (0..8)
            .map(|i| {
                let p = dir.join(format!("f{i:03}.jpg"));
                let mut bytes = include_bytes!("../../develop/tests/data/frame.jpg").to_vec();
                bytes.extend_from_slice(format!("frame {i}").as_bytes());
                std::fs::write(&p, bytes).unwrap();
                p
            })
            .collect();
        let mut seq = Sequence::open(&paths).unwrap();
        for f in [0, 4, 7] {
            let d = seq.settings(f).unwrap();
            seq.save_keyframe(f, d).unwrap();
        }
        let opts = crate::MeasureSettings {
            edge: 64,
            jobs: 1,
            ..Default::default()
        };
        let mode = lapsewerk_core::keyframes::Interpolation::Linear;
        let plan = seq.plan(mode).unwrap();
        let rep = Count(AtomicUsize::new(0), std::sync::Mutex::new(Vec::new()));
        analyse_cached(&mut seq, &plan, &opts, &rep, &dir).unwrap();
        assert_eq!(rep.0.load(Ordering::Relaxed), 8);
        // The last keyframe made brighter outside by half a stop, nothing at white: frames 6-8 (from
        // 1) change in exposure only, kept moved by as much; nothing is measured again.
        let mut last = seq.settings(7).unwrap();
        last.light.exposure = 0.5;
        seq.save_keyframe(7, last).unwrap();
        let plan = seq.plan(mode).unwrap();
        let partly = analyse_cached(&mut seq, &plan, &opts, &rep, &dir).unwrap();
        assert_eq!(rep.0.load(Ordering::Relaxed), 0, "{:?}", rep.1.lock().unwrap());
        assert!(
            rep.1.lock().unwrap().iter().any(|n| n.contains(
                "in exposure only: 3 frame(s)' measurements moved by as much, nothing measured again; edited: keyframe 8"
            )),
            "{:?}",
            rep.1.lock().unwrap()
        );
        // The same as measuring all of them.
        let whole = crate::analyse(&mut seq, &plan, &opts, &crate::Quiet).unwrap();
        for (a, b) in partly.as_shot.stops.iter().zip(&whole.as_shot.stops) {
            assert!((a.unwrap() - b.unwrap()).abs() < 1e-3, "{a:?} {b:?}");
        }
        for (a, b) in partly.corrections.iter().zip(&whole.corrections) {
            assert!((a - b).abs() < 1e-3, "{a} {b}");
        }
        // Much brighter (more headroom for every frame): still only the frames it reaches, and
        // the same as measuring all of them.
        let mut last = seq.settings(7).unwrap();
        last.light.exposure = 2.5;
        seq.save_keyframe(7, last).unwrap();
        let plan = seq.plan(mode).unwrap();
        rep.0.store(0, Ordering::Relaxed);
        let partly = analyse_cached(&mut seq, &plan, &opts, &rep, &dir).unwrap();
        // Frames 7-8 brighter by more than a stop measured again; 6 (two thirds of one) kept.
        assert_eq!(rep.0.load(Ordering::Relaxed), 2, "{:?}", rep.1.lock().unwrap());
        let whole = crate::analyse(&mut seq, &plan, &opts, &crate::Quiet).unwrap();
        // Measured with another headroom: the same to rounding (a millionth of a stop).
        for (a, b) in partly.corrections.iter().zip(&whole.corrections) {
            assert!(
                (a - b).abs() < 1e-5,
                "{:?} {:?}",
                partly.corrections,
                whole.corrections
            );
        }
        // Smooth keyframes: the night's end made brighter moves the frames before the keyframe
        // before it too, by a little (the curve's slope there): those keep their measurement,
        // moved by as much, and the result is the same as measuring all of them.
        let smooth = lapsewerk_core::keyframes::Interpolation::Smooth;
        for (f, e) in [(4, 1.0), (7, 1.2)] {
            let mut d = seq.settings(f).unwrap();
            d.light.exposure = e;
            seq.save_keyframe(f, d).unwrap();
        }
        let plan = seq.plan(smooth).unwrap();
        analyse_cached(&mut seq, &plan, &opts, &crate::Quiet, &dir).unwrap();
        let before = plan;
        let mut last = seq.settings(7).unwrap();
        last.light.exposure = 1.4;
        seq.save_keyframe(7, last).unwrap();
        let plan = seq.plan(smooth).unwrap();
        let moved = plan.settings(2, 0.0).light.exposure - before.settings(2, 0.0).light.exposure;
        assert!(
            moved.abs() > 1e-6,
            "the smooth curve moves the frames before keyframe 5"
        );
        rep.0.store(0, Ordering::Relaxed);
        rep.1.lock().unwrap().clear();
        let partly = analyse_cached(&mut seq, &plan, &opts, &rep, &dir).unwrap();
        assert!(rep.0.load(Ordering::Relaxed) <= 1, "{:?}", rep.1.lock().unwrap());
        let whole = crate::analyse(&mut seq, &plan, &opts, &crate::Quiet).unwrap();
        for (a, b) in partly.corrections.iter().zip(&whole.corrections) {
            assert!(
                (a - b).abs() < 1e-3,
                "{:?} {:?}",
                partly.corrections,
                whole.corrections
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_photo_changed_in_any_byte_is_told_apart() {
        let dir = std::env::temp_dir().join(format!("lapsewerk-studio-ends-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.raw");
        // Bytes that are no UTF-8 (they all read as one replacement character as text), and the
        // middle of a large file (a raw's sensor data).
        let mut bytes = vec![0xffu8; 4000 * 1024];
        std::fs::write(&p, &bytes).unwrap();
        let before = ends_print(&p);
        bytes[1000] = 0xfe;
        std::fs::write(&p, &bytes).unwrap();
        let first = ends_print(&p);
        assert_ne!(before, first);
        // In a large file, one of the pieces read (the middle ones fall on a raw's sensor data).
        let step = (bytes.len() - 16 * 1024) / 63;
        bytes[31 * step + 100] = 0xfe;
        std::fs::write(&p, &bytes).unwrap();
        assert_ne!(first, ends_print(&p));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_implausible_analysis_is_measured_anew() {
        let mut a = sample();
        let mut v = to_json(&a);
        assert!(from_json(&v).is_some());
        v["scene"][1] = serde_json::json!(1e9);
        assert!(from_json(&v).is_none());
        a.scene[0] = f64::NAN;
        assert!(from_json(&to_json(&a)).is_some());
    }

    #[test]
    fn an_analysis_reads_back_as_it_was_written() {
        let a = sample();
        let b = from_json(&serde_json::from_str(&to_json(&a).to_string()).unwrap()).unwrap();
        assert_eq!(b.ramp, a.ramp);
        assert_eq!(b.checks, a.checks);
        assert_eq!(b.as_shot.stops, a.as_shot.stops);
        assert_eq!(b.as_shot.failures, a.as_shot.failures);
        assert_eq!(b.as_shot.tiles, a.as_shot.tiles);
        assert_eq!(b.as_shot.sizes, a.as_shot.sizes);
        assert_eq!(b.as_shot.clipped, a.as_shot.clipped);
        assert_eq!(b.motion, a.motion);
        assert!(b.scene[1].is_nan() && (b.scene[0] - -1.5).abs() < 1e-12);
        assert_eq!(b.video_trend, a.video_trend);
    }

    fn sample() -> Analysis {
        Analysis {
            exif_ramp: vec![0.0, -0.33],
            ramp: vec![0.0, 0.33],
            checks: vec![StepCheck {
                frame: 1,
                reported: -0.33,
                measured: None,
                used: -0.33,
            }],
            as_shot: Pass {
                stops: vec![Some(-1.5), None],
                raw: vec![None, None],
                failures: vec![(1, "broken".into())],
                perceived: vec![Some(-1.0), None],
                tiles: vec![vec![Some(0.5), None], Vec::new()],
                thumbs: Vec::new(),
                sizes: vec![Some((900, 600)), None],
                clipped: vec![Some(0.25), None],
                time: Duration::from_secs(3),
            },
            compensated: vec![Some(-1.5), None],
            corrections: vec![0.01, 0.0],
            motion: Some(vec![(0.5, -0.25), (0.0, 0.0)]),
            scene: vec![-1.5, f64::NAN],
            video_trend: vec![-1.5, -1.6],
            edits: vec![0.0, 0.0],
        }
    }
}
