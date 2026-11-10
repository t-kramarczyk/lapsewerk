//! The movie: every frame rendered at full quality as 16-bit sRGB on worker threads and handed to
//! the encoder in order.
//!
//! At most `jobs + 2` frames exist at a time (rendering or waiting for their turn), so memory stays
//! bounded however long the sequence is.

use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use lapsewerk_core::temporal::Image16;
use lapsewerk_develop::{DeepSamples, DevelopPlan, Output, Sequence};
use lapsewerk_video::Encoder;

use crate::stage::Stage;
use crate::{CANCELLED, RenderArgs, Reporter, Step};

/// A rendered frame: 16-bit RGB samples, width, height.
type Frame = (Vec<u16>, usize, usize);

struct State {
    /// Next frame a worker takes.
    next: usize,
    /// Frames handed to the encoder.
    written: usize,
    done: BTreeMap<usize, Result<Frame, String>>,
    abort: bool,
}

/// What is corrected frame by frame.
#[derive(Clone, Copy)]
pub struct Corrections<'a> {
    /// Exposure offsets in stops (ramp, deflicker, day to night), per frame.
    pub offsets: &'a [f64],
    /// The part of `offsets` showing a frame brighter than its light (see
    /// [`crate::Corrected::brightening`]): what holding the blacks takes back.
    pub brightening: &'a [f64],
    /// Sensor dust to remove.
    pub dust: Option<&'a lapsewerk_core::dust::DustMap>,
    /// Regional deflicker, per frame and tile of `RenderArgs::regional`.
    pub regional: Option<&'a [Vec<f64>]>,
    /// Stabilization: how far to move the crop, per frame (thumb pixels).
    pub motion: Option<&'a [(f64, f64)]>,
}

/// Renders the movie. `measured_size` is a measuring render's size: its aspect ratio decides how the
/// frames are scaled to cover the video size; the excess is cropped at `args.position`.
///
/// # Errors
/// When a frame cannot be rendered or written, or [`CANCELLED`] (the partial movie is deleted).
pub fn render(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    fix: &Corrections<'_>,
    measured_size: (usize, usize),
    args: &RenderArgs,
    rep: &dyn Reporter,
) -> Result<std::path::PathBuf, String> {
    let Corrections {
        offsets,
        brightening,
        dust,
        regional,
        motion,
    } = *fix;
    let n = seq.len();
    let v = args.video;
    let (mw, mh) = measured_size;
    if mw == 0 || mh == 0 {
        return Err("frame 0 rendered empty".into());
    }
    let (box_w, box_h) = work_box(args, mw, mh, motion.is_some());
    // What each frame is worked at (the photo's shape around the video's): within what fits in
    // memory, or refused here rather than ended by the system halfway.
    if box_w.saturating_mul(box_h) > 120_000_000 {
        return Err(format!(
            "frames of {}x{} would be worked at {box_w}x{box_h} (the photos' shape around it), more than fits in memory; choose a size closer to the photos' shape",
            v.width, v.height
        ));
    }
    // Resuming what can't be resumed: refused before the folder is touched (its settings file
    // stays, so a finished delivery is not taken for a stopped one).
    if args.resume
        && !(args.denoise.is_none()
            && args.blend <= 1
            && args.astro == crate::stage::AstroParams::default()
            && args.tween == 0)
    {
        return Err("resuming works without denoise, blend, star work and in-between frames (they use neighbouring frames over a window)".into());
    }
    // The output: a movie, or a folder of 16-bit TIFF frames (on any system; resumable).
    let mut sink = if args.sequence {
        let w = lapsewerk_video::FrameWriter::create_as(
            &args.out,
            v,
            args.frame_format,
            args.overwrite,
            args.resume,
            &manifest(args, plan, offsets, n),
        )
        .map_err(|e| e.to_string())?;
        // Pictures of another name there (another delivery): kept, but not this one's.
        let stem = format!("{}_", lapsewerk_video::frames::frame_stem(&args.out));
        let strangers = lapsewerk_video::frames::other_pictures(
            &args.out,
            &lapsewerk_video::frames::frame_stem(&args.out),
        );
        if !strangers.is_empty() {
            // Named (the first few): which files are meant is then plain.
            let more = strangers.len().saturating_sub(5);
            let shown = strangers.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
            rep.note(&format!(
                "warning:       {} other picture file(s) in {} (not named {stem}…): {shown}{}; kept, but not part of this delivery; a folder of its own is clearer",
                strangers.len(),
                args.out.display(),
                if more > 0 { format!(" and {more} more") } else { String::new() }
            ));
        }
        for lock in lapsewerk_video::frames::taken_over_locks() {
            rep.note(&format!("lock:          a left-over lock taken over: {lock}"));
        }
        if w.removed() > 0 {
            rep.note(&format!(
                "replaced:      {} frame(s) of an earlier render in {} removed",
                w.removed(),
                args.out.display()
            ));
        }
        Sink::Frames(w)
    } else {
        if args.resume {
            return Err("resuming needs a frame sequence (--sequence)".into());
        }
        // The folder it goes into (by default one beside the photos) may not exist yet.
        if let Some(d) = args.out.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        Sink::Movie(Encoder::create(&args.out, v, args.overwrite).map_err(|e| e.to_string())?)
    };
    // Resuming: the frames there are kept, and rendering goes on where the next missing frame
    // comes from. Frames that blend their neighbours over a window (denoise, blend, star work,
    // in-between frames) would come out differently; repairs are fine: rendering starts at the
    // last good frame before the gap, so a replaced frame or a filled gap has its neighbours.
    // Resuming a sequence that is complete: nothing to render.
    let total = n + args.repair.fill.iter().map(|(_, m)| m).sum::<usize>();
    if let Sink::Frames(w) = &sink
        && args.resume
        && args.tween == 0
        && w.next() >= total
    {
        let there = w.next();
        // Frames past the end (one too many, from somewhere else): gone, so the edit gets this
        // render's frames only; named.
        if let Sink::Frames(w) = &mut sink {
            w.restart_at(total);
            note_past_end(w, rep, &[]);
        }
        // The delivery's own INFO.txt (its repairs, what made it, when) stays as it was; only a
        // folder without one gets it written.
        let info = args.out.join(lapsewerk_video::frames::INFO_FILE);
        let kept = lapsewerk_video::frames::read_text(&info).ok();
        let path = sink.finish()?;
        match kept {
            Some(text) if text.contains("made by") => {
                lapsewerk_video::frames::write_whole(&info, &text).map_err(|e| info_failed(&info, &e))?;
            }
            _ => note_delivery(&path, seq, args, "")?,
        }
        // The sequence's own length (frames past it were just named and removed).
        let _ = there;
        rep.note(&format!(
            "resume:        all {total} frames are there already: nothing to render ({})",
            path.display()
        ));
        return Ok(path);
    }
    // Frames of this render found edited and set aside on resuming: said as "changed" there, not
    // again as someone else's files.
    let mut said_aside: Vec<String> = Vec::new();
    let start = match &mut sink {
        Sink::Frames(w) if w.next() > 0 || !w.damaged_all().is_empty() => {
            let alone = args.denoise.is_none()
                && args.blend <= 1
                && args.astro == crate::stage::AstroParams::default()
                && args.tween == 0;
            if !alone {
                return Err("resuming works without denoise, blend, star work and in-between frames (they use neighbouring frames over a window)".into());
            }
            let there = w.next();
            let stem = lapsewerk_video::frames::frame_stem(&args.out);
            let path_of =
                |d: usize| lapsewerk_video::frames::frame_path_as(&args.out, &stem, d, args.frame_format);
            let name_of = |d: usize| {
                path_of(d)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            // Each by name: those gone (deleted in a copy) apart from those damaged or cut short.
            // A whole frame not as written (retouched) was set aside as `….kept` just now: changed,
            // not gone.
            let aside = |d: usize| {
                let kept = format!("{}.kept", name_of(d));
                w.kept_foreign().iter().any(|k| k.starts_with(&kept))
            };
            let (changed, rest): (Vec<usize>, Vec<usize>) = w.damaged_all().iter().partition(|&&d| aside(d));
            let (missing, damaged): (Vec<usize>, Vec<usize>) =
                rest.iter().partition(|&&d| !path_of(d).exists());
            if !changed.is_empty() {
                let names: Vec<String> = changed.iter().map(|&d| name_of(d)).collect();
                said_aside.extend(names.iter().map(|n| format!("{n}.kept")));
                rep.note(&format!(
                    "changed:       {} frame(s) not as written (edited?): {}; set aside as .kept, made again",
                    names.len(),
                    names.join(", ")
                ));
            }
            if damaged.is_empty() && missing.is_empty() {
                if let Some(d) = w.damaged().filter(|_| changed.is_empty()) {
                    rep.note(&format!(
                        "damaged:       frame {} ({}) was damaged or cut short: made again from there",
                        d + 1,
                        name_of(d)
                    ));
                }
            } else {
                if !damaged.is_empty() {
                    let names: Vec<String> = damaged.iter().map(|&d| name_of(d)).collect();
                    rep.note(&format!(
                        "damaged:       {} frame(s) damaged or cut short: {}; made again",
                        names.len(),
                        names.join(", ")
                    ));
                }
                if !missing.is_empty() {
                    let names: Vec<String> = missing.iter().map(|&d| name_of(d)).collect();
                    rep.note(&format!(
                        "missing:       {} frame(s) gone from the folder: {}; made again",
                        names.len(),
                        names.join(", ")
                    ));
                }
            }
            let (from, written) = resume_point(&args.repair, n, there);
            w.restart_at(written);
            rep.note(&format!(
                "resume:        frames 1-{there} whole, rendering on from photo {} (video frame {})",
                from + 1,
                written + 1
            ));
            from
        }
        _ => 0,
    };
    let run_when = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z").to_string();
    // Whole frames after a damaged one are kept, not made again (only the damaged ones are): where
    // a photo is a video frame (no repairs, no in-between frames) and nothing hangs on each frame's
    // measured light (PQ's HDR10 figures).
    let keep: std::collections::HashSet<usize> = match &sink {
        Sink::Frames(w)
            if args.repair.replace.is_empty()
                && args.repair.fill.is_empty()
                && args.tween == 0
                && !matches!(v.transfer, lapsewerk_video::Transfer::Pq { .. }) =>
        {
            w.keepable().iter().copied().collect()
        }
        _ => std::collections::HashSet::new(),
    };
    if !keep.is_empty() {
        rep.note(&format!(
            "resume:        {} whole frame(s) after it kept, not made again (only the damaged and missing ones are)",
            keep.len()
        ));
    }
    // This run, from where it writes: a delivery made in two runs names both in its INFO.txt.
    // Frames kept between those it makes stay the earlier run's: each stretch it makes is noted,
    // and the earlier run again after it.
    if let Sink::Frames(w) = &sink {
        let earlier = lapsewerk_video::frames::runs(&args.out);
        let made = made_by(seq, &args.out);
        let mut i = w.next();
        while i < n {
            if keep.contains(&i) {
                i += 1;
                continue;
            }
            let start_at = i;
            while i < n && !keep.contains(&i) {
                i += 1;
            }
            // Noting a run from `start_at` drops the notes after it: the earlier runs there come
            // back after this stretch, each from where it began (three resumes keep all four).
            lapsewerk_video::frames::note_run(&args.out, start_at, &run_when, &made);
            if i < n
                && let Some((_, when, by)) = earlier.iter().rev().find(|(f, _, _)| *f <= i)
            {
                lapsewerk_video::frames::note_run(&args.out, i, when, by);
            }
            for (f, when, by) in earlier.iter().filter(|(f, _, _)| *f > i) {
                lapsewerk_video::frames::note_run(&args.out, *f, when, by);
            }
        }
    }
    let mut jobs = Vec::with_capacity(n);
    // HDR: rendered with LightCraft's HDR headroom up to the signal's peak, Rec. 2020, and carried
    // through the stages as `hdr::encode_frame` makes it (the encoder's table makes the signal).
    let hdr_peak = match v.transfer {
        lapsewerk_video::Transfer::Pq { peak } | lapsewerk_video::Transfer::Hlg { peak } => Some(peak),
        _ => None,
    };
    for i in 0..n {
        let mut settings = plan.settings(i, offsets.get(i).copied().unwrap_or(0.0));
        let output = match hdr_peak {
            Some(peak) => {
                settings.hdr.enabled = true;
                settings.hdr.max_ev = f64::from(peak).log2();
                Output::HdrRec2020
            }
            None => Output::Display16(lapsewerk_develop::OutputSpace::Srgb),
        };
        // Frames there already (resuming) are not rendered.
        let job = if i < start || keep.contains(&i) {
            None
        } else {
            Some(
                seq.render_job(i, settings, box_w, box_h, output)
                    .map_err(|e| e.to_string())?,
            )
        };
        jobs.push(Mutex::new(job));
    }

    let encoder = &mut sink;
    let window = args.jobs + 2;
    let state = Mutex::new(State {
        next: start,
        written: start,
        done: BTreeMap::new(),
        abort: false,
    });
    let changed = Condvar::new();
    let started = Instant::now();
    let mut error = None;
    let mut stage = Stage::new(args.denoise, args.blend, args.astro, &args.repair, args.tween);
    let mut sky_frame = None;
    if stage.wants_sky() {
        let found = find_sky_at(seq, plan, offsets, measured_size, args);
        sky_frame = found.as_ref().map(|(_, i)| *i);
        // Trails over a sky that moves in the frame keep to the area given as well.
        let moving = args.astro.trails.is_some()
            && found
                .as_ref()
                .is_some_and(|(m, i)| sky_moves(seq, plan, offsets, measured_size, args, m, *i));
        stage.set_sky(found.map(|(m, _)| m));
        stage.set_sky_moving(moving);
    }

    std::thread::scope(|scope| {
        for _ in 0..args.jobs {
            scope.spawn(|| {
                loop {
                    let i = {
                        let Ok(mut s) = state.lock() else { return };
                        while !s.abort && s.next < n && s.next >= s.written + window {
                            s = match changed.wait(s) {
                                Ok(s) => s,
                                Err(_) => return,
                            };
                        }
                        if s.abort || s.next >= n {
                            return;
                        }
                        s.next += 1;
                        s.next - 1
                    };
                    // Kept as it is: nothing to render (the writer keeps it in turn).
                    if keep.contains(&i) {
                        continue;
                    }
                    let job = jobs.get(i).and_then(|j| j.lock().ok().and_then(|mut j| j.take()));
                    let frame = match job {
                        // A crash in one frame must not leave the writer waiting for it forever.
                        Some(job) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.run()))
                            .map_err(|_| "the renderer crashed".to_owned())
                            .and_then(|done| frame_of(done.rendered, hdr_peak)),
                        None => Err("render job missing".into()),
                    };
                    if let Ok(mut s) = state.lock() {
                        s.done.insert(i, frame);
                    }
                    changed.notify_all();
                }
            });
        }

        // Counted from where this run starts, so a resume's speed and time left are right; frames
        // kept as they are count neither (the count is of frames made).
        let to_make = (start..n).filter(|i| !keep.contains(i)).count();
        let mut made = 0;
        rep.progress(Step::Render, 0, to_make);
        for i in start..n {
            if rep.cancelled() {
                if let Ok(mut s) = state.lock() {
                    s.abort = true;
                }
                changed.notify_all();
                error = Some(CANCELLED.to_owned());
                break;
            }
            if keep.contains(&i) {
                let kept = encoder.keep();
                if let Ok(mut s) = state.lock() {
                    s.written = i + 1;
                    if kept.is_err() {
                        s.abort = true;
                    }
                }
                changed.notify_all();
                if let Err(e) = kept {
                    error = Some(e);
                    break;
                }
                continue;
            }
            let frame = {
                let Ok(mut s) = state.lock() else {
                    error = Some("worker state poisoned".to_owned());
                    break;
                };
                loop {
                    if let Some(f) = s.done.remove(&i) {
                        break Some(f);
                    }
                    s = match changed.wait(s) {
                        Ok(s) => s,
                        Err(_) => break None,
                    };
                }
            };
            let result = match frame {
                Some(Ok((mut rgb, w, h))) => {
                    // Dust first: it sits on the sensor, in the whole frame, before anything moves.
                    if let Some(map) = dust {
                        map.apply_srgb16(&mut rgb, w, h);
                    }
                    // Black stays black in a frame shown brighter than shot (before anything blends).
                    if args.hold_blacks
                        && let Some(&o) = brightening.get(i)
                        && o > 0.0
                    {
                        let pedestal =
                            lapsewerk_core::blacks::pedestal(o, lapsewerk_core::blacks::darkest(&rgb, w));
                        lapsewerk_core::blacks::hold(&mut rgb, pedestal, hdr_peak.map_or(1.0, f64::from));
                    }
                    // Regional deflicker on the whole render, so tiles keep their place when cropping.
                    if let (Some(grid), Some(stops)) = (args.regional, regional.and_then(|r| r.get(i))) {
                        lapsewerk_core::regional::apply_srgb16(&mut rgb, w, h, grid, stops);
                    }
                    if w < v.width || h < v.height {
                        Err(format!(
                            "frame {} renders {w}×{h}, smaller than the {}×{} video",
                            i + 1,
                            v.width,
                            v.height
                        ))
                    } else {
                        if let Some(mv) = args.framing {
                            let view = mv.at_frame(i, n);
                            let aspect = v.width as f64 / v.height as f64;
                            let mut win = lapsewerk_core::framing::window(view, w as f64, h as f64, aspect);
                            if let Some(c) = motion.and_then(|m| m.get(i)) {
                                let scale = lapsewerk_core::stabilize::SIZE as f64;
                                win.left =
                                    (win.left - c.0 * w as f64 / scale).clamp(0.0, w as f64 - win.width);
                                win.top =
                                    (win.top - c.1 * h as f64 / scale).clamp(0.0, h as f64 - win.height);
                            }
                            lapsewerk_core::framing::resample(&rgb, w, h, win, v.width, v.height)
                                .and_then(|data| Image16::new(v.width, v.height, data))
                                .ok_or_else(|| format!("frame {}: cannot scale the view", i + 1))
                                .and_then(|frame| {
                                    stage
                                        .push(i, frame)
                                        .into_iter()
                                        .try_for_each(|f| encoder.push(&f.data, f.width))
                                })
                        } else {
                            let mut left = offset(w - v.width, args.position.0);
                            let mut top = offset(h - v.height, args.position.1);
                            // Stabilization: the crop moves against the shake (thumb pixels scaled to this
                            // render), by whole pixels (no resampling, so no sharpness changing frame to
                            // frame), never past the frame.
                            let shake = motion.and_then(|m| m.get(i)).map(|c| {
                                let scale = lapsewerk_core::stabilize::SIZE as f64;
                                (c.0 * w as f64 / scale, c.1 * h as f64 / scale)
                            });
                            // Sub-pixel (asked for): the exact shift, resampled 1:1 (Lanczos). Steadier,
                            // at the cost of a slightly softer picture that varies a little frame to frame.
                            let sub = shake.filter(|_| args.subpixel).map(|(dx, dy)| {
                                let win = lapsewerk_core::framing::Window {
                                    left: (left as f64 - dx).clamp(0.0, (w - v.width) as f64),
                                    top: (top as f64 - dy).clamp(0.0, (h - v.height) as f64),
                                    width: v.width as f64,
                                    height: v.height as f64,
                                };
                                lapsewerk_core::framing::resample(&rgb, w, h, win, v.width, v.height)
                                    .and_then(|data| Image16::new(v.width, v.height, data))
                                    .ok_or_else(|| format!("frame {}: cannot shift the crop", i + 1))
                            });
                            if let Some((dx, dy)) = shake.filter(|_| !args.subpixel) {
                                left = left
                                    .saturating_add_signed(-(dx.round() as isize))
                                    .min(w - v.width);
                                top = top
                                    .saturating_add_signed(-(dy.round() as isize))
                                    .min(h - v.height);
                            }
                            sub.unwrap_or_else(|| crop(&rgb, w, left, top, v.width, v.height))
                                .and_then(|frame| {
                                    stage
                                        .push(i, frame)
                                        .into_iter()
                                        .try_for_each(|f| encoder.push(&f.data, f.width))
                                })
                        }
                    }
                }
                Some(Err(e)) => Err(format!("frame {}: {e}", i + 1)),
                None => Err("worker state poisoned".into()),
            };
            if let Ok(mut s) = state.lock() {
                s.written = i + 1;
                if result.is_err() {
                    s.abort = true;
                }
            }
            changed.notify_all();
            if let Err(e) = result {
                error = Some(e);
                break;
            }
            made += 1;
            rep.progress(Step::Render, made, to_make);
        }
    });

    if let Some(e) = error {
        // Dropping the encoder deletes the partial movie.
        return Err(e);
    }
    for f in stage.finish() {
        encoder.push(&f.data, f.width)?;
    }
    let astro = args.astro;
    if args.denoise.is_some() || args.blend > 1 || astro != crate::stage::AstroParams::default() {
        let what = [
            astro.light_pollution.map(|(_, amount)| {
                let missed = if stage.unfitted > 0 {
                    format!(
                        " ({} frames could not be fitted, left as they were)",
                        stage.unfitted
                    )
                } else {
                    String::new()
                };
                format!("light pollution {:.0} % out{missed}", amount * 100.0)
            }),
            astro.reduce_stars.map(|amount| {
                format!(
                    "stars dimmed by {:.0} % ({:.0} a frame)",
                    amount * 100.0,
                    stage.stars as f64 / n as f64
                )
            }),
            astro.trails.map(|t| {
                if t == 0 {
                    "star trails (endless)".to_owned()
                } else {
                    format!("star trails fading over {t} frames")
                }
            }),
            args.denoise.map(|d| {
                format!(
                    "denoise {} frames each side (tolerance {})",
                    d.radius, d.tolerance
                )
            }),
            (args.blend > 1).then(|| format!("blend {} frames", args.blend)),
            (astro.light_pollution.is_some() || astro.reduce_stars.is_some() || astro.trails.is_some()).then(
                || {
                    stage.sky_share().map_or_else(
                        || "no sky found (looked in 5 frames): in the sky area given".to_owned(),
                        |c| match sky_frame.filter(|i| *i > 0) {
                            // Found in a later frame: said, as a camera that moves would want to know.
                            Some(i) => format!(
                                "in the sky found in frame {} (none in the first): {:.0} % of the frame",
                                i + 1,
                                c * 100.0
                            ),
                            None => format!("in the sky found ({:.0} % of the frame)", c * 100.0),
                        },
                    )
                },
            ),
        ];
        if stage.sky_moved() {
            rep.note(if astro.trails_area.is_some() {
                "note:          the sky lies elsewhere in later frames (the camera pans or tracks the stars): star trails keep within the area given (--sky) as well as the first frame's sky"
            } else {
                "warning:       the sky lies elsewhere in later frames (the camera pans or tracks the stars): star trails form in the first frame's sky, foreground lights that move into it leave trails; --sky (top share) keeps them out"
            });
        }
        rep.note(&format!(
            "frame stage:   {}; {:.2} s/frame",
            what.into_iter().flatten().collect::<Vec<_>>().join(", "),
            stage.time.as_secs_f64() / n as f64
        ));
    }
    // Repairs can add frames (gaps filled): the movie's length is what was written.
    let written = usize::try_from(sink.frames()).unwrap_or(n);
    if !args.repair.is_empty() {
        rep.note(&format!(
            "repairs:       {} odd frame(s) replaced, {} frame(s) put into gaps",
            args.repair.replace.len(),
            args.repair.fill.iter().map(|(_, m)| m).sum::<usize>()
        ));
    }
    let light = sink.light_level();
    let total = match &sink {
        Sink::Frames(w) => w.frames_total() as u64,
        Sink::Movie(e) => e.frames(),
    };
    if let Sink::Frames(w) = &sink {
        note_past_end(w, rep, &said_aside);
    }
    let path = sink.finish()?;
    if args.sequence {
        note_delivery(&path, seq, args, &run_when)?;
    }
    if let Some(l) = light {
        rep.note(&format!(
            "HDR10 light:   MaxCLL {} cd/m², MaxFALL {} cd/m² (measured over {} of {} frame(s){}{})",
            l.max_cll,
            l.max_fall,
            l.frames,
            total,
            if l.frames < total {
                "; INCOMPLETE: frames of an earlier render have no light log, --overwrite renders and measures all"
            } else {
                ""
            },
            if args.sequence {
                "; in HDR10.txt beside the frames, for the grading or delivery tool"
            } else {
                "; written into the movie"
            }
        ));
    }
    let elapsed = started.elapsed().as_secs_f64();
    rep.note(&format!(
        "{}{} ({}×{}, {} frame{}, {:.1} s; rendered in {elapsed:.1} s, {:.2} s/frame with {} job(s))",
        if args.sequence {
            "frames:        "
        } else {
            "movie:         "
        },
        path.display(),
        v.width,
        v.height,
        written,
        if written == 1 { "" } else { "s" },
        written as f64 * f64::from(v.fps_den) / f64::from(v.fps_num),
        elapsed / n as f64,
        args.jobs
    ));
    if args.sequence {
        let curve = match (args.frame_format, v.transfer) {
            (lapsewerk_video::frames::FrameFormat::Exr, t) => {
                if t.is_hdr() {
                    "linear display light, Rec. 2020"
                } else {
                    "linear display light, Rec. 709"
                }
            }
            (_, t) => match t {
                lapsewerk_video::Transfer::Srgb => "sRGB",
                lapsewerk_video::Transfer::Rec709 => "Rec. 709 (BT.1886)",
                lapsewerk_video::Transfer::Pq { .. } => "PQ, Rec. 2020",
                lapsewerk_video::Transfer::Hlg { .. } => "HLG, Rec. 2020",
            },
        };
        rep.note(&format!(
            "delivery:      {} frames, {:.3} fps ({}/{}), timecode {}, curve {curve}; all details in {} beside them",
            args.frame_format.name(),
            f64::from(v.fps_num) / f64::from(v.fps_den.max(1)),
            v.fps_num,
            v.fps_den,
            timecode_range(&v, written),
            lapsewerk_video::frames::INFO_FILE
        ));
    } else {
        rep.note(&format!("delivery:      {}", movie_delivery(&v, written)));
        // A timecode is asked for an edit or a broadcast, which expect Rec. 709: sRGB is the
        // default for HEVC (to share), said here rather than found in the grade.
        if v.timecode.is_some() && matches!(v.transfer, lapsewerk_video::Transfer::Srgb) {
            rep.note(
                "note:          the curve is sRGB (the default for sharing); an edit or broadcast with this timecode most likely expects Rec. 709: --transfer rec709",
            );
        }
    }
    Ok(path)
}

/// The photo a resume renders from and the video frame that photo becomes, when `there` video
/// frames exist: repairs put frames into gaps (shifting the numbers) and replace frames from their
/// neighbours, so it starts at the last good photo (neither replaced nor after a gap) whose video
/// frame is not past those there.
fn resume_point(repair: &crate::stage::RepairParams, n: usize, there: usize) -> (usize, usize) {
    let fill: std::collections::HashMap<usize, usize> = repair.fill.iter().copied().collect();
    // Video frame of each photo: its index plus the frames put into gaps before it.
    let mut video = Vec::with_capacity(n);
    let mut added = 0;
    for i in 0..n {
        if i > 0 {
            added += fill.get(&i).copied().unwrap_or(0);
        }
        video.push(i + added);
    }
    let first_missing = video.iter().position(|&o| o >= there).unwrap_or(n);
    let mut from = first_missing.min(n.saturating_sub(1));
    while from > 0 && (repair.replace.contains(&from) || fill.contains_key(&from)) {
        from -= 1;
    }
    (from, video.get(from).copied().unwrap_or(there).min(there))
}

/// What a frame render with `args` keeps beside its frames, to tell whether frames there were made
/// with the same settings ([`lapsewerk_video::frames::render_there`]).
#[must_use]
pub fn frames_manifest(args: &RenderArgs, plan: &DevelopPlan, offsets: &[f64], n: usize) -> String {
    manifest(args, plan, offsets, n)
}

/// A fingerprint of everything that decides how the frames look: the arguments (not where they go
/// or how many jobs), the exposure offsets, and the keyframes' settings.
fn manifest(args: &RenderArgs, plan: &DevelopPlan, offsets: &[f64], n: usize) -> String {
    let mut r = args.clone();
    r.out = std::path::PathBuf::new();
    (r.overwrite, r.resume, r.jobs) = (false, false, 0);
    // What only decides the offsets (which are in the text themselves) or never touches a pixel:
    // a render started in the app goes on from the command line or the queue, and the other way
    // round, whether it verified or how it arrived at the same offsets.
    (r.verify, r.passes, r.deflicker, r.keep_light) = (false, 0, None, None);
    (r.video.codec, r.video.bitrate) = (lapsewerk_video::Codec::Hevc, None);
    // The switches that lead to repairs: what they lead to is in `repair` itself.
    (r.replace_outliers, r.fill_gaps) = (false, false);
    let mut text = format!("{r:?} frames {n}");
    for &k in plan.keyframes() {
        text.push_str(&format!(" {k}:{:?}", plan.settings(k, 0.0)));
    }
    // The offsets beside the fingerprint, compared with a tolerance (the app and the command line
    // measure the same frames on different threads and agree to about a millionth of a stop).
    let offsets: Vec<String> = offsets.iter().map(|o| format!("{o:.5}")).collect();
    format!(
        "{} {}{}\n{}",
        lapsewerk_video::frames::fingerprint(&text),
        lapsewerk_video::frames::OFFSETS_MARK,
        offsets.join(","),
        settings_in_words(args, plan, n)
    )
}

/// The render's settings in words, one `name: value` a line, for a refused resume to say which
/// differ (a keyframe by its number and a fingerprint of its settings: changed in LightCraft?).
fn settings_in_words(args: &RenderArgs, plan: &DevelopPlan, n: usize) -> String {
    let mut lines = args_in_words(args, n);
    for &k in plan.keyframes() {
        lines.push(format!(
            "keyframe {}: {}",
            k + 1,
            lapsewerk_video::frames::fingerprint(&format!("{:?}", plan.settings(k, 0.0)))
        ));
    }
    lines.join("\n")
}

/// The settings a resume is refused for that are known before any photo is measured: those in the
/// render's fingerprint that come from the settings alone (not the frame count, repairs or
/// keyframes, which the analysis decides or the writer compares later). Deflicker, keep light and
/// passes are left out: they only decide the offsets, which are compared with a tolerance.
const EARLY_SETTINGS: [&str; 11] = [
    "size",
    "frame rate",
    "timecode",
    "curve",
    "in-between frames",
    "position",
    "hold blacks",
    "stabilize",
    "camera move",
    "dust",
    "regional",
];

/// The render's settings known before measuring, in words as the frames' settings file has them,
/// for [`lapsewerk_video::frames::resume_preflight`]: a resume of another size, frame rate,
/// timecode or curve is refused before minutes of measuring.
#[must_use]
pub fn early_settings_words(args: &RenderArgs) -> String {
    args_in_words(args, 0)
        .into_iter()
        .filter(|l| {
            l.split_once(": ")
                .is_some_and(|(k, _)| EARLY_SETTINGS.contains(&k))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The render's settings in words but its keyframes, one `name: value` a line.
fn args_in_words(args: &RenderArgs, n: usize) -> Vec<String> {
    let v = &args.video;
    vec![
        format!("size: {}x{}", v.width, v.height),
        format!("frame rate: {}/{}", v.fps_num, v.fps_den),
        format!(
            "timecode: {}",
            v.timecode.map_or_else(
                || "none".to_owned(),
                |t| lapsewerk_video::frames::timecode_text(t, (v.fps_num, v.fps_den), v.drop_frame)
            )
        ),
        format!("curve: {}", transfer_words(v.transfer)),
        format!("frames: {n}"),
        format!(
            "deflicker: {}",
            args.deflicker
                .map_or_else(|| "off".to_owned(), |d| format!("{d}"))
        ),
        format!(
            "keep light: {}",
            args.keep_light
                .map_or_else(|| "off".to_owned(), |k| format!("{k}"))
        ),
        format!(
            "hand-drawn brightness: {}",
            if args.brightness.is_empty() {
                "none".to_owned()
            } else {
                args.brightness
                    .iter()
                    .map(|(f, v)| format!("frame {} {v:+.2}", f + 1))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
        format!("in-between frames: {}", args.tween),
        format!("repairs: {}", repairs_in_words(&args.repair)),
        format!("passes: {}", args.passes),
        format!("position: {:.3},{:.3}", args.position.0, args.position.1),
        format!("hold blacks: {}", if args.hold_blacks { "on" } else { "off" }),
        format!("stabilize: {}", if args.stabilize { "on" } else { "off" }),
        format!(
            "camera move: {}",
            args.framing.map_or_else(
                || "none".to_owned(),
                |m| {
                    // A pan alone is said as one ("zoom 1.30 to 1.30" read oddly).
                    if (m.from.zoom - m.to.zoom).abs() < 0.005 {
                        format!(
                            "pan from {:.2}/{:.2} to {:.2}/{:.2} at zoom {:.2}",
                            m.from.x, m.from.y, m.to.x, m.to.y, m.from.zoom
                        )
                    } else if (m.from.x - m.to.x).abs() < 0.005 && (m.from.y - m.to.y).abs() < 0.005 {
                        format!("zoom {:.2} to {:.2}", m.from.zoom, m.to.zoom)
                    } else {
                        format!(
                            "zoom {:.2} to {:.2}, from {:.2}/{:.2} to {:.2}/{:.2}",
                            m.from.zoom, m.to.zoom, m.from.x, m.from.y, m.to.x, m.to.y
                        )
                    }
                }
            )
        ),
        format!("dust: {}", if args.dust { "on" } else { "off" }),
        format!(
            "regional: {}",
            args.regional
                .map_or_else(|| "off".to_owned(), |g| format!("{}x{}", g.cols, g.rows))
        ),
    ]
}

/// Photos replaced and frames put into gaps, counted from 1 ("none" without).
fn repairs_in_words(r: &crate::stage::RepairParams) -> String {
    let mut parts = Vec::new();
    if !r.replace.is_empty() {
        let list: Vec<String> = r.replace.iter().map(|f| (f + 1).to_string()).collect();
        parts.push(format!("photos {} replaced", list.join(", ")));
    }
    for (before, count) in &r.fill {
        parts.push(format!("{count} frame(s) put in before photo {}", before + 1));
    }
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join(", ")
    }
}

/// A transfer curve as INFO.txt names it.
fn transfer_words(t: lapsewerk_video::Transfer) -> String {
    match t {
        lapsewerk_video::Transfer::Srgb => "sRGB".into(),
        lapsewerk_video::Transfer::Pq { .. } => "PQ (HDR)".into(),
        lapsewerk_video::Transfer::Hlg { .. } => "HLG (HDR)".into(),
        other => format!("{other:?}").replace("Rec709", "Rec. 709"),
    }
}

/// Adds to the sequence's INFO.txt what was repaired (photos replaced, frames put into gaps) and
/// what made it: the command line as given, or the app with the command that renders the same.
fn note_delivery(
    dir: &std::path::Path,
    seq: &Sequence,
    args: &RenderArgs,
    run_when: &str,
) -> Result<(), String> {
    let info = dir.join(lapsewerk_video::frames::INFO_FILE);
    let Ok(mut text) = lapsewerk_video::frames::read_text(&info) else {
        return Ok(());
    };
    // In the video's own count (what an edit or a complaint names: "frame 19"), with the photos'
    // file names: each photo moves on by the frames put into gaps before it, and in-between
    // frames multiply it.
    let per = args.tween + 1;
    let inserted_before = |photo: usize| -> usize {
        args.repair
            .fill
            .iter()
            .filter(|(at, _)| *at <= photo)
            .map(|(_, m)| *m)
            .sum()
    };
    let video_of = |photo: usize| (photo + inserted_before(photo)) * per + 1;
    let name = |photo: usize| {
        seq.frames()
            .get(photo)
            .and_then(|f| f.path.file_name())
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    };
    let mut parts = Vec::new();
    if !args.repair.replace.is_empty() {
        let shown: Vec<String> = args
            .repair
            .replace
            .iter()
            .map(|&i| format!("{} ({})", video_of(i), name(i)))
            .collect();
        parts.push(format!(
            "video frame(s) {} replaced by their neighbours blended",
            shown.join(", ")
        ));
    }
    if !args.repair.fill.is_empty() {
        let shown: Vec<String> = args
            .repair
            .fill
            .iter()
            .map(|&(at, m)| {
                let last = video_of(at).saturating_sub(1);
                let first = video_of(at).saturating_sub(m * per);
                format!("{first}-{last} (before {})", name(at))
            })
            .collect();
        parts.push(format!("video frames blended into gaps: {}", shown.join("; ")));
    }
    if !seq.skipped().is_empty() {
        let names: Vec<String> = seq
            .skipped()
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        parts.push(format!("photos left out (unreadable): {}", names.join(", ")));
    }
    let repairs = if parts.is_empty() {
        "none".to_owned()
    } else {
        parts.join("; ")
    };
    let keyframes = keyframes_in_words(seq);
    let made = made_by(seq, dir);
    // Made in more than one run (stopped, then gone on with, maybe from the app and a terminal):
    // the earlier runs whose frames are kept are named too.
    let runs = lapsewerk_video::frames::runs(dir);
    // Each run's stretch of frames ("5" or "5-19", the last one "21 on"); this run's apart.
    // The sequence's length, as INFO.txt counts it: the last stretch ends there.
    let last = text
        .split_once(" frames, numbered")
        .and_then(|(head, _)| head.rsplit('(').next()?.trim().parse::<usize>().ok());
    let stretch = |k: usize| -> String {
        let from = runs.get(k).map_or(0, |r| r.0) + 1;
        let to = runs.get(k + 1).map(|(next, _, _)| *next).or(last);
        match to {
            Some(to) if to <= from => format!("{from}"),
            Some(to) => format!("{from}-{to}"),
            None => format!("{from} on"),
        }
    };
    // Each earlier run once, with all its stretches (in the order the runs first appear).
    let mut grouped: Vec<(String, String, Vec<String>)> = Vec::new();
    let mut mine: Vec<String> = Vec::new();
    for (k, (_, when, by)) in runs.iter().enumerate() {
        if when == run_when {
            mine.push(stretch(k));
        } else if let Some(g) = grouped.iter_mut().find(|(w, b, _)| w == when && b == by) {
            g.2.push(stretch(k));
        } else {
            grouped.push((when.clone(), by.clone(), vec![stretch(k)]));
        }
    }
    let earlier: Vec<String> = grouped
        .iter()
        .map(|(when, by, parts)| {
            format!(
                "earlier    frames {} made on {when} by {}\n",
                parts.join(", "),
                by.replace('\n', "\n           ")
            )
        })
        .collect();
    if args.tween > 0 {
        text.push_str(&format!(
            "in-between {} frame(s) after each photo, made along the motion: of every {} video frames the first is a photo (frame 1 is photo 1)\n",
            args.tween,
            args.tween + 1
        ));
    }
    let when = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z");
    text.push_str(&format!(
        "repairs    {repairs}\n{keyframes}{}made by    {}{made}\nmade on    {when}\n",
        earlier.concat(),
        if earlier.is_empty() || mine.is_empty() {
            String::new()
        } else {
            format!("frames {}: ", mine.join(", "))
        }
    ));
    // Whole or not at all (a full disk must not leave it empty). Not written: the short one goes
    // too, so the delivery counts as not ended and a resume finishes it (app and command line).
    lapsewerk_video::frames::write_whole(&info, &text).map_err(|e| {
        let _ = std::fs::remove_file(&info);
        info_failed(&info, &e)
    })
}

/// INFO.txt could not be written: said, with what is there and how to finish.
fn info_failed(info: &std::path::Path, e: &std::io::Error) -> String {
    format!(
        "{}: could not be written ({e}; is the disk full?): the frames are there, render again with --resume to finish the delivery",
        info.display()
    )
}

/// INFO.txt's keyframes line: each keyframe's photo, its sidecar and a short print of what it
/// holds (a later delivery that looks different shows which one changed); the automatic ones
/// (never edited, following the others) marked. Empty without keyframes.
fn keyframes_in_words(seq: &Sequence) -> String {
    let frames = seq.keyframe_frames();
    if frames.is_empty() {
        return String::new();
    }
    let folder = seq
        .frames()
        .first()
        .and_then(|f| f.path.parent().map(std::path::Path::to_path_buf));
    let mut auto: Vec<(usize, String)> = folder
        .and_then(|f| lapsewerk_develop::project::Project::load(&f).ok().flatten())
        .and_then(|p| p.app)
        .map(|app| crate::cache::untouched_in(&app))
        .unwrap_or_default();
    crate::cache::with_marked(seq, &mut auto);
    let items: Vec<String> = frames
        .iter()
        .map(|&k| {
            let print = crate::cache::sidecar_print(seq, k);
            let file = seq
                .sidecar(k)
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default();
            let short: String = print.chars().take(8).collect();
            let automatic = auto.iter().any(|(f, p)| *f == k && (*p == print || p.is_empty()));
            format!(
                "{} ({file}{}, {short})",
                k + 1,
                if automatic { ", automatic" } else { "" }
            )
        })
        .collect();
    format!(
        "keyframes  {} (photo, sidecar, print of its settings)\n",
        items.join(", ")
    )
}

/// Where the frames go: a movie, or a folder of TIFF frames.
enum Sink {
    Movie(Encoder),
    Frames(lapsewerk_video::FrameWriter),
}

impl Sink {
    /// Keeps the frame there (whole, as written) as the next one.
    fn keep(&mut self) -> Result<(), String> {
        match self {
            Self::Movie(_) => Err("a movie keeps no frames".into()),
            Self::Frames(w) => w.keep_existing().map_err(|e| e.to_string()),
        }
    }

    fn push(&mut self, rgb: &[u16], width: usize) -> Result<(), String> {
        match self {
            Self::Movie(e) => e.push(rgb, width, 0, 0),
            Self::Frames(w) => w.push(rgb, width, 0, 0),
        }
        .map_err(|e| e.to_string())
    }

    fn frames(&self) -> u64 {
        match self {
            Self::Movie(e) => e.frames(),
            // Every frame in the folder, those of an earlier run too.
            Self::Frames(w) => w.next() as u64,
        }
    }

    fn light_level(&self) -> Option<lapsewerk_core::hdr::LightLevel> {
        match self {
            Self::Movie(e) => e.light_level(),
            Self::Frames(w) => w.light_level(),
        }
    }

    fn finish(self) -> Result<std::path::PathBuf, String> {
        match self {
            Self::Movie(e) => e.finish().map_err(|e| e.to_string()),
            Self::Frames(w) => w.finish_checked().map_err(|e| e.to_string()),
        }
    }
}

/// The `width × height` window at (`left`, `top`) of a 16-bit RGB image `src_width` pixels wide.
fn crop(
    rgb: &[u16],
    src_width: usize,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
) -> Result<Image16, String> {
    let mut data = Vec::with_capacity(width * height * 3);
    for y in top..top + height {
        let start = (y * src_width + left) * 3;
        let row = rgb
            .get(start..start + width * 3)
            .ok_or_else(|| format!("crop {width}×{height} at {left},{top} does not fit"))?;
        data.extend_from_slice(row);
    }
    Image16::new(width, height, data).ok_or_else(|| "crop size".to_owned())
}

/// The size each frame is rendered at for `args` from photos of `mw × mh` (any scale): the
/// photo's shape around the video's, with room for stabilizing (`moves`) and a camera move.
fn work_box(args: &RenderArgs, mw: usize, mh: usize, moves: bool) -> (usize, usize) {
    let v = args.video;
    // Fit the frame to the side that limits it, with a margin (8 px + 0.5 %): the render's size is
    // rounded in more than one scaling step and must never come out short. The other side of the box
    // follows from the aspect ratio, so LightCraft loads the source at the size needed instead of the
    // whole sensor.
    // Stabilizing needs room to move the crop: the frame is rendered 4 % larger than it would be.
    // A camera move zooming in renders the frame as much larger, so the closest view still has a
    // source pixel for each video pixel.
    let zoom = args.framing.map_or(1.0, |m| m.max_zoom());
    let room = |v: usize| {
        let v = (v as f64 * zoom).ceil() as usize;
        if moves { v + v / 25 } else { v }
    };
    let margin = |v: usize| room(v) + 8 + v / 200;
    let wider = mw * v.height >= mh * v.width;
    let (box_w, box_h) = if wider {
        let h = margin(v.height);
        (margin((h * mw).div_ceil(mh)), h)
    } else {
        let w = margin(v.width);
        (w, margin((w * mh).div_ceil(mw)))
    };
    (box_w, box_h)
}

/// The sky the night sky work takes (none asked for: `None`): looked for in the first frame as the
/// video shows it, and where that shows none (city light over a dark land, a moonless start) in
/// later frames, the last, the middle and the quarters, as a later, brighter frame shows the
/// horizon. The same for every frame of the render (and a resumed one).
#[must_use]
pub fn find_sky(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    offsets: &[f64],
    measured_size: (usize, usize),
    args: &RenderArgs,
) -> Option<lapsewerk_core::skymask::SkyMatte> {
    find_sky_at(seq, plan, offsets, measured_size, args).map(|(m, _)| m)
}

/// [`find_sky`] with the frame (from 0) it was found in.
#[must_use]
pub fn find_sky_at(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    offsets: &[f64],
    measured_size: (usize, usize),
    args: &RenderArgs,
) -> Option<(lapsewerk_core::skymask::SkyMatte, usize)> {
    let n = seq.len();
    let v = args.video;
    let (mw, mh) = measured_size;
    if n == 0 || mw == 0 || mh == 0 || v.width == 0 || v.height == 0 {
        return None;
    }
    let mut tried = Vec::new();
    for i in [0, n - 1, n / 2, n / 4, 3 * n / 4] {
        if tried.contains(&i) {
            continue;
        }
        tried.push(i);
        if let Some(sky) = sky_in_frame(seq, plan, offsets, measured_size, args, i) {
            return Some((sky, i));
        }
    }
    None
}

/// The sky of frame `i` as the video shows it (the camera move's first view, or the crop), if one
/// is found there.
fn sky_in_frame(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    offsets: &[f64],
    measured_size: (usize, usize),
    args: &RenderArgs,
    i: usize,
) -> Option<lapsewerk_core::skymask::SkyMatte> {
    let n = seq.len();
    let v = args.video;
    let (mw, mh) = measured_size;
    let (box_w, box_h) = work_box(args, mw, mh, false);
    let settings = plan.settings(i, offsets.get(i).copied().unwrap_or(0.0));
    let job = seq
        .render_job(
            i,
            settings,
            box_w,
            box_h,
            Output::Display16(lapsewerk_develop::OutputSpace::Srgb),
        )
        .ok()?;
    let (rgb, w, h) = frame_of(job.run().rendered, None).ok()?;
    if w < v.width || h < v.height {
        return None;
    }
    let frame = match args.framing {
        Some(mv) => {
            let win = lapsewerk_core::framing::window(
                mv.at_frame(0, n),
                w as f64,
                h as f64,
                v.width as f64 / v.height as f64,
            );
            lapsewerk_core::framing::resample(&rgb, w, h, win, v.width, v.height)
                .and_then(|data| Image16::new(v.width, v.height, data))
        }
        None => crop(
            &rgb,
            w,
            offset(w - v.width, args.position.0),
            offset(h - v.height, args.position.1),
            v.width,
            v.height,
        )
        .ok(),
    };
    frame.and_then(|f| lapsewerk_core::astro::sky_matte(&f))
}

/// Whether the sky lies elsewhere in the middle or the last frame than in `sky` (found in frame
/// `found_in`): a camera that pans or follows the stars, whose foreground moves into the first
/// frame's sky. Known before the render, so the app can say it where the trails are set.
#[must_use]
pub fn sky_moves(
    seq: &mut Sequence,
    plan: &DevelopPlan,
    offsets: &[f64],
    measured_size: (usize, usize),
    args: &RenderArgs,
    sky: &lapsewerk_core::skymask::SkyMatte,
    found_in: usize,
) -> bool {
    let n = seq.len();
    [n / 2, n.saturating_sub(1)]
        .into_iter()
        .filter(|&i| i != found_in)
        .filter_map(|i| sky_in_frame(seq, plan, offsets, measured_size, args, i))
        .any(|m| lapsewerk_core::skymask::differ(sky, &m))
}

/// What a movie of `written` frames with `v` delivers, in one line: codec, size, rate, the
/// timecode from first to last frame (drop-frame or not) and the curve. The log's `delivery:`
/// line, and the app's after an export (a movie has no INFO.txt beside it).
#[must_use]
pub fn movie_delivery(v: &lapsewerk_video::VideoSettings, written: usize) -> String {
    let codec = match v.codec {
        lapsewerk_video::Codec::ProRes422Hq => "ProRes 422 HQ",
        lapsewerk_video::Codec::ProRes422 => "ProRes 422",
        lapsewerk_video::Codec::ProRes4444 => "ProRes 4444",
        lapsewerk_video::Codec::Hevc => "HEVC Main 10",
    };
    let curve = match v.transfer {
        lapsewerk_video::Transfer::Srgb => "sRGB",
        lapsewerk_video::Transfer::Rec709 => "Rec. 709 (BT.1886)",
        lapsewerk_video::Transfer::Pq { .. } => "PQ, Rec. 2020",
        lapsewerk_video::Transfer::Hlg { .. } => "HLG, Rec. 2020",
    };
    let timecode = timecode_range(v, written);
    format!(
        "{codec}, {}x{}, {:.3} fps ({}/{}), {written} frame{}, timecode {timecode}, curve {curve}",
        v.width,
        v.height,
        f64::from(v.fps_num) / f64::from(v.fps_den.max(1)),
        v.fps_num,
        v.fps_den,
        if written == 1 { "" } else { "s" }
    )
}

/// The timecode of `written` frames of `v`, first to last: "01:00:00;00 - 01:00:16;08 (drop-frame)",
/// or "none".
fn timecode_range(v: &lapsewerk_video::VideoSettings, written: usize) -> String {
    v.timecode.map_or_else(
        || "none".to_owned(),
        |start| {
            let last = start.saturating_add(u32::try_from(written.saturating_sub(1)).unwrap_or(u32::MAX));
            format!(
                "{} - {} ({})",
                lapsewerk_video::frames::timecode_text(start, (v.fps_num, v.fps_den), v.drop_frame),
                lapsewerk_video::frames::timecode_text(last, (v.fps_num, v.fps_den), v.drop_frame),
                if v.drop_frame { "drop-frame" } else { "non-drop" }
            )
        },
    )
}

/// The crop offset for `excess` pixels at `position` (0: start, 1: end).
fn offset(excess: usize, position: f64) -> usize {
    ((excess as f64 * position.clamp(0.0, 1.0)).round() as usize).min(excess)
}

/// A render as the 16-bit frame the stages carry: SDR as rendered; HDR (linear float, up to `peak`)
/// through [`lapsewerk_core::hdr::encode_frame`].
fn frame_of(
    rendered: Result<lapsewerk_develop::Rendered, String>,
    hdr_peak: Option<f32>,
) -> Result<Frame, String> {
    let r = rendered?;
    let deep = r.deep.ok_or("no 16-bit output")?;
    match (deep.samples, hdr_peak) {
        (DeepSamples::U16(data), None) => Ok((data, deep.width, deep.height)),
        (DeepSamples::F32(data), Some(peak)) => Ok((
            lapsewerk_core::hdr::encode_frame(&data, peak),
            deep.width,
            deep.height,
        )),
        _ => Err("unexpected sample format".into()),
    }
}

/// Names the frames past `total` that finishing removes (a copy put there by hand, a longer
/// render's).
fn note_past_end(w: &lapsewerk_video::frames::FrameWriter, rep: &dyn Reporter, said: &[String]) {
    let list = |names: &[String]| {
        let more = names.len().saturating_sub(5);
        format!(
            "{}{}",
            names.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        )
    };
    let (removed, kept) = w.past_end();
    if !removed.is_empty() {
        rep.note(&format!(
            "removed:       {} frame(s) past the end, from an earlier render here: {}",
            removed.len(),
            list(&removed)
        ));
    }
    // Copies put there, set aside at a fresh start (the render would have written over them).
    // (Those said as this render's edited frames: `name.kept`, or `name.kept-2` … beside an older.)
    let aside: Vec<String> = w
        .kept_foreign()
        .iter()
        .filter(|k| !said.iter().any(|s| k.starts_with(s.as_str())))
        .cloned()
        .collect();
    if !aside.is_empty() {
        rep.note(&format!(
            "warning:       {} file(s) named like this delivery's frames but not made by Lapsewerk here, set aside (not written over): {}",
            aside.len(),
            list(&aside)
        ));
    }
    if !kept.is_empty() {
        rep.note(&format!(
            "warning:       {} file(s) named like this delivery's frames but not made by Lapsewerk here, kept: {} (move them out, or an edit takes them for frames)",
            kept.len(),
            list(&kept)
        ));
    }
}

/// The render's settings spelled out as one command (`render FOLDER --out … …`), when the caller
/// resolved them (the command line's `--from-project`): INFO.txt's `settings` line.
static SETTINGS_LINE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Sets [`SETTINGS_LINE`] (once).
pub fn set_settings_line(line: String) {
    let _ = SETTINGS_LINE.set(line);
}

/// `text` with the home folder written `~`: a delivery's INFO.txt goes to clients, and the
/// user's name in its paths is none of theirs.
fn home_short(text: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if home.len() > 1 => text.replace(&format!("{}/", home.trim_end_matches('/')), "~/"),
        _ => text.to_owned(),
    }
}

/// What made this render, as INFO.txt names it: the app (with the same from the command line,
/// spelled out), or the command as a shell takes it back.
fn made_by(seq: &Sequence, out: &std::path::Path) -> String {
    let made = made_by_program(seq);
    // The frames' name kept from an earlier run (`--name` given then, not now): in the settings
    // too, so they make the same files again.
    match lapsewerk_video::frames::given_name(out) {
        Some(name) if !made.contains("--name") => match made.split_once("\nsettings   ") {
            Some((command, settings)) => format!("{command}\nsettings   {settings} --name {name}"),
            None => format!(
                "{made}\nsettings   {} --name {name}",
                made.split_once(" && ").map_or(made.as_str(), |(_, c)| c)
            ),
        },
        _ => made,
    }
}

/// [`made_by`] as the program was run.
fn made_by_program(seq: &Sequence) -> String {
    let program = std::env::args().next().unwrap_or_default();
    if program.contains("lapsewerk-app") {
        let folder = seq
            .frames()
            .first()
            .and_then(|f| f.path.parent())
            .map_or_else(String::new, |p| p.display().to_string());
        // Spelled out too: the project may change later, the delivery's settings don't.
        let spelled = project_render(&folder);
        format!(
            "the Lapsewerk app; the same from the command line: lapsewerk render \"{folder}\" --from-project\nsettings   lapsewerk render \"{folder}\" {spelled}"
        )
    } else {
        // As a shell takes it back: in the folder it was run from, each argument quoted where it
        // needs it (a ";" of a drop-frame timecode, a space in a folder's name).
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
        let cwd = std::env::current_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        let command = home_short(&format!(
            "cd {} && {}",
            quote(&cwd),
            std::env::args().map(|a| quote(&a)).collect::<Vec<_>>().join(" ")
        ));
        // From the project: its settings spelled out too, with the options given on top (the
        // project may change later, the delivery's settings don't).
        if let Some(line) = SETTINGS_LINE.get() {
            format!("{command}\nsettings   lapsewerk {}", home_short(line))
        } else if std::env::args().any(|a| a == "--from-project") {
            let folder = seq
                .frames()
                .first()
                .and_then(|f| f.path.parent())
                .map_or_else(String::new, |p| p.display().to_string());
            let given: Vec<String> = std::env::args()
                .skip(3)
                .filter(|a| a != "--from-project")
                .map(|a| quote(&a))
                .collect();
            let spelled = project_render(&folder);
            format!(
                "{command}\nsettings   lapsewerk render \"{folder}\" {spelled} {}",
                given.join(" ")
            )
            .trim_end()
            .to_owned()
        } else {
            command
        }
    }
}

/// The render options kept in `folder`'s project (what the app last exported with), quoted as a
/// shell takes them back.
fn project_render(folder: &str) -> String {
    lapsewerk_develop::project::Project::load(std::path::Path::new(folder))
        .ok()
        .flatten()
        .and_then(|p| p.app)
        .and_then(|a| a.get("render").and_then(|r| r.as_array()).cloned())
        .map(|r| {
            r.iter()
                .filter_map(|v| v.as_str())
                .map(|a| {
                    if a.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_./:=,+-@%".contains(c))
                    {
                        a.to_owned()
                    } else {
                        format!("'{}'", a.replace('\'', "'\\''"))
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::RepairParams;

    #[test]
    fn repairs_are_said_in_words() {
        assert_eq!(repairs_in_words(&crate::stage::RepairParams::default()), "none");
        let r = crate::stage::RepairParams {
            replace: vec![2, 6],
            fill: vec![(9, 2)],
        };
        assert_eq!(
            repairs_in_words(&r),
            "photos 3, 7 replaced, 2 frame(s) put in before photo 10"
        );
    }

    #[test]
    fn a_resume_starts_at_the_last_good_photo_before_the_gap() {
        let none = RepairParams::default();
        assert_eq!(resume_point(&none, 10, 4), (4, 4));
        // Two frames put in before photo 5 (video frames 5, 6): photo 5 is video frame 7.
        let fill = RepairParams {
            replace: Vec::new(),
            fill: vec![(5, 2)],
        };
        assert_eq!(resume_point(&fill, 10, 9), (7, 9));
        // Stopped inside the gap: from photo 4, so the gap has its frame before.
        assert_eq!(resume_point(&fill, 10, 6), (4, 4));
        // A replaced photo needs the good one before it.
        let odd = RepairParams {
            replace: vec![6],
            fill: Vec::new(),
        };
        assert_eq!(resume_point(&odd, 10, 6), (5, 5));
        assert_eq!(resume_point(&none, 10, 99), (9, 9));
    }
}
