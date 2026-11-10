//! The six steps: the step bar at the top, the page, and the status bar at the bottom (progress,
//! time left, cancel, back / next).

use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use crate::app::{App, PRESETS, PreviewMode, RATES, STEPS, TaskKind};
use crate::curve::{Curve, Series};
use crate::preview::{Key, Kind, LARGE, SMALL};
use crate::text::{Texts, clock, with_n};
use crate::theme;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    egui::Panel::top("steps")
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::symmetric(20, 12)),
        )
        .show(ui, |ui| top_bar(app, ui));
    egui::Panel::bottom("status")
        .frame(
            egui::Frame::new()
                .fill(theme::SURFACE)
                .inner_margin(egui::Margin::symmetric(20, 12))
                .stroke(Stroke::new(1.0, theme::LINE)),
        )
        .show(ui, |ui| status_bar(app, ui));
    if app.help {
        let step = app.project.as_ref().map_or(1, |p| p.step);
        // Narrower in a small window, so the page beside keeps its picture.
        let width = (ui.available_width() * 0.26).clamp(250.0, 340.0);
        egui::Panel::right("help")
            .exact_size(width)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::same(18))
                    .stroke(Stroke::new(1.0, theme::LINE)),
            )
            .show(ui, |ui| crate::help::panel(app, ui, step));
    }
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::symmetric(24, 16)),
        )
        .show(ui, |ui| {
            let step = app.project.as_ref().map_or(1, |p| p.step);
            let t = app.lang.t();
            ui.label(
                RichText::new(format!(
                    "{}. {}",
                    step,
                    t.steps.get(step - 1).copied().unwrap_or("")
                ))
                .size(24.0)
                .strong(),
            );
            ui.label(theme::muted(t.step_hints.get(step - 1).copied().unwrap_or("")));
            ui.add_space(10.0);
            match step {
                1 => photos(app, ui),
                2 => analysis(app, ui),
                3 => keyframes(app, ui),
                4 => finishing(app, ui),
                5 => preview_page(app, ui),
                _ => export(app, ui),
            }
        });
    keys(app, ui.ctx());
}

/// Arrow keys move through the frames; space plays and pauses.
fn keys(app: &mut App, ctx: &egui::Context) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    // Undo and redo while editing a keyframe: Cmd/Ctrl+Z, Shift+Cmd/Ctrl+Z or Ctrl+Y.
    if app
        .project
        .as_ref()
        .is_some_and(|p| p.step == 3 && p.edit.is_some())
    {
        let (undo, redo) = ctx.input(|i| {
            let z = i.modifiers.command && i.key_pressed(egui::Key::Z);
            let y = i.modifiers.command && i.key_pressed(egui::Key::Y);
            (z && !i.modifiers.shift, (z && i.modifiers.shift) || y)
        });
        if undo || redo {
            app.undo(undo);
        }
    }
    let Some(p) = &mut app.project else { return };
    let n = p.frames().max(1);
    ctx.input(|i| {
        if i.key_pressed(egui::Key::ArrowRight) {
            p.cursor = (p.cursor + 1).min(n - 1);
            p.playing = None;
        }
        if i.key_pressed(egui::Key::ArrowLeft) {
            p.cursor = p.cursor.saturating_sub(1);
            p.playing = None;
        }
        if i.key_pressed(egui::Key::Space) && p.step == 5 {
            p.playing = match p.playing {
                Some(_) => None,
                None => {
                    p.play_from = p.cursor;
                    Some(std::time::Instant::now())
                }
            };
        }
    });
}

fn top_bar(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    let (name, step, reached) = app
        .project
        .as_ref()
        .map_or((String::new(), 1, 1), |p| (p.name.clone(), p.step, p.reached));
    let busy = app.task.is_some();
    // In a narrow window the steps show their numbers only (the current one keeps its name), so
    // they never run over the project's name.
    let compact = ui.available_width() < 1300.0;
    let narrow = ui.available_width() < 1150.0;
    ui.horizontal(|ui| {
        if ui.add(theme::secondary(format!("‹  {}", t.projects))).clicked() {
            app.close_project();
            return;
        }
        ui.add_space(8.0);
        crate::start::logo(ui, 26.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            crate::start::lang_switch(app, ui);
            let help = if app.help {
                theme::primary(RichText::new(t.help).strong())
            } else {
                theme::secondary(RichText::new(t.help).strong())
            };
            if ui.add(help).on_hover_text(t.help_tip).clicked() {
                app.help = !app.help;
            }
            ui.add_space(12.0);
            // The steps, right to left so they end at the language switch.
            let mut go = None;
            for s in (1..=STEPS).rev() {
                let name = t.steps.get(s - 1).copied().unwrap_or("");
                // Very narrow (a laptop): numbers only, so the project's name stays whole (the
                // page's own title names the step).
                let label = if (compact && s != step) || narrow {
                    ""
                } else {
                    name
                };
                if step_chip(
                    ui,
                    s,
                    label,
                    s == step,
                    s < reached || (s < step),
                    s <= reached && !busy,
                )
                .on_hover_text(name)
                .clicked()
                {
                    go = Some(s);
                }
                if s > 1 {
                    // Short in a narrow window: the project's name gets the room.
                    let (r, _) = ui.allocate_exact_size(
                        Vec2::new(if compact { 6.0 } else { 18.0 }, 2.0),
                        Sense::hover(),
                    );
                    ui.painter().line_segment(
                        [r.left_center(), r.right_center()],
                        Stroke::new(
                            2.0,
                            if s <= reached {
                                theme::ACCENT_DIM
                            } else {
                                theme::LINE
                            },
                        ),
                    );
                }
            }
            if let Some(s) = go {
                app.go(s);
            }
            // The name in all the room the steps leave, cut off with "…" only before it reaches
            // them (whole on hover).
            ui.add_space(12.0);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new(&name).size(16.0).strong()).truncate())
                    .on_hover_text(&name);
            });
        });
    });
}

/// A step in the bar: its number in a circle (a tick when done) and its name.
fn step_chip(
    ui: &mut egui::Ui,
    n: usize,
    label: &str,
    current: bool,
    done: bool,
    enabled: bool,
) -> egui::Response {
    let font = FontId::proportional(14.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), theme::TEXT);
    let size = Vec2::new(30.0 + galley.size().x + 14.0, 32.0);
    let (rect, response) =
        ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if current {
        p.rect_filled(rect, CornerRadius::same(16), theme::SURFACE_HI);
    } else if response.hovered() && enabled {
        p.rect_filled(rect, CornerRadius::same(16), theme::SURFACE);
    }
    let c = Pos2::new(rect.left() + 16.0, rect.center().y);
    let (fill, fg) = if current {
        (theme::ACCENT, theme::ON_ACCENT)
    } else if done {
        (theme::ACCENT_DIM, theme::ACCENT)
    } else {
        (
            theme::SURFACE_HI,
            if enabled { theme::MUTED } else { theme::FAINT },
        )
    };
    p.circle_filled(c, 11.0, fill);
    let mark = if done && !current {
        "✓".to_owned()
    } else {
        n.to_string()
    };
    p.text(c, Align2::CENTER_CENTER, mark, FontId::proportional(12.5), fg);
    let text_color = if current {
        theme::TEXT
    } else if enabled {
        theme::MUTED
    } else {
        theme::FAINT
    };
    p.text(
        Pos2::new(rect.left() + 32.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        text_color,
    );
    if enabled && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn status_bar(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    let step = app.project.as_ref().map_or(1, |p| p.step);
    ui.horizontal(|ui| {
        ui.set_min_height(40.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let can_next = can_go_on(app);
            if step < STEPS
                && ui
                    .add_enabled(can_next, theme::primary(format!("{}  ›", t.next)))
                    .clicked()
            {
                app.next();
            }
            // The last step's own button here too, where "Next" is in the others: in a laptop
            // window the page's button is below the fold.
            if step == STEPS
                && let Some(p) = &app.project
            {
                // Done: where it is, here too (the page's done card can be below the fold), as the
                // main button; making it again is then the second one.
                let queued = app.queue.has_target(&p.target()) || p.render_there().is_some_and(|r| r.busy);
                let done = p.finished_export().filter(|_| app.task.is_none() && !queued);
                // Checked and found damaged: going on is the main thing to do, not looking.
                let hurt = done.is_some() && p.sequence && p.can_resume() && !p.verified_damaged().is_empty();
                if let Some(d) = &done {
                    let show = if hurt {
                        theme::secondary(t.show_in_folder)
                    } else {
                        theme::primary(t.show_in_folder)
                    };
                    if ui.add(show).clicked() {
                        App::reveal(d);
                    }
                }
                // A stopped render with these settings: going on, as on the page.
                // Not while exporting: the frames being written are no stopped render.
                let goes_on = (done.is_none() || hurt) && app.task.is_none() && p.goes_on();
                let label = match (done.is_some(), p.sequence) {
                    // Made now by another render, or in the queue: said, not offered.
                    // Another's render, not the queue's own (whose lock it is).
                    _ if p.render_there().is_some_and(|r| r.busy) && !app.queue.runs_target(&p.target()) => {
                        t.target_busy_short
                    }
                    _ if queued => t.queue_making,
                    // This window's own export runs: said, as the queue does.
                    _ if matches!(app.task, Some((crate::app::TaskKind::Export, _))) => t.making,
                    // Refused for room: trying anyway is what is left (the estimate is a bound).
                    _ if p.space_refused => t.space_anyway,
                    _ if goes_on => t.resume,
                    (true, _) => t.export_again,
                    (false, true) => t.export_frames,
                    (false, false) => t.export,
                };
                // Another frames' name than those in the folder: refused, so not offered here either.
                let clash = step == STEPS && (p.name_clash().is_some() || p.bad_frame_name());
                let ready =
                    app.task.is_none() && p.analysis.is_some() && !p.ask_overwrite && !queued && !clash;
                let button = if done.is_some() && !goes_on {
                    theme::secondary(label)
                } else {
                    theme::primary(label)
                };
                if ui.add_enabled(ready, button).clicked() {
                    if goes_on && done.is_some() {
                        if let Some(p) = &mut app.project {
                            p.exported = None;
                            p.resume_next = true;
                        }
                        app.start_export(false);
                    } else if p.space_refused {
                        if let Some(p) = &mut app.project {
                            p.space_anyway = true;
                        }
                        app.start_export(false);
                    } else if goes_on {
                        if let Some(p) = &mut app.project {
                            p.resume_next = true;
                        }
                        app.start_export(false);
                    } else if p.target().exists() {
                        if let Some(p) = &mut app.project {
                            p.ask_overwrite = true;
                        }
                    } else {
                        app.start_export(false);
                    }
                }
            }
            if step > 1
                && ui
                    .add_enabled(app.task.is_none(), theme::secondary(format!("‹  {}", t.back)))
                    .clicked()
            {
                app.go(step - 1);
            }
            // What runs or what was said, in the room the buttons leave: a long message wraps
            // instead of pushing them out of the window.
            ui.with_layout(Layout::left_to_right(Align::Center).with_main_wrap(true), |ui| {
                let mut cancel = false;
                if let Some((_, task)) = &app.task {
                    let s = task.status();
                    let frames = app.project.as_ref().is_some_and(|p| p.sequence);
                    // The check pass measures twice: which round, and the time left over both (the
                    // bar runs over both already).
                    let check = s.doing == crate::work::Doing::Step(lapsewerk_studio::Step::Check);
                    let mut what = doing_text(t, s.doing.text_index(), frames).to_owned();
                    // (The time left counts the second round already: `work` reckons it so.)
                    if check {
                        what = format!(
                            "{what} · {}",
                            t.pass_of.replace("{k}", &s.round.clamp(1, 2).to_string())
                        );
                    }
                    progress_line(ui, &what, s.fraction(), s.done, s.total, s.left, t);
                    if ui.add(theme::secondary(t.cancel)).clicked() {
                        cancel = true;
                    }
                    if task.cancelling() {
                        ui.spinner();
                    }
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(250));
                } else if let Some((done, total, left)) = app.previews.bulk_status() {
                    let what = t.working.get(6).copied().unwrap_or("");
                    progress_line(
                        ui,
                        what,
                        Some(done as f32 / total.max(1) as f32),
                        done,
                        total,
                        left,
                        t,
                    );
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(250));
                } else if let Some(r) = app.queue.running() {
                    // The queue runs in the background: what it does, here as everywhere.
                    let mut what = format!("{}: {} – {}", t.queue_title, r.name, queue_step(t, &r.step));
                    if r.waiting > 0 {
                        what.push_str(&format!(" (+{})", r.waiting));
                    }
                    progress_line(ui, &what, Some(r.fraction), r.done, r.total, r.left, t);
                } else if let Some(m) = app
                    .message
                    .as_ref()
                    .filter(|m| m.error || m.at.elapsed().as_secs() < 8)
                {
                    ui.add(
                        egui::Label::new(RichText::new(&m.text).color(if m.error {
                            theme::BAD
                        } else {
                            theme::GOOD
                        }))
                        .wrap(),
                    );
                } else if step == STEPS
                    && let Some(p) = app
                        .project
                        .as_ref()
                        .filter(|p| p.finished_export().is_some() && !app.queue.has_target(&p.target()))
                {
                    // Done, said where it is seen in any window, for as long as the step is open;
                    // with the delivery check's result once checked (on the page it was below the
                    // fold of a laptop window).
                    let mut done = if p.sequence {
                        t.export_done_frames
                    } else {
                        t.export_done
                    }
                    .to_owned();
                    if let Some((ok, bad)) = &p.verified
                        && bad.is_empty()
                    {
                        done.push(' ');
                        done.push_str(&t.verify_ok.replace("{n}", &ok.to_string()));
                    }
                    ui.add(egui::Label::new(RichText::new(done).color(theme::GOOD).strong()).wrap());
                } else if step == STEPS
                    && let Some(p) = app.project.as_ref().filter(|p| !p.space_refused)
                    && let need = p.space_needed()
                    && need >= 1_000_000
                    && let Some(free) = lapsewerk_video::space::free_bytes(&p.target())
                    && free < need.saturating_add(need / 50)
                {
                    // Room short, said where it is seen before the click (in a laptop window the
                    // page's line is below the fold).
                    let lang = app.lang;
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                t.space_tight
                                    .replace("{need}", &crate::text::bytes(lang, need))
                                    .replace("{free}", &crate::text::bytes(lang, free)),
                            )
                            .color(theme::WARN),
                        )
                        .wrap(),
                    );
                } else if step == STEPS
                    && app.task.is_none()
                    && let Some(p) = app.project.as_ref().filter(|p| p.goes_on())
                    && let Some(r) = p.render_there()
                {
                    // Beside "Go on": what it goes on with (the page's sentence can be below the fold).
                    let mut names = p.verified_damaged();
                    names.extend(
                        r.damaged
                            .iter()
                            .filter(|d| !names.contains(d))
                            .cloned()
                            .collect::<Vec<_>>(),
                    );
                    let damaged = names.len();
                    let words = p.damage_words(t, &names);
                    let text = if damaged > 0 && !r.ended {
                        // Stopped: going on makes them and then the rest.
                        t.footer_damaged_stopped.replace("{bad}", &words)
                    } else if damaged > 0 {
                        t.footer_damaged.replace("{bad}", &words)
                    } else {
                        t.footer_resume.replace("{n}", &r.frames.to_string())
                    };
                    ui.add(egui::Label::new(theme::muted(text)).wrap());
                } else if step == STEPS
                    && app.task.is_none()
                    && app.project.as_ref().is_some_and(|p| p.bad_frame_name())
                {
                    ui.add(egui::Label::new(RichText::new(t.frame_name_refused).color(theme::WARN)).wrap());
                } else if step == STEPS
                    && app.task.is_none()
                    && let Some((there, here)) = app.project.as_ref().and_then(|p| p.name_clash())
                {
                    // Said before the click, in one line (the page's sentence can be below the fold).
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                t.name_clash_footer
                                    .replace("{there}", &there)
                                    .replace("{here}", &here),
                            )
                            .color(theme::WARN),
                        )
                        .wrap(),
                    );
                } else if step == STEPS
                    && app.task.is_none()
                    && let Some(p) = app.project.as_ref()
                    && let Some(r) = p.render_there()
                    && r.frames > 0
                    && r.same == Some(false)
                    && !r.foreign
                    && p.name_clash().is_none()
                {
                    // Made with other settings: the export replaces them (said beside the button, as
                    // the page's sentence can be below the fold).
                    ui.add(
                        egui::Label::new(
                            RichText::new(t.target_other_short.replace("{n}", &r.frames.to_string()))
                                .color(theme::WARN),
                        )
                        .wrap(),
                    );
                }
                if cancel && let Some((_, task)) = &app.task {
                    task.cancel();
                }
            });
        });
    });
}

/// "Measuring brightness · 120 / 492 · 2:10 left", with a bar.
fn progress_line(
    ui: &mut egui::Ui,
    what: &str,
    fraction: Option<f32>,
    done: usize,
    total: usize,
    left: Option<std::time::Duration>,
    t: &Texts,
) {
    ui.label(RichText::new(what).strong());
    let bar = egui::ProgressBar::new(fraction.unwrap_or(0.0))
        .desired_width(260.0)
        .fill(theme::ACCENT)
        .corner_radius(CornerRadius::same(6));
    let bar = if fraction.is_none() {
        bar.animate(true)
    } else {
        bar
    };
    ui.add(bar);
    if total > 0 {
        ui.label(theme::muted(format!("{done} / {total}")));
    }
    match left {
        Some(l) => ui.label(
            RichText::new(
                format!("{} {}", t.time_left, clock(l)).trim().to_owned()
                    + if t.time_left.is_empty() { " left" } else { "" },
            )
            .color(theme::TEXT),
        ),
        None => ui.label(theme::muted(t.estimating)),
    };
}

/// May the user go on from this step?
fn can_go_on(app: &App) -> bool {
    let Some(p) = &app.project else { return false };
    if app.task.is_some() {
        return false;
    }
    match p.step {
        1 => p.seq.is_some(),
        2 | 3 => p.analysis.is_some(),
        4 => true,
        5 => p.corrected.is_some(),
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------
// Step 1: the photos

fn photos(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    let Some(p) = &app.project else { return };
    let n = p.frames();
    let interval = p.timing.as_ref().map(|t| t.interval_ms).filter(|ms| *ms > 0);
    // Gaps in the capture and where damaged photos were left out.
    let gaps = p.timing.as_ref().map_or(0, |t| t.gaps_before.len()) + p.skipped_gaps.len();
    let (opened, beside_raws, camera) = (p.seq.is_some(), p.beside_raws, p.camera_data);
    let fps = p.fps;
    let video_len = p.video_seconds_text(app.lang);
    ui.horizontal_top(|ui| {
        theme::card().show(ui, |ui| {
            ui.set_width(330.0);
            // One fact a line (the row around the card would put them side by side).
            ui.vertical(|ui| {
                ui.label(theme::title(t.sequence));
                ui.add_space(6.0);
                // Photos left out (damaged): how many frames the video gets once they are filled.
                let left_out: usize = app
                    .project
                    .as_ref()
                    .map_or(0, |p| p.skipped_gaps.iter().map(|(_, m)| m).sum());
                if left_out > 0 {
                    fact(
                        ui,
                        t.frames,
                        &t.frames_with_gaps
                            .replace("{n}", &n.to_string())
                            .replace(
                                "{gaps}",
                                &format!(
                                    "{left_out} {}",
                                    t.gap_filled
                                        .get(usize::from(left_out != 1))
                                        .copied()
                                        .unwrap_or("")
                                ),
                            )
                            .replace("{all}", &(n + left_out).to_string()),
                    );
                } else {
                    fact(ui, t.frames, &n.to_string());
                }
                fact(
                    ui,
                    t.interval,
                    &interval.map_or(t.unknown.to_owned(), |ms| {
                        format!("{} s", crate::text::num(app.lang, ms as f64 / 1000.0, 1))
                    }),
                );
                fact(
                    ui,
                    t.video_length,
                    &format!("{} ({} {})", video_len, fps, t.fps_short),
                );
                fact(
                    ui,
                    t.gaps,
                    &if gaps == 0 {
                        t.none.to_owned()
                    } else {
                        // Places, not frames: two damaged photos side by side are one place.
                        format!(
                            "{gaps} {}",
                            t.gap_places.get(usize::from(gaps != 1)).copied().unwrap_or("")
                        )
                    },
                );
                if opened {
                    fact(ui, t.camera_data, &format!("{camera} / {n}"));
                }
                // The photos' shape (with the crop from another raw editor), as the video follows it.
                if let Some(up) = app
                    .project
                    .as_ref()
                    .and_then(crate::app::ProjectState::frames_upright)
                {
                    fact(ui, t.format, if up { t.shape_upright } else { t.shape_wide });
                }
                if let Some(note) = app.project.as_ref().and_then(|p| p.project_note.clone()) {
                    ui.add_space(6.0);
                    ui.label(RichText::new(note).color(theme::WARN).size(12.5));
                }
                if beside_raws > 0 {
                    ui.add_space(6.0);
                    ui.label(theme::small(with_n(t.raw_jpeg_note, beside_raws)));
                }
                ui.add_space(14.0);
                if ui
                    .add_enabled(opened && app.task.is_none(), theme::primary(t.start_analysis))
                    .clicked()
                {
                    app.next();
                }
                if ui
                    .add(egui::Button::new(theme::muted(t.choose_other)).frame(false))
                    .clicked()
                    && let Some(folder) = rfd::FileDialog::new().pick_folder()
                {
                    app.open_folder(&folder);
                }
            });
        });
        ui.add_space(12.0);
        // Frames spread over the series.
        ui.vertical(|ui| {
            if let Some(p) = &app.project {
                let n = p.frames();
                // Eight spread over the series, each once (a short series shows all it has).
                let mut picks: Vec<usize> = (0..8).map(|k| k * n.saturating_sub(1) / 7).collect();
                picks.dedup();
                // Upright photos (as their pictures come, or measured): eight in one row of upright
                // tiles, not cut to landscape.
                let upright = p.frames_upright().unwrap_or_else(|| {
                    picks.iter().any(|&f| {
                        let key = Key {
                            frame: f,
                            kind: Kind::Shot,
                            edge: SMALL,
                        };
                        app.previews
                            .get(key, 0.0, None)
                            .is_some_and(|t| t.size()[1] > t.size()[0])
                    })
                });
                let per_row = if upright { 8 } else { 4 };
                let w = ((ui.available_width() - (per_row - 1) as f32 * 10.0) / per_row as f32).max(40.0);
                let h = if upright { w * 1.5 } else { w * 2.0 / 3.0 };
                let mut chosen = None;
                for row in picks.chunks(per_row) {
                    ui.horizontal(|ui| {
                        for &f in row {
                            let key = Key {
                                frame: f,
                                kind: Kind::Shot,
                                edge: SMALL,
                            };
                            let tex = app.previews.get(key, 0.0, None).cloned();
                            if picture(ui, tex.as_ref(), Vec2::new(w, h), Some(f + 1)).clicked() {
                                chosen = Some(f);
                            }
                        }
                    });
                }
                if let (Some(f), Some(p)) = (chosen, &mut app.project) {
                    p.cursor = f;
                }
            }
        });
    });
}

fn fact(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(theme::muted(label));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).strong())
        });
    });
}

/// A picture in a rounded frame, covering `size`; a frame number in the corner.
fn picture(
    ui: &mut egui::Ui,
    tex: Option<&egui::TextureHandle>,
    size: Vec2,
    number: Option<usize>,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    draw_picture(ui, rect, tex, true);
    if let Some(n) = number {
        let tag = Rect::from_min_size(
            rect.min + Vec2::splat(8.0),
            Vec2::new(10.0 + 8.0 * n.to_string().len() as f32, 20.0),
        );
        ui.painter()
            .rect_filled(tag, CornerRadius::same(6), Color32::from_black_alpha(150));
        ui.painter().text(
            tag.center(),
            Align2::CENTER_CENTER,
            n.to_string(),
            FontId::proportional(11.5),
            theme::TEXT,
        );
    }
    response
}

/// Draws `tex` into `rect`: covering it (cropped) or fitting it (letterboxed).
fn draw_picture(ui: &egui::Ui, rect: Rect, tex: Option<&egui::TextureHandle>, cover: bool) -> Rect {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, CornerRadius::same(10), theme::SURFACE_HI);
    let Some(tex) = tex else {
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "…",
            FontId::proportional(18.0),
            theme::FAINT,
        );
        return rect;
    };
    let [tw, th] = tex.size().map(|v| v as f32);
    let (sx, sy) = (rect.width() / tw.max(1.0), rect.height() / th.max(1.0));
    if cover {
        let s = sx.max(sy);
        let uv = Rect::from_center_size(
            Pos2::new(0.5, 0.5),
            Vec2::new(
                (rect.width() / (tw * s)).min(1.0),
                (rect.height() / (th * s)).min(1.0),
            ),
        );
        egui::Image::new((tex.id(), rect.size()))
            .uv(uv)
            .corner_radius(CornerRadius::same(10))
            .paint_at(ui, rect);
        rect
    } else {
        let s = sx.min(sy);
        let r = Rect::from_center_size(rect.center(), Vec2::new(tw * s, th * s));
        egui::Image::new((tex.id(), r.size()))
            .corner_radius(CornerRadius::same(8))
            .paint_at(ui, r);
        r
    }
}

/// The video's view of a frame (`uv`: left, top, width, height in 0..1 of `tex`), fitted into
/// `rect`; `split` paints only the part right of that fraction. Returns where it is.
fn draw_view(ui: &egui::Ui, rect: Rect, tex: Option<&egui::TextureHandle>, uv: [f32; 4], split: f32) -> Rect {
    let Some(tex) = tex else {
        return draw_picture(ui, rect, None, false);
    };
    let [tw, th] = tex.size().map(|v| v as f32);
    let [u, v, uw, vh] = uv;
    let (w, h) = ((uw * tw).max(1.0), (vh * th).max(1.0));
    let s = (rect.width() / w).min(rect.height() / h);
    let shown = Rect::from_center_size(rect.center(), Vec2::new(w * s, h * s));
    if split <= 0.0 {
        ui.painter_at(rect)
            .rect_filled(rect, CornerRadius::same(10), theme::SURFACE_HI);
    }
    let split = split.clamp(0.0, 1.0);
    let part = Rect::from_min_max(
        Pos2::new(shown.left() + shown.width() * split, shown.top()),
        shown.max,
    );
    let uv = Rect::from_min_max(Pos2::new(u + uw * split, v), Pos2::new(u + uw, v + vh));
    egui::Image::new((tex.id(), part.size()))
        .uv(uv)
        .corner_radius(CornerRadius::same(if split > 0.0 { 0 } else { 8 }))
        .paint_at(ui, part);
    shown
}

// ---------------------------------------------------------------------------------------------
// Step 2: analysis

fn analysis(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    let lang = app.lang;
    let Some(p) = &app.project else { return };
    let Some(a) = p.analysis.clone() else {
        waiting(ui, t, task_status(app), false);
        return;
    };
    // Findings in words.
    // Without the odd frames (a flash): they are replaced, and one would dominate the figure.
    let mut odd: Vec<usize> = p.quality.as_ref().map(|q| q.outliers.clone()).unwrap_or_default();
    // Nor the first frame after a gap (photos missing or damaged): the jump across it is the
    // scene's change over the missing frames, not flicker, and the gap gets filled.
    odd.extend(
        p.timing
            .as_ref()
            .map(|t| t.gaps_before.clone())
            .unwrap_or_default(),
    );
    odd.extend(p.skipped_gaps.iter().map(|(at, _)| *at));
    let steady: Vec<Option<f64>> = a
        .compensated
        .iter()
        .enumerate()
        .map(|(i, v)| if odd.contains(&i) { None } else { *v })
        .collect();
    let flicker = lapsewerk_studio::jitter(&steady);
    let level = if flicker < 0.01 {
        0
    } else if flicker < 0.04 {
        1
    } else {
        2
    };
    let steps = a.checks.len();
    let (scene, _) = a.light_ranges();
    ui.columns(3, |cols| {
        if let [a1, a2, a3] = cols {
            tile(
                a1,
                t.flicker,
                t.flicker_levels.get(level).copied().unwrap_or(""),
                &if level > 0 {
                    t.flicker_evened
                        .replace("{n}", &crate::text::num(lang, flicker, 3))
                } else {
                    format!("{} {}", crate::text::num(lang, flicker, 3), t.stops)
                },
                [theme::GOOD, theme::WARN, theme::BAD]
                    .get(level)
                    .copied()
                    .unwrap_or(theme::TEXT),
            );
            let detail = if steps == 0 {
                t.exposure_steps_none.to_owned()
            } else {
                // Found in the pictures (no exposure data): where, so they can be checked.
                let seen: Vec<String> = a
                    .checks
                    .iter()
                    .filter(|c| c.reported == 0.0)
                    .take(6)
                    .map(|c| (c.frame + 1).to_string())
                    .collect();
                if seen.len() == steps {
                    t.exposure_steps_seen.replace("{frames}", &seen.join(", "))
                } else {
                    with_n(t.exposure_steps_detail, steps)
                }
            };
            tile(a2, t.exposure_steps, &steps.to_string(), &detail, theme::TEXT);
            let stops = crate::text::num(lang, scene, 1);
            tile(
                a3,
                t.light_change,
                &stops,
                &with_n(t.light_change_detail, &stops),
                theme::TEXT,
            );
        }
    });
    ui.add_space(8.0);
    // Scrolls in a small window, so the curve is never cut off below the status bar.
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            hints(app, ui);
            // Picture, slider and curve share what is left: with many hints the curve gets
            // flatter too, rather than running under the status bar.
            let upright = app
                .project
                .as_ref()
                .map(crate::app::ProjectState::preview_video)
                .is_some_and(|(w, h)| h > w);
            if upright {
                // An upright picture beside the curve, not a narrow strip above it (scrolls in a
                // short window).
                let h = (ui.available_height() - 12.0).max(360.0);
                let pic_w = (ui.available_width() * 0.4).max(380.0);
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(Vec2::new(pic_w, h), Layout::top_down(Align::Min), |ui| {
                        ui.set_width(pic_w);
                        viewer(app, ui, Kind::Shot, 1.0, 60.0);
                    });
                    ui.vertical(|ui| curve(app, ui, (h - 40.0).min(420.0)));
                });
            } else {
                let curve_h = (ui.available_height() - 120.0 - 70.0).clamp(130.0, 190.0);
                viewer(app, ui, Kind::Shot, 0.5, curve_h + 90.0);
                curve(app, ui, curve_h);
            }
        });
}

/// What could spoil the movie, in words, with what to do about it.
fn hints(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    let lang = app.lang;
    let Some(p) = &mut app.project else { return };
    let Some(q) = p.quality.clone() else { return };
    let mut jump = None;
    let warn = |ui: &mut egui::Ui, text: String| {
        ui.label(RichText::new(format!("⚠  {text}")).color(theme::WARN));
    };
    let mut buttons = |ui: &mut egui::Ui, frames: Vec<usize>| {
        ui.horizontal(|ui| {
            for f in frames.into_iter().take(8) {
                if ui
                    .small_button(format!("{} {}", t.frame_word, f + 1))
                    .on_hover_text(t.jump_to_frame)
                    .clicked()
                {
                    jump = Some(f);
                }
            }
        });
    };
    theme::card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(t.hints).strong());
        ui.add_space(4.0);
        // Photos without exposure data (exported JPEGs): the camera's small steps cannot be told
        // from the light, so a change of light is measured smaller than it was.
        let no_exif = p.camera_data == 0 && p.frames() > 30;
        if no_exif {
            warn(ui, t.no_exif_hint.to_owned());
        }
        // Portrait and landscape mixed: said now, not when the export fails.
        let shapes = p
            .analysis
            .as_ref()
            .and_then(|a| lapsewerk_studio::frame_size(&a.as_shot.sizes).err());
        if let Some(e) = &shapes {
            warn(ui, crate::text::reason(lang, e));
        }
        // Damaged photos left out: said here too, not "no gaps" in green beside it.
        let skipped = !p.skipped_names.is_empty();
        if skipped {
            warn(
                ui,
                t.skipped_hint
                    .replace("{names}", &crate::text::names_list(lang, &p.skipped_names)),
            );
        }
        if q.outliers.is_empty() && q.clipped.is_empty() && q.missing.is_empty() {
            if no_exif || shapes.is_some() || skipped {
                return;
            }
            ui.label(RichText::new(format!("✓  {}", t.no_hints)).color(theme::GOOD));
            return;
        }
        egui::Grid::new("hints")
            .num_columns(3)
            .spacing([18.0, 8.0])
            .show(ui, |ui| {
                if !q.outliers.is_empty() {
                    warn(ui, with_n(t.outliers_found, q.outliers.len()));
                    ui.checkbox(&mut p.finish.replace_outliers, t.replace_outliers);
                    buttons(ui, q.outliers.clone());
                    ui.end_row();
                }
                if !q.missing.is_empty() {
                    warn(ui, with_n(t.gaps_found, q.missing.len()));
                    ui.checkbox(&mut p.finish.fill_gaps, t.fill_gaps);
                    // The frame before the pause.
                    buttons(ui, q.missing.iter().map(|&(f, _)| f.saturating_sub(1)).collect());
                    ui.end_row();
                }
                if !q.clipped.is_empty() {
                    warn(
                        ui,
                        format!(
                            "{} ({})",
                            with_n(t.clipped_found, q.clipped.len()),
                            t.clipped_share.replace(
                                "{p}",
                                &crate::text::num(lang, f64::from(q.clipped_most) * 100.0, 1)
                            )
                        ),
                    );
                    ui.label(theme::small(t.clipped_tip));
                    buttons(ui, q.clipped.iter().copied().take(1).collect());
                    ui.end_row();
                }
            });
    });
    if let Some(f) = jump {
        p.cursor = f;
    }
}

/// A finding: name, big value, detail.
fn tile(ui: &mut egui::Ui, name: &str, value: &str, detail: &str, color: Color32) {
    theme::card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(theme::muted(name));
            ui.label(RichText::new(value).size(26.0).strong().color(color));
            ui.label(theme::small(detail));
        });
    });
}

/// What the running task is doing, if one runs.
fn task_status(app: &App) -> Option<crate::work::Status> {
    app.task.as_ref().map(|(_, task)| task.status())
}

/// What a task is doing, in words; rendering frames of a sequence is "making the frames".
fn doing_text(t: &Texts, index: usize, frames: bool) -> &'static str {
    if index == 5 && frames {
        return t.working_frames;
    }
    t.working.get(index).copied().unwrap_or("")
}

/// Shown while the page's work runs: what, how far, time left, big.
fn waiting(ui: &mut egui::Ui, t: &Texts, status: Option<crate::work::Status>, frames: bool) {
    ui.add_space(60.0);
    ui.vertical_centered(|ui| {
        if let Some(s) = status {
            let check = s.doing == crate::work::Doing::Step(lapsewerk_studio::Step::Check);
            let mut what = doing_text(t, s.doing.text_index(), frames).to_owned();
            if check {
                what = format!(
                    "{what} ({})",
                    t.pass_of.replace("{k}", &s.round.min(2).to_string())
                );
            }
            ui.label(RichText::new(what).size(20.0).strong());
            // Measuring and correcting: done once, kept; the export needs them.
            // Not when measuring again (keyframes changed): said below, with why.
            if s.again.is_none()
                && (check || s.doing == crate::work::Doing::Step(lapsewerk_studio::Step::Measure))
            {
                ui.add_space(4.0);
                ui.label(theme::muted(t.only_once_why));
            }
            ui.add_space(10.0);
            ui.add(
                egui::ProgressBar::new(s.fraction().unwrap_or(0.0))
                    .desired_width(420.0)
                    .fill(theme::ACCENT)
                    .animate(s.fraction().is_none()),
            );
            ui.add_space(6.0);
            // Measuring as shot is followed by the check pass and more: its time left is this
            // step's, said so (it read as the whole wait).
            let more_follows = s.doing == crate::work::Doing::Step(lapsewerk_studio::Step::Measure);
            let left = s.left.map_or(t.estimating.to_owned(), |l| {
                let text = format!("{} {}", t.time_left, clock(l)).trim().to_owned();
                if more_follows {
                    format!("{text} {}", t.this_step)
                } else {
                    text
                }
            });
            ui.label(theme::muted(if s.total > 0 {
                format!("{} / {}  ·  {left}", s.done, s.total)
            } else {
                left
            }));
            // Measuring again because keyframes changed (in LightCraft, or by hand): said, with how
            // many frames.
            if let Some(line) = &s.again {
                ui.add_space(6.0);
                ui.label(RichText::new(again_text(t, line)).color(theme::TEXT));
            }
        } else {
            ui.spinner();
        }
    });
}

/// The large picture of the frame at the cursor, with a slider through the sequence.
fn viewer(app: &mut App, ui: &mut egui::Ui, kind: Kind, share: f32, reserve: f32) {
    let Some(p) = &mut app.project else { return };
    let n = p.frames().max(1);
    let offset = if matches!(kind, Kind::Video(_)) {
        p.offsets().get(p.cursor).copied().unwrap_or(0.0)
    } else {
        0.0
    };
    let key = Key {
        frame: p.cursor,
        kind,
        edge: LARGE,
    };
    let tex = app.previews.get_or_near(key, offset, None);
    // Room left below for the slider and the curve (190 px): a short window shrinks the picture,
    // not the curve's axis.
    let h = (ui.available_height() * share)
        .min(ui.available_height() - reserve)
        .max(120.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
    draw_picture(ui, rect, tex.as_ref(), false);
    frame_slider(ui, &mut p.cursor, n);
}

fn frame_slider(ui: &mut egui::Ui, cursor: &mut usize, n: usize) {
    ui.horizontal(|ui| {
        if ui.add(theme::secondary("◀")).clicked() {
            *cursor = cursor.saturating_sub(1);
        }
        let mut f = *cursor + 1;
        // Room for the ▶ button and the number box.
        ui.spacing_mut().slider_width = (ui.available_width() - 150.0).max(100.0);
        if ui
            .add(egui::Slider::new(&mut f, 1..=n.max(1)).show_value(true))
            .changed()
        {
            *cursor = f.saturating_sub(1);
        }
        if ui.add(theme::secondary("▶")).clicked() {
            *cursor = (*cursor + 1).min(n.saturating_sub(1));
        }
    });
}

/// The brightness curve of the project at the cursor.
fn curve(app: &mut App, ui: &mut egui::Ui, height: f32) {
    let t = app.lang.t();
    let keyframes = app
        .project
        .as_ref()
        .map(crate::app::ProjectState::keyframes)
        .unwrap_or_default();
    let Some(p) = &mut app.project else { return };
    let Some(a) = p.analysis.clone() else { return };
    let offsets = p.offsets();
    let shot: Vec<Option<f64>> = a.as_shot.stops.clone();
    let video: Vec<Option<f64>> = a
        .as_shot
        .stops
        .iter()
        .zip(&offsets)
        .map(|(s, o)| s.map(|s| s + o))
        .collect();
    let scene: Vec<Option<f64>> = a.scene.iter().map(|v| v.is_finite().then_some(*v)).collect();
    let suggested = open_suggestions(p.suggestions(&a), &p.keyframes(), p.frames());
    let warnings = p.repair().replace;
    // Replaced frames are in the video as their neighbours blended: drawn so.
    let mut video = video;
    for &i in &warnings {
        let good = |j: usize| !warnings.contains(&j);
        let before = (0..i)
            .rev()
            .find(|&j| good(j))
            .and_then(|j| video.get(j).copied().flatten().map(|v| (j, v)));
        let after = (i + 1..video.len())
            .find(|&j| good(j))
            .and_then(|j| video.get(j).copied().flatten().map(|v| (j, v)));
        let v = match (before, after) {
            (Some((j0, a)), Some((j1, b))) => Some(a + (b - a) * (i - j0) as f64 / (j1 - j0) as f64),
            (Some((_, a)), None) => Some(a),
            (None, Some((_, b))) => Some(b),
            (None, None) => None,
        };
        if let Some(slot) = video.get_mut(i) {
            *slot = v;
        }
    }
    let c = Curve {
        series: vec![
            Series {
                values: &scene,
                color: theme::CURVE_SCENE,
                label: t.curve_scene,
                width: 1.5,
            },
            Series {
                values: &shot,
                color: theme::CURVE_SHOT,
                label: t.curve_as_shot,
                width: 1.5,
            },
            Series {
                values: &video,
                color: theme::CURVE_VIDEO,
                // Before the corrections are worked out the auto brightness is not in it yet: said.
                label: if p.corrected.is_none() && p.auto_brightness() {
                    t.curve_after_unlifted
                } else {
                    t.curve_after
                },
                width: 2.5,
            },
        ],
        frames: p.frames(),
        keyframes: &keyframes,
        suggested: &suggested,
        warnings: &warnings,
        height,
        stops_word: t.curve_axis,
        frame_word: t.frame_word,
        suggested_word: t.suggested_ring,
        suggested_legend: t.suggested_legend,
    };
    if c.show(ui, &mut p.cursor) {
        p.playing = None;
    }
}

/// The keyframe suggestions still worth making: one right beside a saved keyframe (a frame or
/// two, or 2 % of the series) adds nothing and is left out, in the list and on the curve.
fn open_suggestions(suggested: Vec<usize>, saved: &[usize], frames: usize) -> Vec<usize> {
    let near = (frames / 50).max(2);
    suggested
        .into_iter()
        .filter(|s| !saved.iter().any(|k| k.abs_diff(*s) <= near && k != s))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Step 3: keyframes

fn keyframes(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    let Some(p) = &app.project else { return };
    let Some(a) = p.analysis.clone() else {
        waiting(ui, t, task_status(app), false);
        return;
    };
    let saved = p.keyframes();
    let untouched = p.untouched.clone();
    let locked = p.untouched_kept.clone();
    let has_sidecars = p
        .seq
        .as_ref()
        .is_some_and(|s| crate::work::try_lock(s).is_none_or(|s| !s.keyframe_frames().is_empty()));
    let suggested = open_suggestions(p.suggestions(&a), &saved, p.frames());
    let mut all: Vec<usize> = saved.iter().chain(&suggested).copied().collect();
    all.sort_unstable();
    all.dedup();
    let editing = p.edit.as_ref().map(|e| e.frame);
    let cursor = p.cursor;
    let mut pick = None;
    let mut create = false;
    let mut add_here = false;
    let mut make_auto = None;
    let editing_now = app.project.as_ref().is_some_and(|p| p.edit.is_some());
    // Three columns: the list, the picture and curve, the controls.
    // Narrow (a small window, the help open): the list and the controls give way first, so the
    // picture keeps at least about a third of the width.
    let gap = 16.0;
    let total = ui.available_width();
    let (mut list_w, mut controls_w) = (230.0_f32, 400.0_f32);
    let room = |l: f32, c: f32| total - l - gap - if editing_now { c + gap } else { 0.0 };
    if room(list_w, controls_w) < total * 0.36 {
        list_w = 180.0;
        controls_w = 320.0;
    }
    let height = ui.available_height();
    let centre_w = room(list_w, controls_w).max(120.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        ui.allocate_ui_with_layout(Vec2::new(list_w, height), Layout::top_down(Align::Min), |ui| {
            ui.set_width(list_w);
            ui.label(theme::title(t.keyframes));
            ui.label(theme::small(t.keyframes_hint));
            // Keyframes edited elsewhere (another raw editor, LightCraft) came with the photos: said, and
            // that the suggestions beside them are a choice, not work left to do.
            if let Some(p) = &app.project {
                let taken = p
                    .sidecar_keyframes
                    .iter()
                    .filter(|k| !p.untouched.iter().any(|(f, _)| f == *k))
                    .count();
                if taken > 0 {
                    let text = if suggested.is_empty() {
                        t.keyframes_taken
                    } else {
                        t.keyframes_taken_suggested
                    };
                    ui.label(RichText::new(text.replace("{n}", &taken.to_string())).color(theme::MUTED));
                }
            }
            if let Some(p) = &app.project
                && !p.other_editor.is_empty()
            {
                let frames: Vec<String> = p.other_editor.iter().map(|f| (f + 1).to_string()).collect();
                ui.label(
                    RichText::new(t.sidecar_other_editor.replace("{frames}", &frames.join(", ")))
                        .color(theme::WARN)
                        .size(12.5),
                );
            }
            if let Some(p) = &app.project {
                for (f, names) in p.unknown.iter().take(3) {
                    ui.label(
                        RichText::new(
                            t.sidecar_unknown
                                .replace("{frame}", &(f + 1).to_string())
                                .replace("{names}", &names.join(", ")),
                        )
                        .color(theme::WARN)
                        .size(12.5),
                    );
                }
            }
            // Automatic keyframes whose XMP is read-only or linked: they keep their look.
            if let Some(p) = &app.project
                && !p.untouched_kept.is_empty()
            {
                let frames: Vec<String> = p.untouched_kept.iter().map(|f| (f + 1).to_string()).collect();
                ui.label(
                    RichText::new(t.sidecar_kept.replace("{frames}", &frames.join(", ")))
                        .color(theme::WARN)
                        .size(12.5),
                );
            }
            if let Some(p) = &app.project
                && !p.unreadable.is_empty()
            {
                let frames: Vec<String> = p.unreadable.iter().map(|f| (f + 1).to_string()).collect();
                ui.label(
                    RichText::new(t.sidecar_unreadable.replace("{frames}", &frames.join(", ")))
                        .color(theme::WARN)
                        .size(12.5),
                );
            }
            ui.add_space(4.0);
            // A solid bar, seen at rest (the floating one is invisible until hovered).
            ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
            egui::ScrollArea::vertical()
                .id_salt("keyframe-list")
                // Its scroll bar always shown: suggestions below the fold are seen to be there.
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                // Room below for its buttons (suggestions, back to automatic, add here), above the footer.
                .max_height((height - 260.0).max(120.0))
                .show(ui, |ui| {
                    for &f in &all {
                        // Saved and edited, saved and following the edited ones, or a suggestion.
                        let state = (has_sidecars && saved.contains(&f))
                            .then(|| !untouched.iter().any(|(u, _)| *u == f));
                        if keyframe_row(ui, list_w, f, state, locked.contains(&f), editing == Some(f), t)
                            .clicked()
                        {
                            pick = Some(f);
                        }
                    }
                    if all.is_empty() {
                        ui.label(theme::muted(t.no_keyframes));
                    }
                });
            ui.add_space(8.0);
            if suggested.iter().any(|s| !(has_sidecars && saved.contains(s)))
                && ui
                    .add_sized([list_w, 40.0], theme::primary(t.create_suggested))
                    .clicked()
            {
                create = true;
            }
            // An edited keyframe between others can follow them again.
            if let Some(f) = editing
                && has_sidecars
                && saved.contains(&f)
                && !untouched.iter().any(|(u, _)| *u == f)
                && saved.iter().any(|k| *k < f)
                && saved.iter().any(|k| *k > f)
                && ui
                    .add_sized([list_w, 34.0], theme::secondary(t.make_auto))
                    .on_hover_text(t.make_auto_tip)
                    .clicked()
            {
                make_auto = Some(f);
            }
            // Only where there is none yet.
            if !saved.contains(&cursor)
                && ui
                    .add_sized(
                        [list_w, 38.0],
                        theme::secondary(format!("+ {} {}", t.add_keyframe_here, cursor + 1)),
                    )
                    .clicked()
            {
                add_here = true;
            }
        });
        ui.allocate_ui_with_layout(Vec2::new(centre_w, height), Layout::top_down(Align::Min), |ui| {
            ui.set_width(centre_w);
            let Some(p) = &mut app.project else { return };
            let (kind, settings) = match &p.edit {
                Some(e) => (
                    Kind::Edit(e.version),
                    Some(std::sync::Arc::new(e.settings.clone())),
                ),
                None => (Kind::Shot, None),
            };
            let frame = p.edit.as_ref().map_or(p.cursor, |e| e.frame);
            let tex = app.previews.get_or_near(
                Key {
                    frame,
                    kind,
                    edge: LARGE,
                },
                0.0,
                settings,
            );
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(centre_w, (height - 284.0).max(160.0)), Sense::hover());
            let shown = draw_picture(ui, rect, tex.as_ref(), false);
            // Picking the white balance: a click on the picture.
            let picking = app
                .project
                .as_ref()
                .is_some_and(|p| p.picking_wb && p.edit.is_some());
            if picking && tex.is_some() {
                let resp = ui.interact(shown, ui.scope_id().with("wb-pick"), Sense::click());
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                if resp.clicked()
                    && let Some(pos) = resp.interact_pointer_pos()
                {
                    let x = f64::from((pos.x - shown.left()) / shown.width().max(1.0));
                    let y = f64::from((pos.y - shown.top()) / shown.height().max(1.0));
                    app.white_balance(Some((x, y)));
                }
            } else
            // The selected mask, with handles to drag.
            if tex.is_some()
                && let Some(e) = app.project.as_ref().and_then(|p| p.edit.as_ref())
            {
                let (settings, selected) = (e.settings.clone(), e.mask);
                let mut actions = Vec::new();
                let mut painting = std::mem::take(&mut app.painting);
                crate::masks_ui::overlay(
                    ui,
                    shown,
                    &settings,
                    selected,
                    app.brush,
                    &mut painting,
                    &mut actions,
                );
                app.painting = painting;
                app.mask_actions(actions);
            }
            ui.add_space(8.0);
            filmstrip(app, ui, &all);
            ui.add_space(8.0);
            curve(app, ui, 170.0);
        });
        if editing_now {
            ui.allocate_ui_with_layout(
                Vec2::new(controls_w, height),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_width(controls_w);
                    controls(app, ui);
                },
            );
        }
    });
    copy_dialog(app, ui.ctx());
    if create {
        app.create_suggested();
    }
    if let Some(f) = make_auto {
        app.make_automatic(f);
    }
    if add_here {
        app.edit_frame(cursor);
    }
    if let Some(f) = pick {
        app.edit_frame(f);
    }
    // Moving to a keyframe in the curve opens it.
    if let Some(p) = &app.project
        && p.edit.as_ref().is_none_or(|e| e.frame != p.cursor)
        && all.contains(&p.cursor)
    {
        let c = p.cursor;
        app.edit_frame(c);
    }
}

/// The dialog copying the edited keyframe's settings: parts to tick, keyframes to tick.
fn copy_dialog(app: &mut App, ctx: &egui::Context) {
    let t = app.lang.t();
    let lang = app.lang;
    let Some(p) = &mut app.project else { return };
    let Some(d) = &mut p.copy else { return };
    let from = p.edit.as_ref().map_or(0, |e| e.frame);
    let parts = crate::sliders::copy_parts();
    let (mut go, mut close, mut make) = (false, false, false);
    egui::Modal::new(egui::Id::unique("copy-settings")).show(ctx, |ui| {
        // A margin of its own: the title and the lists off the dialog's edge.
        egui::Frame::new().inner_margin(12.0).show(ui, |ui| {
            ui.set_width(600.0);
            ui.label(theme::title(format!("{} {}", t.copy_title, from + 1)));
            ui.add_space(8.0);
            // The list scrolls in a small window, so the title and the buttons stay in view.
            let room = (ctx.content_rect().height() - 240.0).max(160.0);
            // A solid bar, seen at rest (the floating one is invisible until hovered).
            ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
            egui::ScrollArea::vertical()
                .id_salt("copy-parts")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .max_height(room)
                .show(ui, |ui| {
                    ui.columns(2, |cols| {
                        let [left, right] = cols else { return };
                        // Left-aligned, not stretched to the column (justified text left gaps).
                        left.with_layout(Layout::top_down(Align::Min), |left| {
                            left.label(RichText::new(t.copy_what).strong());
                            for (part, on) in parts.iter().zip(d.parts.iter_mut()) {
                                left.checkbox(on, part.name(lang));
                                // A term of the trade with a few plain words: its own grey line.
                                if let Some(h) = crate::sliders::part_hint(part.de, lang) {
                                    left.horizontal(|ui| {
                                        ui.add_space(26.0);
                                        ui.add(egui::Label::new(theme::small(h)).wrap());
                                    });
                                }
                                // The masks one by one, under their heading.
                                if part.masks && *on {
                                    for (name, pick) in &mut d.masks {
                                        left.horizontal(|ui| {
                                            ui.add_space(22.0);
                                            ui.checkbox(pick, name.as_str());
                                        });
                                    }
                                }
                            }
                        });
                        right.with_layout(Layout::top_down(Align::Min), |right| {
                            right.label(RichText::new(t.copy_to).strong());
                            if d.targets.is_empty() {
                                right.add(egui::Label::new(theme::muted(t.no_other_keyframes)).wrap());
                                if right.add(theme::secondary(t.create_suggested)).clicked() {
                                    make = true;
                                }
                            }
                            egui::ScrollArea::vertical().max_height(300.0).show(right, |ui| {
                                for (f, on) in &mut d.targets {
                                    ui.checkbox(on, format!("{} {}", t.frame_word, *f + 1));
                                }
                            });
                        });
                    });
                });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let any = d.parts.iter().any(|on| *on) && d.targets.iter().any(|(_, on)| *on);
                if ui.add_enabled(any, theme::primary(t.copy_go)).clicked() {
                    go = true;
                }
                if ui.add(theme::secondary(t.cancel)).clicked() {
                    close = true;
                }
            });
        });
    });
    if close {
        p.copy = None;
    }
    if go {
        app.copy_to_keyframes();
    }
    // The suggestions saved as keyframes, then the dialog again with them as targets.
    if make {
        app.create_suggested();
        app.open_copy();
    }
}

/// A histogram of the picture looked at: red, green, blue over each other, and a warning colour
/// at the ends when they are crowded (clipping).
fn draw_histogram(ui: &mut egui::Ui, h: &crate::preview::Histogram) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 64.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(6), theme::SURFACE_HI);
    let top = h.bins.iter().flatten().copied().max().unwrap_or(1).max(1) as f32;
    let colors = [
        Color32::from_rgba_unmultiplied(235, 80, 80, 110),
        Color32::from_rgba_unmultiplied(80, 210, 110, 110),
        Color32::from_rgba_unmultiplied(90, 140, 255, 110),
    ];
    let n = h.bins.first().map_or(0, |b| b.len()).max(1);
    for (bins, color) in h.bins.iter().zip(colors) {
        let mut pts = vec![rect.left_bottom()];
        for (i, v) in bins.iter().enumerate() {
            let x = rect.left() + rect.width() * (i as f32 + 0.5) / n as f32;
            // Square root: the quiet parts stay visible beside a big peak.
            let y = rect.bottom() - rect.height() * ((*v as f32) / top).sqrt();
            pts.push(Pos2::new(x, y));
        }
        pts.push(rect.right_bottom());
        painter.add(egui::Shape::line(pts, Stroke::new(1.2, color)));
    }
    for (clipped, x) in [(h.dark, rect.left() + 3.0), (h.bright, rect.right() - 3.0)] {
        if clipped > 0.005 {
            painter.circle_filled(Pos2::new(x, rect.top() + 5.0), 3.0, theme::WARN);
        }
    }
}

/// A codec's name for people.
fn codec_label(c: lapsewerk_video::Codec) -> &'static str {
    match c {
        lapsewerk_video::Codec::ProRes422Hq => "ProRes 422 HQ",
        lapsewerk_video::Codec::ProRes422 => "ProRes 422",
        lapsewerk_video::Codec::ProRes4444 => "ProRes 4444",
        lapsewerk_video::Codec::Hevc => "HEVC (H.265)",
    }
}

/// Thumbnails of the frames around the cursor (keyframes marked); a click moves there, the wheel
/// scrolls through the sequence.
fn filmstrip(app: &mut App, ui: &mut egui::Ui, keyframes: &[usize]) {
    let Some(p) = &mut app.project else { return };
    let n = p.frames();
    if n == 0 {
        return;
    }
    let (thumb_h, gap) = (60.0f32, 4.0f32);
    // The frames' shape: upright photos get upright cells, not landscape cuts of them.
    let thumb_w = if p.frames_upright() == Some(true) {
        thumb_h / 1.5
    } else {
        thumb_h * 1.5
    };
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, thumb_h + 8.0), Sense::click());
    let count = (((width + gap) / (thumb_w + gap)) as usize).clamp(1, 40).min(n);
    let first = p.cursor.saturating_sub(count / 2).min(n - count);
    let painter = ui.painter_at(rect);
    let mut clicked = None;
    for k in 0..count {
        let frame = first + k;
        let r = Rect::from_min_size(
            rect.min + Vec2::new(k as f32 * (thumb_w + gap), 4.0),
            Vec2::new(thumb_w, thumb_h),
        );
        let key = Key {
            frame,
            kind: Kind::Shot,
            edge: SMALL,
        };
        match app.previews.want(key, 0.0) {
            Some(tex) => {
                let [tw, th] = tex.size().map(|v| v as f32);
                // Cover the cell, cropped in the middle.
                let s = (thumb_w / tw.max(1.0)).max(thumb_h / th.max(1.0));
                let uv = Rect::from_center_size(
                    Pos2::new(0.5, 0.5),
                    Vec2::new((thumb_w / (tw * s)).min(1.0), (thumb_h / (th * s)).min(1.0)),
                );
                egui::Image::new((tex.id(), r.size()))
                    .uv(uv)
                    .corner_radius(CornerRadius::same(4))
                    .paint_at(ui, r);
            }
            None => {
                painter.rect_filled(r, CornerRadius::same(4), theme::SURFACE_HI);
            }
        }
        let (stroke, width) = if frame == p.cursor {
            (theme::TEXT, 2.0)
        } else if keyframes.contains(&frame) {
            (theme::ACCENT, 2.0)
        } else {
            (Color32::TRANSPARENT, 0.0)
        };
        if width > 0.0 {
            painter.rect_stroke(
                r,
                CornerRadius::same(4),
                Stroke::new(width, stroke),
                egui::StrokeKind::Outside,
            );
        }
        if keyframes.contains(&frame) {
            painter.circle_filled(r.right_top() + Vec2::new(-7.0, 7.0), 3.5, theme::ACCENT);
        }
        painter.text(
            r.left_bottom() + Vec2::new(4.0, -3.0),
            Align2::LEFT_BOTTOM,
            (frame + 1).to_string(),
            FontId::proportional(10.0),
            Color32::from_white_alpha(200),
        );
        if response.clicked() && response.interact_pointer_pos().is_some_and(|q| r.contains(q)) {
            clicked = Some(frame);
        }
    }
    if let Some(f) = clicked {
        p.cursor = f;
    }
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.x + i.smooth_scroll_delta.y);
        if scroll.abs() > 0.5 {
            let step = if scroll > 0.0 { -1 } else { 1 };
            p.cursor = p.cursor.saturating_add_signed(step).min(n - 1);
        }
    }
}

/// A keyframe in the list: its frame, and whether it is saved or a suggestion.
fn keyframe_row(
    ui: &mut egui::Ui,
    width: f32,
    frame: usize,
    saved: Option<bool>,
    locked: bool,
    selected: bool,
    t: &Texts,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::click());
    let p = ui.painter();
    let fill = if selected {
        theme::SURFACE_HI
    } else if response.hovered() {
        theme::SURFACE
    } else {
        Color32::TRANSPARENT
    };
    p.rect(
        rect,
        CornerRadius::same(10),
        fill,
        Stroke::new(1.0, if selected { theme::ACCENT } else { theme::LINE }),
        egui::StrokeKind::Inside,
    );
    // A diamond like the curve's marks: filled when saved.
    let c = Pos2::new(rect.left() + 18.0, rect.center().y);
    let d = 5.5;
    let pts = vec![
        c + Vec2::new(0.0, -d),
        c + Vec2::new(d, 0.0),
        c + Vec2::new(0.0, d),
        c + Vec2::new(-d, 0.0),
    ];
    if saved.is_some() {
        p.add(egui::Shape::convex_polygon(pts, theme::ACCENT, Stroke::NONE));
    } else {
        p.add(egui::Shape::closed_line(pts, Stroke::new(1.5, theme::ACCENT)));
    }
    p.text(
        Pos2::new(rect.left() + 34.0, rect.center().y),
        Align2::LEFT_CENTER,
        format!("{} {}", t.frame_word, frame + 1),
        FontId::proportional(14.5),
        theme::TEXT,
    );
    let (badge, color) = match saved {
        Some(true) => (t.saved, theme::GOOD),
        // Automatic, but its XMP read-only or linked: it keeps its look.
        Some(false) if locked => (t.auto_locked, theme::WARN),
        Some(false) => (t.auto_keyframe, theme::MUTED),
        None => (t.suggested, theme::MUTED),
    };
    p.text(
        Pos2::new(rect.right() - 12.0, rect.center().y),
        Align2::RIGHT_CENTER,
        badge,
        FontId::proportional(12.0),
        color,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn controls(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    let Some(p) = &app.project else { return };
    let Some(e) = &p.edit else { return };
    let frame = e.frame;
    let dirty = e.dirty();
    let (can_undo, can_redo) = (!e.undo.is_empty(), !e.redo.is_empty());
    let lang = app.lang;
    let settings = e.settings.clone();
    let selected = e.mask;
    let brush = app.brush;
    let app_picking = p.picking_wb;
    let lightcraft_found = app.lightcraft_found;
    let histogram = app
        .previews
        .histogram_near(e.frame, crate::preview::Kind::Edit(e.version));
    let mut changes: Vec<(&str, f64)> = Vec::new();
    let mut mask_actions: Vec<crate::masks_ui::Action> = Vec::new();
    theme::card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(theme::title(format!("{} {}", t.frame_word, frame + 1)));
        ui.add_space(4.0);
        // Saving and undoing first, so they stay in view however far the groups are scrolled.
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(dirty, theme::primary(t.save_keyframe)).clicked() {
                changes.push(("__save", 0.0));
            }
            if ui.add_enabled(dirty, theme::secondary(t.reset)).clicked() {
                changes.push(("__reset", 0.0));
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(can_undo, egui::Button::new(t.undo).small())
                .on_hover_text(t.undo_keys)
                .clicked()
            {
                changes.push(("__undo", 0.0));
            }
            if ui
                .add_enabled(can_redo, egui::Button::new(t.redo).small())
                .on_hover_text(t.redo_keys)
                .clicked()
            {
                changes.push(("__redo", 0.0));
            }
        });
        if dirty {
            ui.label(RichText::new(t.unsaved).color(theme::WARN).size(12.5));
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(egui::Button::new(t.copy_button).small())
                .on_hover_text(t.copy_tip)
                .clicked()
            {
                changes.push(("__copy", 0.0));
            }
            if lightcraft_found
                && ui
                    .add(egui::Button::new(t.open_lightcraft).small())
                    .on_hover_text(t.open_lightcraft_tip)
                    .clicked()
            {
                changes.push(("__lightcraft", 0.0));
            }
        });
        ui.add_space(4.0);
        let label_w = 112.0;
        let inner_w = ui.available_width();
        egui::ScrollArea::vertical()
            .id_salt("keyframe-controls")
            .auto_shrink([false, true])
            .max_width(inner_w)
            .show(ui, |ui| {
                ui.set_max_width(inner_w - 12.0);
                // Auto, and the white balance from the frame or from a spot clicked in the picture.
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add(egui::Button::new(t.auto).small())
                        .on_hover_text(t.auto_tip)
                        .clicked()
                    {
                        changes.push(("__auto", 0.0));
                    }
                    if ui.add(egui::Button::new(t.wb_auto).small()).clicked() {
                        changes.push(("__wbauto", 0.0));
                    }
                    let picking = app_picking;
                    if ui
                        .add(egui::Button::new(t.wb_pick).small().selected(picking))
                        .on_hover_text(t.wb_pick_tip)
                        .clicked()
                    {
                        changes.push(("__wbpick", 0.0));
                    }
                });
                if let Some(h) = &histogram {
                    draw_histogram(ui, h);
                }
                for (k, g) in crate::sliders::GROUPS.iter().enumerate() {
                    // The masks right after the basic group: they matter most for a time-lapse.
                    if k == 1 {
                        crate::masks_ui::panel(
                            ui,
                            lang,
                            &settings,
                            selected,
                            brush,
                            label_w,
                            &mut mask_actions,
                        );
                    }
                    let specs = g.specs();
                    // A dot marks a group with something set.
                    let changed = specs.iter().any(|s| {
                        lapsewerk_develop::controls::get(&settings, s.id)
                            .is_some_and(|v| (v - s.default).abs() > 1e-9)
                    });
                    let title = if changed {
                        format!("{}  •", g.name(lang))
                    } else {
                        g.name(lang).to_owned()
                    };
                    egui::CollapsingHeader::new(RichText::new(title).strong())
                        .id_salt(("group", g.en))
                        .default_open(g.open)
                        .show(ui, |ui| {
                            ui.spacing_mut().slider_width =
                                (ui.available_width() - label_w - 100.0).max(60.0);
                            for spec in specs {
                                let value = lapsewerk_develop::controls::get(&settings, spec.id)
                                    .unwrap_or(spec.default);
                                ui.horizontal(|ui| {
                                    ui.allocate_ui_with_layout(
                                        Vec2::new(label_w, 26.0),
                                        Layout::left_to_right(Align::Center),
                                        |ui| {
                                            ui.set_width(label_w);
                                            ui.add(
                                                egui::Label::new(theme::muted(crate::sliders::label(
                                                    spec, lang,
                                                )))
                                                .truncate(),
                                            );
                                        },
                                    );
                                    let mut v = value;
                                    // Kelvin: the everyday range, spaced as the eye sees it.
                                    let (lo, hi) = if spec.id == "wb.temp" {
                                        (2000.0, 15000.0)
                                    } else {
                                        (spec.min, spec.max)
                                    };
                                    let r = ui
                                        .add(
                                            theme::localized(
                                                egui::Slider::new(&mut v, lo..=hi),
                                                lang,
                                                usize::from(spec.decimals),
                                            )
                                            .logarithmic(spec.id == "wb.temp"),
                                        )
                                        .on_hover_text(t.reset_hint);
                                    if r.double_clicked() {
                                        changes.push((spec.id, spec.default));
                                    } else if r.changed() && (v - value).abs() > 1e-9 {
                                        changes.push((spec.id, v));
                                    }
                                });
                            }
                        });
                }
            });
    });
    for (id, v) in changes {
        match id {
            "__save" => app.save_edit(),
            "__reset" => app.edit_frame(frame),
            "__copy" => app.open_copy(),
            "__auto" => app.auto_tone(),
            "__wbauto" => app.white_balance(None),
            "__wbpick" => {
                if let Some(p) = &mut app.project {
                    p.picking_wb = !p.picking_wb;
                }
            }
            "__lightcraft" => app.open_in_lightcraft(),
            "__undo" => app.undo(true),
            "__redo" => app.undo(false),
            id => app.set_control(id, v),
        }
    }
    app.mask_actions(mask_actions);
}

// ---------------------------------------------------------------------------------------------
// Step 4: finishing

fn finishing(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    let lang = app.lang;
    let advanced = app.open_advanced;
    let (light, recommend) = app
        .project
        .as_ref()
        .and_then(|p| p.analysis.as_ref())
        .map_or((0.0, false), |a| (a.light_ranges().0, a.day_to_night()));
    // A picture of the sequence for the framing card: the frame looked at, as shot.
    let thumb = app.project.as_ref().map(|p| Key {
        frame: p.cursor,
        kind: Kind::Shot,
        edge: SMALL,
    });
    let thumb = thumb.and_then(|k| app.previews.get_or_near(k, 0.0, None));
    let Some(p) = &mut app.project else { return };
    let estimate = p.flicker_estimate();
    // Whether the night sky work finds a sky in the first frame (looked for once, when used).
    let night_on = p.finish.light_pollution || p.finish.reduce_stars.is_some() || p.finish.trails.is_some();
    let sky_found = if night_on { Some(p.sky_found()) } else { None };
    let sky_moving = p.sky_moving;
    let sky_frame = p.sky_frame;
    // For the hand-drawn curve: the saved keyframes, and the frame looked at.
    let keyframes = p.keyframes();
    let cursor = p.cursor;
    let column = 780.0f32.min(ui.available_width());
    let pad = ((ui.available_width() - column) / 2.0).max(0.0);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.add_space(pad);
                ui.vertical(|ui| {
                    ui.set_width(column);
                    let f = &mut p.finish;
                    ui.label(theme::muted(t.finish_hint));
                    ui.add_space(6.0);
                    option(ui, t.deflicker, t.deflicker_detail, &mut f.deflicker, |ui, _| {
                        ui.horizontal(|ui| {
                            ui.label(t.gentle);
                            ui.add(
                                egui::Slider::new(&mut f.smoothing, 3.0..=24.0)
                                    .logarithmic(true)
                                    .show_value(false),
                            )
                            .on_hover_text(t.strength_tip);
                            ui.label(t.strong);
                        });
                        if let Some((before, after)) = estimate {
                            // In words; the figures (stops, frame to frame) on hover.
                            let word = |v: f64| {
                                let level = if v < 0.01 {
                                    0
                                } else if v < 0.04 {
                                    1
                                } else {
                                    2
                                };
                                t.flicker_levels.get(level).copied().unwrap_or("")
                            };
                            ui.label(theme::small(
                                t.flicker_estimate
                                    .replace("{a}", word(before))
                                    .replace("{b}", word(after)),
                            ))
                            .on_hover_text(format!(
                                "{} → {} {}  ·  {}",
                                crate::text::num(lang, before, 3),
                                crate::text::num(lang, after, 3),
                                t.stops,
                                t.flicker_estimate_tip
                            ));
                        }
                        ui.checkbox(&mut f.thorough, t.thorough);
                    });
                    let mut day_night = f.keep_light.is_some();
                    if light > 1.0 || day_night {
                        let name = if recommend {
                            format!("{}   ·   {}", t.day_night, t.recommended)
                        } else {
                            t.day_night.to_owned()
                        };
                        option(ui, &name, t.day_night_detail, &mut day_night, |ui, on| {
                            if on {
                                let mut k = f.keep_light.unwrap_or(crate::app::KEEP_LIGHT_DEFAULT) * 100.0;
                                ui.horizontal(|ui| {
                                    ui.label(theme::small(t.light_even));
                                    ui.add(
                                        theme::localized(egui::Slider::new(&mut k, 0.0..=100.0), lang, 0)
                                            .suffix(" %"),
                                    )
                                    .on_hover_text(t.keep_light);
                                    ui.label(theme::small(t.light_real));
                                });
                                f.keep_light = Some(k / 100.0);
                                ui.checkbox(&mut f.hold_blacks, t.hold_blacks)
                                    .on_hover_text(t.hold_blacks_tip);
                            }
                        });
                        if !day_night {
                            f.keep_light = None;
                        } else if f.keep_light.is_none() {
                            f.keep_light = Some(crate::app::KEEP_LIGHT_DEFAULT);
                        }
                    }
                    // The brightness shaped by hand, on the curve (as a holy-grail curve).
                    if let Some(a) = p.analysis.as_ref() {
                        let smoothing = f.deflicker.then_some(f64::from(f.smoothing));
                        let keep = f.keep_light.map(f64::from);
                        let plain = a.planned_offsets(smoothing, keep, &[]);
                        let shape = lapsewerk_core::daynight::shaped(&f.brightness, plain.len());
                        let base: Vec<Option<f64>> = a
                            .as_shot
                            .stops
                            .iter()
                            .zip(&plain)
                            .map(|(s, o)| s.map(|s| s + o))
                            .collect();
                        let video: Vec<Option<f64>> =
                            base.iter().zip(&shape).map(|(b, s)| b.map(|b| b + s)).collect();
                        // The frame "+ point" uses: chosen in the list's field, the frame looked at
                        // to begin with.
                        let add_id = egui::Id::unique("shape-add-at");
                        let add_at = ui
                            .ctx()
                            .data(|d| d.get_temp::<usize>(add_id))
                            .unwrap_or(cursor)
                            .min(plain.len().saturating_sub(1));
                        egui::CollapsingHeader::new(RichText::new(t.shape_title).strong())
                            .id_salt("shape")
                            .default_open(!f.brightness.is_empty())
                            .show(ui, |ui| {
                                ui.label(theme::small(t.shape_hint));
                                crate::curve::shape_editor(
                                    ui,
                                    &base,
                                    &video,
                                    &mut f.brightness,
                                    // Room for the stops of a deep holy grail.
                                    320.0,
                                    &crate::curve::ShapeWords {
                                        frame: t.frame_word,
                                        axis: t.curve_axis,
                                        with_points: t.shape_with,
                                        without: t.shape_without,
                                        stops_short: t.stops_short,
                                        keyframes: &keyframes,
                                        cursor: Some(add_at),
                                        light: &a.compensated,
                                        light_word: t.shape_light,
                                    },
                                    lang == crate::text::Lang::De,
                                );
                                // Each point exactly: its frame and its change, typed or dragged finely
                                // (fields of one width, so the rows line up).
                                let n = plain.len().max(1);
                                let mut remove = None;
                                for (k, (fr, st)) in f.brightness.iter_mut().enumerate() {
                                    ui.allocate_ui_with_layout(
                                        Vec2::new(ui.available_width(), 40.0),
                                        Layout::left_to_right(Align::Center),
                                        |ui| {
                                            let mut shown = *fr + 1;
                                            ui.label(t.frame_word);
                                            ui.add_sized(
                                                [56.0, 34.0],
                                                egui::DragValue::new(&mut shown).range(1..=n).speed(0.2),
                                            );
                                            *fr = shown.saturating_sub(1);
                                            ui.add_sized(
                                                [110.0, 34.0],
                                                theme::localized_drag(
                                                    egui::DragValue::new(st).range(-6.0..=6.0).speed(0.01),
                                                    lang,
                                                    2,
                                                )
                                                .suffix(format!(" {}", t.stops_short)),
                                            );
                                            if ui.small_button("×").on_hover_text(t.shape_remove).clicked() {
                                                remove = Some(k);
                                            }
                                        },
                                    );
                                }
                                if let Some(k) = remove {
                                    f.brightness.remove(k);
                                }
                                f.brightness.sort_by_key(|&(fr, _)| fr);
                                ui.allocate_ui_with_layout(
                                    Vec2::new(ui.available_width(), 40.0),
                                    Layout::left_to_right(Align::Center),
                                    |ui| {
                                        // A point at a frame chosen here (the white line; the frame
                                        // looked at to begin with), where the curve is now.
                                        let mut shown = add_at + 1;
                                        ui.label(t.shape_new_at);
                                        ui.add_sized(
                                            [56.0, 34.0],
                                            egui::DragValue::new(&mut shown).range(1..=n).speed(0.2),
                                        );
                                        let at = shown.saturating_sub(1);
                                        ui.ctx().data_mut(|d| d.insert_temp(add_id, at));
                                        let taken = f.brightness.iter().any(|(fr, _)| *fr == at);
                                        if taken {
                                            ui.label(theme::muted(t.shape_has_point));
                                        }
                                        if !taken && ui.button(t.shape_add).clicked() {
                                            let now = shape.get(at).copied().unwrap_or(0.0);
                                            f.brightness.push((at, (now * 100.0).round() / 100.0));
                                            f.brightness.sort_by_key(|&(fr, _)| fr);
                                        }
                                        if !f.brightness.is_empty() && ui.button(t.shape_reset).clicked() {
                                            f.brightness.clear();
                                        }
                                    },
                                );
                            });
                        ui.add_space(6.0);
                    }
                    option(ui, t.stabilize, t.stabilize_detail, &mut f.stabilize, |ui, _| {
                        ui.checkbox(&mut f.subpixel, t.subpixel)
                            .on_hover_text(t.subpixel_tip);
                    });
                    option(ui, t.dust, t.dust_detail, &mut f.dust, |_, _| {});
                    option(
                        ui,
                        t.smooth_motion,
                        t.smooth_motion_detail,
                        &mut f.smooth_motion,
                        |_, _| {},
                    );
                    let mut tween = f.tween > 0;
                    option(ui, t.tween, t.tween_detail, &mut tween, |ui, _| {
                        ui.horizontal(|ui| {
                            ui.label(t.tween_count);
                            for n in 1..=3usize {
                                ui.selectable_value(&mut f.tween, n, n.to_string());
                            }
                        });
                        ui.label(theme::small(
                            t.tween_longer.replace("{n}", &(f.tween.max(1) + 1).to_string()),
                        ));
                    });
                    if !tween {
                        f.tween = 0;
                    } else if f.tween == 0 {
                        f.tween = 1;
                    }
                    option(ui, t.denoise, t.denoise_detail, &mut f.denoise, |_, _| {});
                    let mut night = f.light_pollution || f.reduce_stars.is_some() || f.trails.is_some();
                    option(ui, t.night_sky, t.night_sky_detail, &mut night, |ui, on| {
                        if !on {
                            return;
                        }
                        ui.checkbox(&mut f.light_pollution, t.light_pollution);
                        let mut dim = f.reduce_stars.is_some();
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut dim, t.reduce_stars);
                            if dim {
                                let mut v = f.reduce_stars.unwrap_or(0.5) * 100.0;
                                ui.add(
                                    egui::Slider::new(&mut v, 10.0..=100.0)
                                        .suffix(" %")
                                        .fixed_decimals(0),
                                );
                                f.reduce_stars = Some(v / 100.0);
                            } else {
                                f.reduce_stars = None;
                            }
                        });
                        let mut trails = f.trails.is_some();
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut trails, t.star_trails);
                            if trails {
                                let mut n = f.trails.unwrap_or(0) as f64;
                                ui.add(egui::Slider::new(&mut n, 0.0..=200.0).custom_formatter(|v, _| {
                                    if v < 0.5 {
                                        t.trails_endless.to_owned()
                                    } else {
                                        format!("{v:.0}")
                                    }
                                }));
                                f.trails = Some(n.round() as usize);
                            } else {
                                f.trails = None;
                            }
                        });
                        // Where it works: the sky found along the horizon, else the area given.
                        match sky_found {
                            Some(Some(share)) => {
                                let mut said = t.sky_found.replace("{p}", &format!("{:.0}", share * 100.0));
                                // Found in a later frame (none in the first): said, for a camera that moves.
                                if let Some(i) = sky_frame.filter(|i| *i > 0) {
                                    said.push(' ');
                                    said.push_str(&t.sky_found_later.replace("{n}", &(i + 1).to_string()));
                                }
                                if !sky_moving {
                                    said.push(' ');
                                    said.push_str(t.sky_slider_off);
                                }
                                ui.label(theme::small(said));
                                if sky_moving {
                                    ui.label(RichText::new(t.sky_moving).color(theme::WARN).size(12.5));
                                }
                            }
                            Some(None) => {
                                ui.label(RichText::new(t.sky_not_found).color(theme::WARN).size(12.5));
                            }
                            None => {}
                        }
                        let mut sky = f.sky * 100.0;
                        // A sky found decides where the night sky work acts: the share above
                        // then does nothing (shown so, not left to be tried in vain).
                        ui.add_enabled(
                            !matches!(sky_found, Some(Some(_))) || sky_moving,
                            egui::Slider::new(&mut sky, 20.0..=100.0)
                                .suffix(" %")
                                .text(t.sky_area),
                        );
                        f.sky = sky / 100.0;
                    });
                    if !night {
                        f.light_pollution = false;
                        f.reduce_stars = None;
                        f.trails = None;
                    }
                    camera(ui, t, lang, p, thumb.as_ref());
                    let f = &mut p.finish;
                    ui.add_space(8.0);
                    egui::CollapsingHeader::new(RichText::new(t.advanced).strong())
                        .default_open(advanced)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(t.reference_area);
                                // 0: whole frame, 1: upper part, 2: lower part.
                                let mut part = match (f.reference.is_some(), f.reference_low) {
                                    (false, _) => 0,
                                    (true, false) => 1,
                                    (true, true) => 2,
                                };
                                ui.selectable_value(&mut part, 0, t.whole_frame);
                                ui.selectable_value(&mut part, 1, t.upper_part);
                                ui.selectable_value(&mut part, 2, t.lower_part);
                                f.reference_low = part == 2;
                                let upper = part > 0;
                                if upper {
                                    let mut h = f.reference.unwrap_or(0.4) * 100.0;
                                    ui.add(
                                        egui::Slider::new(&mut h, 10.0..=90.0)
                                            .suffix(" %")
                                            .fixed_decimals(0),
                                    );
                                    f.reference = Some(h / 100.0);
                                } else {
                                    f.reference = None;
                                }
                            });
                            ui.add_enabled(
                                f.reference.is_none(),
                                egui::Checkbox::new(&mut f.regional, t.regional),
                            );
                            if f.reference.is_some() {
                                f.regional = false;
                            }
                            ui.horizontal(|ui| {
                                ui.label(t.measure_size);
                                for e in [512usize, 768, 1024] {
                                    ui.selectable_value(&mut f.edge, e, format!("{e} px"));
                                }
                            });
                        });
                });
            });
        });
}

/// Framing and camera move: the kind of move, how far, where, and a picture with the view at the
/// start and at the end.
fn camera(
    ui: &mut egui::Ui,
    t: &Texts,
    lang: crate::text::Lang,
    p: &mut crate::app::ProjectState,
    thumb: Option<&egui::TextureHandle>,
) {
    use crate::app::CameraMove;
    theme::card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(t.camera).strong());
        ui.label(theme::small(t.camera_detail));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            // The picture: the frame, the view at the start (white) and at the end (accent).
            let size = Vec2::new(240.0, 160.0);
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            let shown = draw_picture(ui, rect, thumb, false);
            if let Some(tex) = thumb {
                let n = p.frames().max(1);
                let tsize = tex.size().map(|v| v as f32);
                let frame_rect = |uv: [f32; 4]| {
                    Rect::from_min_size(
                        shown.min + Vec2::new(uv[0] * shown.width(), uv[1] * shown.height()),
                        Vec2::new(uv[2] * shown.width(), uv[3] * shown.height()),
                    )
                };
                let start = frame_rect(p.view_uv(0, tsize));
                let end = frame_rect(p.view_uv(n - 1, tsize));
                let painter = ui.painter_at(rect);
                // Outside the view: dimmed.
                let dim = Color32::from_black_alpha(110);
                let both = start.union(end);
                for r in [
                    Rect::from_min_max(shown.min, Pos2::new(shown.right(), both.top())),
                    Rect::from_min_max(Pos2::new(shown.left(), both.bottom()), shown.max),
                    Rect::from_min_max(
                        Pos2::new(shown.left(), both.top()),
                        Pos2::new(both.left(), both.bottom()),
                    ),
                    Rect::from_min_max(
                        Pos2::new(both.right(), both.top()),
                        Pos2::new(shown.right(), both.bottom()),
                    ),
                ] {
                    if r.is_positive() {
                        painter.rect_filled(r, CornerRadius::ZERO, dim);
                    }
                }
                painter.rect_stroke(
                    start,
                    CornerRadius::ZERO,
                    Stroke::new(1.5, theme::TEXT),
                    egui::StrokeKind::Inside,
                );
                if p.camera.kind != CameraMove::Still {
                    painter.rect_stroke(
                        end,
                        CornerRadius::ZERO,
                        Stroke::new(2.0, theme::ACCENT),
                        egui::StrokeKind::Inside,
                    );
                    painter.arrow(
                        start.center(),
                        end.center() - start.center(),
                        Stroke::new(1.5, theme::ACCENT),
                    );
                    let tag = |at: Pos2, text: &str, c: Color32| {
                        painter.text(at, Align2::LEFT_TOP, text, FontId::proportional(10.5), c);
                    };
                    tag(
                        start.left_top() + Vec2::new(3.0, 2.0),
                        t.camera_start,
                        theme::TEXT,
                    );
                    tag(
                        end.left_bottom() + Vec2::new(3.0, -14.0),
                        t.camera_end,
                        theme::ACCENT,
                    );
                }
            }
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    for (m, name) in CameraMove::ALL.into_iter().zip(t.camera_moves) {
                        ui.selectable_value(&mut p.camera.kind, m, name);
                    }
                });
                ui.add_space(4.0);
                let c = &mut p.camera;
                if c.kind != CameraMove::Still {
                    let mut z = 1.0 + c.amount;
                    ui.add(
                        theme::localized(egui::Slider::new(&mut z, 1.05..=2.0), lang, 2)
                            .suffix("×")
                            .text(t.camera_amount),
                    );
                    c.amount = z - 1.0;
                }
                if !matches!(c.kind, CameraMove::PanLeft | CameraMove::PanRight) {
                    ui.add(
                        egui::Slider::new(&mut c.x, 0.0..=1.0)
                            .show_value(false)
                            .text(t.camera_across),
                    );
                }
                ui.add(
                    egui::Slider::new(&mut c.y, 0.0..=1.0)
                        .show_value(false)
                        .text(t.camera_down),
                );
            });
        });
    });
    ui.add_space(4.0);
}

/// An option card: a switch, its name and what it does, and its own settings while on.
fn option(
    ui: &mut egui::Ui,
    name: &str,
    detail: &str,
    on: &mut bool,
    inner: impl FnOnce(&mut egui::Ui, bool),
) {
    theme::card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            toggle(ui, on);
            ui.vertical(|ui| {
                ui.label(RichText::new(name).strong());
                ui.label(theme::small(detail));
            });
        });
        let on = *on;
        if on {
            ui.add_space(4.0);
            ui.indent(name, |ui| inner(ui, on));
        }
    });
    ui.add_space(4.0);
}

/// An iOS-like switch.
fn toggle(ui: &mut egui::Ui, on: &mut bool) {
    let size = Vec2::new(44.0, 26.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let how_on = ui.ctx().animate_bool(response.id, *on);
    let p = ui.painter();
    let bg = if *on { theme::ACCENT } else { theme::SURFACE_HI };
    p.rect_filled(rect, CornerRadius::same(13), bg);
    let x = egui::lerp(rect.left() + 13.0..=rect.right() - 13.0, how_on);
    p.circle_filled(
        Pos2::new(x, rect.center().y),
        10.0,
        if *on { theme::ON_ACCENT } else { theme::MUTED },
    );
}

// ---------------------------------------------------------------------------------------------
// Step 5: preview

fn preview_page(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    let Some(p) = &app.project else { return };
    if p.corrected.is_none() {
        if app.task.is_some() {
            waiting(ui, t, task_status(app), false);
        } else {
            ui.add_space(30.0);
            if ui.add(theme::primary(t.compute_preview)).clicked() {
                app.start_correct();
            }
        }
        return;
    }
    let proofing = app
        .task
        .as_ref()
        .filter(|(k, _)| *k == TaskKind::Proof)
        .map(|(_, task)| task.status());
    let mut proof = false;
    let mut measure_result = false;
    let app_idle = app.task.is_none();
    let mut proof_said: Option<&str> = None;
    let Some(p) = &mut app.project else { return };
    ui.horizontal(|ui| {
        for (mode, label) in [PreviewMode::Play, PreviewMode::Compare, PreviewMode::Frames]
            .into_iter()
            .zip(t.preview_modes)
        {
            if ui
                .selectable_label(p.mode == mode, RichText::new(label).size(15.0))
                .clicked()
            {
                p.mode = mode;
                p.playing = None;
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            match &proofing {
                Some(s) => {
                    ui.add(
                        egui::ProgressBar::new(s.fraction().unwrap_or(0.0))
                            .desired_width(160.0)
                            .text(format!(
                                "{} {:.0} %",
                                t.proof,
                                s.fraction().unwrap_or(0.0) * 100.0
                            )),
                    );
                }
                None => {
                    if ui
                        .add(theme::secondary(t.proof))
                        .on_hover_text(t.proof_detail)
                        .clicked()
                    {
                        proof = true;
                    }
                    if ui
                        .add_enabled(app_idle, theme::secondary(t.measure_result))
                        .on_hover_text(t.measure_result_tip)
                        .clicked()
                    {
                        measure_result = true;
                    }
                    // The last test video: to play again or find.
                    if let Some(made) = p.proof.clone() {
                        if ui.link(t.show_in_folder).clicked() {
                            App::reveal(&made);
                        }
                        // A folder of frames (no movie files on this system) is shown, not played.
                        let frames = made.is_dir();
                        if !frames && ui.link(t.play_again).clicked() {
                            App::play(&made);
                        }
                        proof_said = Some(if frames { t.proof_done_frames } else { t.proof_done });
                    }
                }
            }
        });
    });
    // On their own lines: beside the test-video links they ran over the mode buttons.
    if let Some(said) = proof_said {
        ui.add(egui::Label::new(RichText::new(said).color(theme::GOOD)).wrap());
    }
    // What "Measure result" found in the finished look.
    if let Some((whole, worst)) = app.project.as_ref().and_then(|p| p.measured) {
        let lang = app.lang;
        let num = |v: f64| crate::text::num(lang, v, 4);
        // In words first: what a viewer would see.
        let wave = app.project.as_ref().and_then(|p| p.measured_wave);
        let why = app
            .project
            .as_ref()
            .map_or(crate::app::WaveWhy::Look, |p| p.measured_wave_why);
        // A swell that shows (from 0.05 stops) and is not drawn so on purpose says so first.
        let wave_shows = wave.is_some_and(|(_, a)| a > 0.05) && why != crate::app::WaveWhy::Drawn;
        let verdict = if wave_shows && whole <= 0.01 {
            t.measured_wave_visible
        } else if whole <= 0.005 && worst.is_none_or(|(_, j)| j <= 0.01) {
            t.measured_calm
        } else if whole <= 0.01 {
            t.measured_slight
        } else {
            t.measured_visible
        };
        // In two lines: what to do in plain words, then the figures (smaller).
        let mut head = verdict.to_owned();
        let mut line = t.measured_line.replace("{v}", &num(whole));
        let left_out = app.project.as_ref().map_or(0, |p| p.measured_left_out);
        if left_out > 0 {
            line.push(' ');
            line.push_str(&t.measured_left_out.replace("{n}", &left_out.to_string()));
        }
        // Yellow only for what shows (about a hundredth of a stop; a slow swell from 0.05 stops,
        // unless drawn so by hand), the whole or a stretch.
        let calm = whole <= 0.01 && worst.is_none_or(|(_, j)| j <= 0.01) && !wave_shows;
        let (jump, step, len) = app.project.as_ref().map_or((false, false, 0), |p| {
            (p.measured_jump, p.measured_step, p.measured_len)
        });
        if let Some((a, j)) = worst {
            // Deflicker off: the advice is to switch it on (an area to measure acts only with it).
            let off = app.project.as_ref().is_some_and(|p| !p.finish.deflicker);
            if jump && off && whole > 0.01 {
                // The whole video flickers with the correction off: that, not one place, is said;
                // the largest jump is named among the figures.
                head.push(' ');
                head.push_str(t.measured_jump_nodefl);
                line.push(' ');
                line.push_str(&t.measured_jump_size.replace("{v}", &num(j)));
            } else if jump {
                head.push(' ');
                head.push_str(
                    &if step {
                        t.measured_step_head
                    } else {
                        t.measured_spike_head
                    }
                    .replace("{a}", &(a + 1).to_string())
                    .replace("{b}", &(a + 2).to_string()),
                );
                line.push(' ');
                line.push_str(&t.measured_jump_size.replace("{v}", &num(j)));
                line.push(' ');
                line.push_str(if off {
                    t.measured_jump_nodefl
                } else {
                    t.measured_jump
                });
            } else {
                let b = (a + 25).min(len.max(a + 1));
                // Below what shows: named, without advice.
                let said = if j <= 0.01 {
                    t.measured_worst_calm
                } else if off {
                    t.measured_worst_nodefl
                } else {
                    t.measured_worst
                };
                line.push(' ');
                line.push_str(
                    &said
                        .replace("{a}", &(a + 1).to_string())
                        .replace("{b}", &b.to_string())
                        .replace("{v}", &num(j)),
                );
            }
        }
        if let Some((at, a)) = wave {
            let len = app.project.as_ref().map_or(0, |p| p.measured_len);
            let (from, to) = app
                .project
                .as_ref()
                .and_then(|p| p.measured_wave_span)
                .unwrap_or((at.saturating_sub(10), (at + 10).min(len.saturating_sub(1))));
            line.push(' ');
            line.push_str(
                &t.measured_wave
                    .replace("{a}", &(from + 1).to_string())
                    .replace("{b}", &(to + 1).to_string())
                    .replace("{v}", &crate::text::num(lang, a, 3)),
            );
            // "Remove flicker is off" is said once (a jump's advice may have said it).
            let said_off = worst.is_some_and(|(_, j)| jump || j > 0.01)
                && app.project.as_ref().is_some_and(|p| !p.finish.deflicker);
            let why_text = match why {
                crate::app::WaveWhy::Drawn => Some(t.measured_wave_drawn),
                crate::app::WaveWhy::NoDeflicker if said_off => None,
                crate::app::WaveWhy::NoDeflicker => Some(t.measured_wave_nodefl),
                crate::app::WaveWhy::Look => Some(t.measured_wave_look),
            };
            if let Some(w) = why_text {
                line.push(' ');
                line.push_str(w);
            }
        }
        // Where to look, one click away: the frames there, one by one.
        // With the correction off and the whole video flickering, the way out is step 4, not a photo.
        let off_all = app.project.as_ref().is_some_and(|p| !p.finish.deflicker) && whole > 0.01;
        let go_to = worst
            .filter(|_| !off_all && (jump || worst.is_some_and(|(_, j)| j > 0.01)))
            .map(|(a, _)| if jump && step { a + 1 } else { a });
        let mut go = None;
        let mut to_step4 = false;
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::Label::new(RichText::new(head).color(if calm { theme::GOOD } else { theme::WARN }))
                    .wrap(),
            );
            if let Some(f) = go_to
                && ui
                    .add(theme::secondary(
                        t.go_to_photo.replace("{n}", &(f + 1).to_string()),
                    ))
                    .clicked()
            {
                go = Some(f);
            }
            if off_all && ui.add(theme::secondary(t.to_step4)).clicked() {
                to_step4 = true;
            }
        });
        ui.add(egui::Label::new(theme::small(line)).wrap());
        if let (Some(f), Some(p)) = (go, app.project.as_mut()) {
            p.cursor = f.min(p.frames().saturating_sub(1));
            p.mode = PreviewMode::Frames;
            p.playing = None;
        }
        if to_step4 {
            app.go(4);
        }
    }
    ui.label(theme::small(t.preview_note));
    if proof {
        app.start_export(true);
    }
    if measure_result {
        app.start_measure_result();
    }
    let Some(p) = &app.project else { return };
    let (vw, vh) = p.preview_video();
    ui.add_space(6.0);
    if vh > vw {
        // An upright video: the picture as tall as there is room, the curve beside it (below it,
        // it would leave the picture a narrow strip in a wide empty field).
        let h = (ui.available_height() - 12.0).max(260.0);
        let pic_h = h - 60.0;
        // Wide enough for the slider below it.
        let pic_w = (pic_h * vw as f32 / vh as f32)
            .max(380.0)
            .min(ui.available_width() * 0.5);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(Vec2::new(pic_w, h), Layout::top_down(Align::Min), |ui| {
                ui.set_width(pic_w);
                preview_pictures(app, ui, pic_h);
            });
            ui.vertical(|ui| curve(app, ui, (h - 40.0).min(420.0)));
        });
    } else {
        let h = (ui.available_height() - 250.0).max(200.0);
        preview_pictures(app, ui, h);
        ui.add_space(4.0);
        curve(app, ui, 160.0);
    }
}

/// Step 5's picture in the mode chosen (playing, before/after or frame by frame), `h` high, with
/// its slider below.
fn preview_pictures(app: &mut App, ui: &mut egui::Ui, h: f32) {
    let t = app.lang.t();
    let lang = app.lang;
    let Some(p) = &mut app.project else { return };
    let v = p.video_version;
    let n = p.frames().max(1);
    let offsets = p.offsets();
    match p.mode {
        PreviewMode::Play => {
            let cursor = app.project.as_ref().map_or(0, |p| p.cursor);
            app.previews.want_play(Kind::Video(v), &offsets, n, cursor);
            let Some(p) = &mut app.project else { return };
            if let Some(started) = p.playing {
                let f = p.play_from + (started.elapsed().as_secs_f64() * f64::from(p.fps)) as usize;
                p.cursor = f % n;
                ui.ctx().request_repaint();
            }
            let key = Key {
                frame: p.cursor,
                kind: Kind::Video(v),
                edge: SMALL,
            };
            let tex = app
                .previews
                .get_or_near(key, offsets.get(p.cursor).copied().unwrap_or(0.0), None);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
            let uv = tex
                .as_ref()
                .map(|t| p.view_uv(p.cursor, t.size().map(|v| v as f32)));
            draw_view(ui, rect, tex.as_ref(), uv.unwrap_or([0.0, 0.0, 1.0, 1.0]), 0.0);
            let Some(p) = &mut app.project else { return };
            ui.horizontal(|ui| {
                let label = if p.playing.is_some() {
                    format!("⏸  {}", t.pause)
                } else {
                    format!("▶  {}", t.play)
                };
                if ui.add(theme::primary(label)).clicked() {
                    p.playing = match p.playing {
                        Some(_) => None,
                        None => {
                            p.play_from = p.cursor;
                            Some(std::time::Instant::now())
                        }
                    };
                }
                ui.label(theme::muted(format!(
                    "{} {} / {n}  ·  {}: {}",
                    t.frame_word,
                    p.cursor + 1,
                    t.length_word,
                    p.video_seconds_text(lang)
                )));
            });
        }
        PreviewMode::Compare => {
            let cursor = p.cursor;
            let before = app.previews.get_or_near(
                Key {
                    frame: cursor,
                    kind: Kind::Shot,
                    edge: LARGE,
                },
                0.0,
                None,
            );
            let after = app.previews.get_or_near(
                Key {
                    frame: cursor,
                    kind: Kind::Video(v),
                    edge: LARGE,
                },
                offsets.get(cursor).copied().unwrap_or(0.0),
                None,
            );
            let Some(p) = &mut app.project else { return };
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click_and_drag());
            let size = before
                .as_ref()
                .or(after.as_ref())
                .map(|t| t.size().map(|v| v as f32));
            let uv = size.map_or([0.0, 0.0, 1.0, 1.0], |s| p.view_uv(p.cursor, s));
            let shown = draw_view(ui, rect, before.as_ref(), uv, 0.0);
            if let Some(pos) = response.interact_pointer_pos() {
                p.split = ((pos.x - shown.left()) / shown.width()).clamp(0.0, 1.0);
            }
            if let Some(tex) = after.as_ref() {
                let x = shown.left() + shown.width() * p.split;
                draw_view(ui, rect, Some(tex), uv, p.split.max(1e-4));
                ui.painter().line_segment(
                    [Pos2::new(x, shown.top()), Pos2::new(x, shown.bottom())],
                    Stroke::new(2.0, theme::TEXT),
                );
                ui.painter()
                    .circle_filled(Pos2::new(x, shown.center().y), 14.0, theme::TEXT);
                ui.painter().text(
                    Pos2::new(x, shown.center().y),
                    Align2::CENTER_CENTER,
                    "⇔",
                    FontId::proportional(15.0),
                    theme::BG,
                );
                label_tag(ui, shown.left_top() + Vec2::splat(10.0), t.before);
                label_tag(ui, Pos2::new(shown.right() - 80.0, shown.top() + 10.0), t.after);
            }
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            frame_slider(ui, &mut p.cursor, n);
        }
        PreviewMode::Frames => {
            viewer_kind(app, ui, Kind::Video(v), h);
            if let Some(p) = &mut app.project {
                let c = p.cursor;
                let found =
                    p.quality.as_ref().is_some_and(|q| q.outliers.contains(&c)) && p.finish.replace_outliers;
                let mut marked = p.finish.replace_marked.contains(&c) || found;
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!found, egui::Checkbox::new(&mut marked, t.replace_this))
                        .changed()
                    {
                        if marked {
                            p.finish.replace_marked.push(c);
                        } else {
                            p.finish.replace_marked.retain(|&f| f != c);
                        }
                    }
                    if marked {
                        ui.label(RichText::new(t.replaced).color(theme::WARN));
                    }
                });
            }
        }
    }
}

fn viewer_kind(app: &mut App, ui: &mut egui::Ui, kind: Kind, h: f32) {
    let Some(p) = &mut app.project else { return };
    let n = p.frames().max(1);
    let offset = p.offsets().get(p.cursor).copied().unwrap_or(0.0);
    let tex = app.previews.get_or_near(
        Key {
            frame: p.cursor,
            kind,
            edge: LARGE,
        },
        offset,
        None,
    );
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
    match (kind, tex.as_ref()) {
        (Kind::Video(_), Some(t)) => {
            draw_view(
                ui,
                rect,
                Some(t),
                p.view_uv(p.cursor, t.size().map(|v| v as f32)),
                0.0,
            );
        }
        _ => {
            draw_picture(ui, rect, tex.as_ref(), false);
        }
    }
    frame_slider(ui, &mut p.cursor, n);
}

fn label_tag(ui: &egui::Ui, at: Pos2, text: &str) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(12.5), theme::TEXT);
    let r = Rect::from_min_size(at, galley.size() + Vec2::new(16.0, 8.0));
    ui.painter()
        .rect_filled(r, CornerRadius::same(6), Color32::from_black_alpha(160));
    ui.painter()
        .galley(r.min + Vec2::new(8.0, 4.0), galley, theme::TEXT);
}

// ---------------------------------------------------------------------------------------------
// Step 6: export

fn export(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    let lang = app.lang;
    let exporting = app.task.as_ref().is_some_and(|(k, _)| *k == TaskKind::Export);
    let advanced = app.open_advanced;
    let has_cli = app.cli.is_some();
    let app_queue_empty = app.queue.jobs.is_empty();
    let mut queue = false;
    let mut verify = false;
    let app_idle = app.task.is_none();
    let mut clicks = QueueClicks::default();
    let status = task_status(app);
    // The queue renders into this very target now: nothing else may write there meanwhile.
    // The queue renders into this very target now, or another render (a terminal, another
    // computer) does: nothing else may write there meanwhile.
    let queued = app
        .project
        .as_ref()
        .is_some_and(|p| app.queue.has_target(&p.target()) || p.render_there().is_some_and(|r| r.busy));
    let Some(p) = &mut app.project else { return };
    let mut start = false;
    let mut reveal = None;
    // A solid bar, always shown: in a low window "Advanced" (HDR, format, the frames' name) goes
    // on below the fold, and that is seen.
    ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .show(ui, |ui| {
            ui.set_max_width(780.0);
            ui.label(theme::title(t.format));
            if p.sequence {
                // Information, not a problem: grey.
                ui.label(RichText::new(t.sequence_note).color(theme::MUTED));
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let names = if p.sequence { &t.presets_frames } else { &t.presets };
                for (i, &(name, detail)) in names.iter().enumerate().take(PRESETS.len()) {
                    // For upright photos the upright preset would be a second upright 4K: it
                    // gives the other shape.
                    let (name, detail) = if p.preset_turned(i) {
                        t.preset_landscape
                    } else {
                        (name, detail)
                    };
                    // What it delivers: upright for upright photos (all but the upright preset).
                    let (w, h) = p.preset_size(i);
                    let selected = p.preset == i;
                    let frame = egui::Frame::new()
                        .fill(if selected {
                            theme::ACCENT_DIM
                        } else {
                            theme::SURFACE
                        })
                        .stroke(Stroke::new(
                            1.5,
                            if selected { theme::ACCENT } else { theme::LINE },
                        ))
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(egui::Margin::same(14));
                    let r = frame.show(ui, |ui| {
                        ui.set_width(170.0);
                        ui.set_height(78.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 3.0;
                            ui.label(RichText::new(name).strong());
                            ui.label(theme::small(format!("{w} × {h}")));
                            ui.label(theme::small(detail));
                        });
                    });
                    if ui
                        .interact(r.response.rect, ui.scope_id().with(("preset", i)), Sense::click())
                        .clicked()
                        && !exporting
                    {
                        p.preset = i;
                        p.preset_chosen = true;
                    }
                }
            });
            if p.frames_upright() == Some(true) && p.preset != 3 {
                // Information, not a problem: grey.
                ui.label(RichText::new(t.upright_photos).color(theme::MUTED));
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.label(t.fps);
                let ntsc = p.ntsc;
                for r in RATES {
                    // With the NTSC variant on, its rate as it is (29,97, not 30).
                    let label = match r {
                        24 | 30 | 60 if ntsc => crate::text::num(
                            lang,
                            f64::from(r) * 1000.0 / 1001.0,
                            if r == 24 { 3 } else { 2 },
                        ),
                        _ => r.to_string(),
                    };
                    ui.selectable_value(&mut p.fps, r, label);
                }
                ui.add_space(12.0);
                ui.label(theme::muted(format!(
                    "{}: {}",
                    t.length_word,
                    p.video_seconds_text(lang)
                )));
            });
            if p.video_seconds() < 5.0 {
                ui.label(theme::small(
                    t.short_video_tip
                        .replace(
                            "{fps}",
                            &if p.ntsc && matches!(p.fps, 24 | 30 | 60) {
                                crate::text::num(lang, f64::from(p.fps) * 1000.0 / 1001.0, 2)
                            } else {
                                p.fps.to_string()
                            },
                        )
                        .replace("{n}", &(p.fps * 10).to_string()),
                ));
            }
            // Photos smaller than the video (or its zoom) needs: softer than the size suggests.
            let read = p
                .seq
                .as_ref()
                .and_then(|s| crate::work::try_lock(s).and_then(|s| s.source_size(0)));
            let source = {
                let mut seen = crate::work::lock(&p.source_seen);
                if read.is_some() {
                    *seen = read;
                }
                *seen
            };
            if let Some(source) = source {
                let ((w, h), ..) = p.format();
                let zoom = p.camera.framing().map_or(1.0, |m| m.max_zoom());
                let f = lapsewerk_studio::upscale(source, (w, h), zoom);
                if f > 1.05 {
                    ui.label(
                        RichText::new(
                            t.upscale_warning
                                .replace("{size}", &format!("{} × {}", source.0, source.1))
                                .replace("{f}", &crate::text::num(lang, f, 1))
                                .replace(
                                    "{hint}",
                                    &format!(
                                        "{}{}",
                                        if w.min(h) <= 1080 {
                                            t.upscale_hint_smallest
                                        } else {
                                            t.upscale_hint
                                        },
                                        // Only when the camera move zooms in (step 4).
                                        if zoom > 1.01 { t.upscale_zoom_hint } else { "" }
                                    ),
                                ),
                        )
                        .color(theme::WARN),
                    );
                }
            }
            egui::CollapsingHeader::new(RichText::new(t.advanced).strong())
                .default_open(advanced)
                .show(ui, |ui| {
                    // Size, codec, rate and transfer beyond the presets.
                    egui::Grid::new("format")
                        .num_columns(2)
                        .spacing([14.0, 8.0])
                        .show(ui, |ui| {
                            ui.label(t.size_word);
                            ui.horizontal_wrapped(|ui| {
                                ui.selectable_value(&mut p.size, None, t.as_preset);
                                for (name, size) in crate::app::SIZES {
                                    ui.selectable_value(&mut p.size, Some(size), name)
                                        .on_hover_text(format!("{} × {}", size.0, size.1));
                                }
                            });
                            ui.end_row();
                            // A frame sequence has no codec: the row is left out.
                            if !p.sequence {
                                ui.label("Codec");
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    ui.selectable_value(&mut p.codec, None, t.as_preset);
                                    for c in crate::app::CODECS {
                                        ui.selectable_value(&mut p.codec, Some(c), codec_label(c));
                                    }
                                });
                                ui.end_row();
                            }
                            ui.label(t.fps);
                            ui.add_enabled(
                                matches!(p.fps, 24 | 30 | 60),
                                egui::Checkbox::new(&mut p.ntsc, t.ntsc),
                            )
                            .on_disabled_hover_text(t.ntsc_only);
                            ui.end_row();
                            ui.label(t.transfer_word);
                            let (_, _, _, transfer) = p.format();
                            let mut rec709 = transfer == lapsewerk_video::Transfer::Rec709;
                            if ui
                                .add_enabled(!p.hdr, egui::Checkbox::new(&mut rec709, t.rec709))
                                .on_hover_text(t.rec709_tip)
                                .changed()
                            {
                                p.rec709 = Some(rec709);
                            }
                            ui.end_row();
                            // Where an edit or a broadcast master needs it: the first frame's timecode.
                            if p.timecode_fits() {
                                ui.label("Timecode");
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut p.timecode)
                                            .hint_text(t.timecode_empty)
                                            .desired_width(200.0),
                                    )
                                    .on_hover_text(t.timecode_tip);
                                    let ok = p.timecode_ok();
                                    if p.timecode.contains(';') && !ok {
                                        // Drop-frame where it exists, but a frame number it skips.
                                        let df_here = p.ntsc && matches!(p.fps, 30 | 60);
                                        let why = if df_here {
                                            t.timecode_drop_skipped
                                        } else {
                                            t.timecode_drop
                                        };
                                        ui.add(egui::Label::new(RichText::new(why).color(theme::BAD)).wrap());
                                    } else if !ok {
                                        // A frame number at or over the rate: said so, with the rate.
                                        let base = p.fps;
                                        let last = p
                                            .timecode
                                            .trim()
                                            .rsplit(':')
                                            .next()
                                            .and_then(|f| f.parse::<u32>().ok());
                                        let text = match last {
                                            Some(f) if f >= base => {
                                                t.timecode_frame.replace("{n}", &base.to_string())
                                            }
                                            _ => t.timecode_bad.to_owned(),
                                        };
                                        ui.label(RichText::new(text).color(theme::BAD));
                                    } else if p.timecode_srgb() {
                                        // A master for an edit or a broadcast: Rec. 709 expected.
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(t.timecode_srgb).color(theme::WARN),
                                            )
                                            .wrap(),
                                        );
                                    }
                                });
                                ui.end_row();
                            }
                        });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        toggle(ui, &mut p.hdr);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(t.hdr).strong());
                            ui.label(theme::small(t.hdr_detail));
                            if p.hdr {
                                ui.horizontal(|ui| {
                                    ui.selectable_value(&mut p.hlg, false, t.hdr_pq);
                                    ui.selectable_value(&mut p.hlg, true, t.hdr_hlg);
                                });
                                ui.label(theme::small(if p.hlg {
                                    t.hdr_hlg_detail
                                } else {
                                    t.hdr_pq_detail
                                }));
                            }
                        });
                    });
                    ui.add_space(6.0);
                    // The same render from the command line (overnight, on another machine).
                    let line = format!("lapsewerk render \"{}\" --from-project", p.folder.display());
                    ui.horizontal(|ui| {
                        ui.label(theme::small(t.cli_line));
                        if ui
                            .small_button(t.copy_line)
                            .on_hover_text(p.cli_args().join(" "))
                            .clicked()
                        {
                            ui.ctx().copy_text(line.clone());
                        }
                    });
                    ui.label(RichText::new(&line).monospace().size(11.5).color(theme::MUTED));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        toggle(ui, &mut p.sequence);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(t.sequence_out).strong());
                            ui.label(theme::small(t.sequence_detail));
                        });
                    });
                    if p.sequence {
                        use lapsewerk_video::frames::FrameFormat;
                        ui.horizontal(|ui| {
                            ui.label(t.frame_format);
                            for (f, name) in [
                                (FrameFormat::Tiff, "TIFF 16 Bit"),
                                (FrameFormat::Dpx, "DPX 10 Bit"),
                                (FrameFormat::Exr, "OpenEXR"),
                            ] {
                                ui.selectable_value(&mut p.frame_format, f, name);
                            }
                        });
                        ui.label(theme::small(match p.frame_format {
                            FrameFormat::Tiff => t.tiff_detail,
                            FrameFormat::Dpx => t.dpx_detail,
                            FrameFormat::Exr if p.hdr => t.exr_detail,
                            FrameFormat::Exr => t.exr_detail_sdr,
                        }));
                        // The frames' name (a shot name for the edit), else as they are named now.
                        let now = lapsewerk_video::frames::frame_stem(&p.target());
                        ui.horizontal(|ui| {
                            ui.label(t.frame_name);
                            ui.add(
                                egui::TextEdit::singleline(&mut p.frame_name)
                                    .hint_text(now.as_str())
                                    .desired_width(180.0),
                            )
                            .on_hover_text(t.frame_name_tip);
                            let typed = p.frame_name.trim();
                            if !typed.is_empty() {
                                match lapsewerk_video::frames::clean_name(typed) {
                                    Ok(n) => {
                                        ui.label(theme::small(format!(
                                            "{n}_000001.{}",
                                            p.frame_format.ext()
                                        )));
                                    }
                                    Err(_) => {
                                        ui.label(RichText::new(t.frame_name_bad).color(theme::BAD));
                                    }
                                }
                            }
                        });
                    }
                });
            ui.add_space(12.0);
            theme::card().show(ui, |ui| {
                // One width whatever it holds (a long resume line widened it by 100 px).
                ui.set_width(ui.available_width().min(756.0));
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(theme::muted(t.save_as));
                        // A frame sequence is a folder named like the movie.
                        let name = p
                            .target()
                            .file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                        let name = if p.sequence {
                            format!(
                                "{name}/  ({})",
                                t.sequence_word.replace(
                                    "{fmt}",
                                    match p.frame_format {
                                        lapsewerk_video::frames::FrameFormat::Tiff => {
                                            if lang == crate::text::Lang::De {
                                                "16-Bit-TIFF"
                                            } else {
                                                "16-bit TIFF"
                                            }
                                        }
                                        lapsewerk_video::frames::FrameFormat::Dpx => "DPX",
                                        lapsewerk_video::frames::FrameFormat::Exr => "OpenEXR",
                                    }
                                )
                            )
                        } else {
                            name
                        };
                        ui.label(RichText::new(name).strong().size(16.0));
                        ui.label(theme::small(
                            p.out
                                .parent()
                                .map_or_else(String::new, |d| d.display().to_string()),
                        ));
                        // How much room it takes, against what is free there.
                        let need = p.space_needed();
                        if let Some(free) = lapsewerk_video::space::free_bytes(&p.target()) {
                            // The frames there all done: nothing more to write (they are kept or
                            // written over), said so rather than "0 MB".
                            let line = if p.name_clash().is_some() || p.bad_frame_name() {
                                // Nothing is written there while the names differ.
                                t.space_free.replace("{free}", &crate::text::bytes(lang, free))
                            } else if need < 1_000_000 && p.target().is_dir() {
                                t.space_none.replace("{free}", &crate::text::bytes(lang, free))
                            } else {
                                t.space_line
                                    .replace("{need}", &crate::text::bytes(lang, need))
                                    .replace("{free}", &crate::text::bytes(lang, free))
                            };
                            ui.label(if free < need.saturating_add(need / 50) {
                                RichText::new(line).color(theme::WARN)
                            } else {
                                theme::small(line)
                            });
                        }
                        // Another format's frames there: said before they are replaced.
                        if let Some((f, n)) = p.frames_there().first().filter(|(f, _)| *f != p.frame_format) {
                            ui.label(
                                RichText::new(
                                    t.replaces_other
                                        .replace("{n}", &n.to_string())
                                        .replace("{old}", f.name()),
                                )
                                .color(theme::WARN),
                            );
                        }
                        // What is there of this render: finished, stopped, or made otherwise.
                        // Among the photos: refused, said before the click.
                        if let Err(e) = lapsewerk_studio::check_target(&p.files, &p.target(), p.sequence) {
                            ui.label(
                                RichText::new(crate::text::reason(lang, &e))
                                    .color(theme::WARN)
                                    .size(13.0),
                            );
                        }
                        let renamed = if p.sequence && !queued && !exporting {
                            lapsewerk_video::frames::renamed_frames(&p.target())
                        } else {
                            None
                        };
                        if queued || exporting {
                            // Being written now (said below the box): nothing about it yet.
                        } else if let Some(other) = renamed {
                            ui.label(
                                RichText::new(t.target_renamed.replace("{other}", &other))
                                    .color(theme::WARN)
                                    .size(13.0),
                            );
                        } else if let Some(r) = p.render_there() {
                            let n = r.frames.to_string();
                            let who = holder_text(t, r.holder.as_ref());
                            let clash = p.name_clash();
                            // Checked (changed bits, gone) and cut short with whole frames after.
                            let mut damaged = p.verified_damaged();
                            for d in &r.damaged {
                                if !damaged.contains(d) {
                                    damaged.push(d.clone());
                                }
                            }
                            damaged.sort();
                            let (text, color) = match (r.same, r.complete) {
                                _ if r.busy && r.frames == 0 => (t.target_busy_measuring, theme::ACCENT),
                                _ if r.busy => (t.target_busy, theme::ACCENT),
                                _ if r.foreign => (t.target_foreign, theme::WARN),
                                // A name typed that frames can't take: no export is this one, whatever
                                // lies there (said first, not "already finished").
                                _ if p.bad_frame_name() => (t.frame_name_refused, theme::WARN),
                                // Damaged since: a delivery that ended, or a render that stopped.
                                (Some(true), _) if !damaged.is_empty() && p.can_resume() && r.ended => {
                                    (t.target_damaged, theme::WARN)
                                }
                                (Some(true), _) if !damaged.is_empty() && p.can_resume() => {
                                    (t.target_damaged_stopped, theme::WARN)
                                }
                                (Some(true), true) => (t.target_done, theme::GOOD),
                                (Some(true), false) if p.can_resume() => (t.target_stopped, theme::TEXT),
                                (Some(true), false) => (t.target_stopped_anew, theme::TEXT),
                                _ if clash.is_some() => (t.target_name_clash, theme::WARN),
                                // What differs is said below: no list of guesses here.
                                (Some(false), _) if r.why_other.is_some() => {
                                    (t.target_other_short, theme::WARN)
                                }
                                (Some(false), _) => (t.target_other, theme::WARN),
                                (None, true) => (t.target_done_unknown, theme::MUTED),
                                (None, false) => (t.target_stopped_unknown, theme::TEXT),
                            };
                            ui.label(
                                RichText::new(
                                    text.replace("{n}", &n)
                                        .replace("{there}", clash.as_ref().map_or("", |c| c.0.as_str()))
                                        .replace("{here}", clash.as_ref().map_or("", |c| c.1.as_str()))
                                        .replace("{who}", &who)
                                        .replace("{bad}", &p.damage_words(t, &damaged))
                                        .replace("{names}", &{
                                            // The first five, and how many more (not cut off silently).
                                            let shown = damaged
                                                .iter()
                                                .take(5)
                                                .cloned()
                                                .collect::<Vec<_>>()
                                                .join(", ");
                                            let more = damaged.len().saturating_sub(5);
                                            if more > 0 {
                                                format!(
                                                    "{shown} {}",
                                                    t.and_more.replace("{n}", &more.to_string())
                                                )
                                            } else {
                                                shown
                                            }
                                        }),
                                )
                                .color(color)
                                .size(13.0),
                            );
                            // Finished here: only whether every frame is there and whole is seen
                            // without reading them; checking reads each against its checksum.
                            // A stopped one too: checked as far as it goes (the sums noted as written).
                            if (r.complete || r.frames > 0)
                                && r.same == Some(true)
                                && p.verified.is_none()
                                && !r.busy
                            {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(theme::small(t.target_done_check));
                                    if ui
                                        .add_enabled(app_idle, theme::secondary(t.verify_button))
                                        .on_hover_text(t.verify_tip)
                                        .clicked()
                                    {
                                        verify = true;
                                    }
                                });
                            }
                            // What differs, as the command line says it ("timecode: there …, here …").
                            if let Some(why) = r
                                .why_other
                                .as_ref()
                                .filter(|_| r.same == Some(false) && !r.foreign && clash.is_none())
                            {
                                ui.label(
                                    RichText::new(
                                        t.target_why
                                            .replace("{why}", &crate::text::differs_words(app.lang, why)),
                                    )
                                    .color(theme::WARN)
                                    .size(12.5),
                                );
                            }
                            // Named: what is there besides this render.
                            let list = |names: &[String]| {
                                let mut l = names.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
                                if names.len() > 5 {
                                    l.push_str(&format!(" … (+{})", names.len() - 5));
                                }
                                l
                            };
                            if !r.beyond.is_empty() {
                                ui.label(
                                    RichText::new(t.target_beyond.replace("{names}", &list(&r.beyond)))
                                        .color(theme::WARN)
                                        .size(13.0),
                                );
                            }
                            if !r.strangers.is_empty() {
                                ui.label(
                                    RichText::new(t.target_strangers.replace("{names}", &list(&r.strangers)))
                                        .color(theme::WARN)
                                        .size(13.0),
                                );
                            }
                        }
                        // Chosen elsewhere and not there (a drive not plugged in).
                        if let Some(d) = p
                            .out
                            .parent()
                            .filter(|d| !d.is_dir() && !d.starts_with(&p.folder))
                        {
                            ui.label(
                                RichText::new(t.target_missing.replace("{folder}", &d.display().to_string()))
                                    .color(theme::WARN),
                            );
                        }
                    });
                    ui.add_space(12.0);
                    // A frame sequence goes into a folder: the folder it is made in is chosen.
                    if p.sequence
                        && ui.add_enabled(!exporting, theme::secondary(t.change)).clicked()
                        && let Some(dir) = rfd::FileDialog::new().set_directory(&p.folder).pick_folder()
                    {
                        let name = p
                            .out
                            .file_name()
                            .map(std::ffi::OsStr::to_os_string)
                            .unwrap_or_default();
                        p.out = dir.join(name);
                    } else if !p.sequence
                        && ui.add_enabled(!exporting, theme::secondary(t.change)).clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .set_directory(&p.folder)
                            .set_file_name(
                                p.out
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                            )
                            .add_filter("QuickTime", &["mov"])
                            .save_file()
                    {
                        p.out = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mov")) {
                            path
                        } else {
                            path.with_extension("mov")
                        };
                    }
                });
            });
            ui.add_space(14.0);
            if exporting {
                waiting(ui, t, status, p.sequence);
            } else if let Some(done) = p.finished_export().filter(|_| !queued) {
                theme::card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let frames = done.is_dir();
                    // Checked and found damaged: not "done" in green any more.
                    let hurt = !p.verified_damaged().is_empty();
                    ui.label(
                        RichText::new(if hurt {
                            t.export_done_damaged
                        } else if frames {
                            t.export_done_frames
                        } else {
                            t.export_done
                        })
                        .color(if hurt { theme::WARN } else { theme::GOOD })
                        .size(18.0)
                        .strong(),
                    );
                    ui.label(theme::muted(done.display().to_string()));
                    if !p.skipped_names.is_empty() {
                        ui.label(
                            RichText::new(
                                t.export_left_out
                                    .replace("{names}", &crate::text::names_list(lang, &p.skipped_names)),
                            )
                            .color(theme::WARN),
                        );
                    }
                    // A movie: what it delivers, as the command line says it (no INFO.txt).
                    if !frames && let Some(d) = &p.delivered {
                        egui::CollapsingHeader::new(theme::small(t.delivery_movie))
                            .id_salt("movie-delivery")
                            .default_open(false)
                            .show(ui, |ui| {
                                ui.label(RichText::new(d).monospace().size(11.5));
                            });
                    }
                    if frames {
                        ui.label(theme::small(t.frames_to_video));
                        // What was delivered (files, rate, timecode, curve): the INFO.txt beside
                        // the frames, for the edit.
                        // Folded: technical, for whoever edits the frames.
                        if let Ok(info) =
                            lapsewerk_video::frames::read_text(done.join(lapsewerk_video::frames::INFO_FILE))
                        {
                            egui::CollapsingHeader::new(theme::small(t.delivery_details))
                                .id_salt("delivery-details")
                                .default_open(false)
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(info.lines().skip(1).collect::<Vec<_>>().join("\n"))
                                            .monospace()
                                            .size(11.5),
                                    );
                                });
                        }
                        // HDR: the light levels the delivery needs, measured over all frames.
                        if let Ok(hdr) =
                            lapsewerk_video::frames::read_text(done.join(lapsewerk_video::frames::HDR10_FILE))
                        {
                            ui.label(
                                RichText::new(hdr.trim().replace('\n', " · "))
                                    .monospace()
                                    .size(12.0),
                            );
                        }
                    }
                    ui.horizontal(|ui| {
                        let show = if hurt {
                            theme::secondary(t.show_in_folder)
                        } else {
                            theme::primary(t.show_in_folder)
                        };
                        if ui.add(show).clicked() {
                            reveal = Some(done.clone());
                        }
                        // A frame sequence checked against its checksums (a copy, a drive, a while).
                        if p.sequence
                            && ui
                                .add_enabled(app_idle, theme::secondary(t.verify_button))
                                .on_hover_text(t.verify_tip)
                                .clicked()
                        {
                            verify = true;
                        }
                        // Checked: frames changed or gone are made again, the rest kept.
                        if p.sequence
                            && p.can_resume()
                            && !p.verified_damaged().is_empty()
                            && ui.add_enabled(app_idle, theme::primary(t.resume)).clicked()
                        {
                            p.exported = None;
                            p.resume_next = true;
                            start = true;
                        }
                        // Anew: asked first what to do with what is there.
                        if ui.add(theme::secondary(t.export_again)).clicked() {
                            p.exported = None;
                            p.ask_overwrite = true;
                        }
                    });
                    // Free software: a quiet way to say thanks, once the film is there.
                    if !hurt {
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(theme::small(t.coffee_line));
                            ui.hyperlink_to(
                                RichText::new(t.coffee_link).color(theme::ACCENT).size(12.5),
                                crate::text::COFFEE_URL,
                            );
                        });
                    }
                    if let Some((ok, bad)) = &p.verified {
                        if bad.is_empty() {
                            ui.label(
                                RichText::new(t.verify_ok.replace("{n}", &ok.to_string())).color(theme::GOOD),
                            );
                        } else {
                            let names = p.verified_damaged();
                            let damaged = names.len();
                            let stopped = p.render_there().is_some_and(|r| !r.ended);
                            let text = if damaged > 0
                                && p.can_resume()
                                && p.render_there().is_none_or(|r| r.same != Some(false))
                            {
                                if stopped {
                                    t.footer_damaged_stopped
                                } else {
                                    t.footer_damaged
                                }
                                .replace("{bad}", &p.damage_words(t, &names))
                            } else {
                                verify_bad_text(
                                    t,
                                    bad,
                                    p.render_there().is_some_and(|r| r.same == Some(false)),
                                )
                            };
                            ui.label(RichText::new(text).color(theme::BAD));
                            for b in bad.iter().take(8) {
                                ui.label(theme::small(verify_line(t, b)));
                            }
                        }
                    }
                });
            } else {
                let exists = p.target().exists();
                if queued {
                    // The queue's, or another render's (a terminal, another computer).
                    match p
                        .render_there()
                        .filter(|r| r.busy && !app.queue.runs_target(&p.target()))
                    {
                        Some(r) => {
                            let text = if r.frames == 0 {
                                t.target_busy_measuring.to_owned()
                            } else {
                                t.target_busy.replace("{n}", &r.frames.to_string())
                            };
                            ui.label(
                                RichText::new(text.replace("{who}", &holder_text(t, r.holder.as_ref())))
                                    .color(theme::ACCENT),
                            );
                            // Queued to come after it: the queue waits, then goes on with its frames.
                            if !app.queue.has_target(&p.target())
                                && ui
                                    .add_enabled(
                                        has_cli,
                                        // With other settings than the frames there, it replaces them
                                        // once the other render ends (not going on with them).
                                        theme::secondary(if r.same == Some(false) {
                                            t.queue_after_replace
                                        } else {
                                            t.queue_after
                                        }),
                                    )
                                    .on_hover_text(t.queue_hint)
                                    .on_disabled_hover_text(t.queue_no_cli)
                                    .clicked()
                            {
                                queue = true;
                            }
                        }
                        None => {
                            ui.label(theme::muted(t.target_queued));
                        }
                    }
                } else if p.ask_overwrite {
                    // What is there: frames of this format (go on, or anew), or of another one
                    // (only to be replaced).
                    let there = p.frames_there();
                    let other = there.first().filter(|(f, _)| *f != p.frame_format).copied();
                    let found = p.render_there();
                    let damaged = !p.verified_damaged().is_empty();
                    let done = found
                        .as_ref()
                        .is_some_and(|r| r.complete && r.same == Some(true) && !damaged);
                    let can_go_on = other.is_none()
                        && p.can_resume()
                        && found
                            .as_ref()
                            .is_some_and(|r| (!r.complete || damaged) && r.same != Some(false));
                    let question = match (p.sequence, there.first(), other) {
                        (true, _, None) if done => t.frames_done_question.to_owned(),
                        (true, _, Some((f, n))) => t
                            .frames_other_question
                            .replace("{n}", &n.to_string())
                            .replace("{old}", f.name())
                            .replace("{new}", p.frame_format.name()),
                        (true, Some((_, n)), None) => t.frames_there_count.replace("{n}", &n.to_string()),
                        (true, None, None) => t.frames_there_question.to_owned(),
                        (false, _, _) => t.overwrite_question.to_owned(),
                    };
                    ui.label(RichText::new(question).color(theme::WARN));
                    ui.horizontal(|ui| {
                        // A stopped frame sequence goes on where it stopped (same settings and
                        // format).
                        if p.sequence && can_go_on && ui.add(theme::primary(t.resume)).clicked() {
                            p.ask_overwrite = false;
                            p.resume_next = true;
                            start = true;
                        }
                        let replace = if p.sequence { t.replace_frames } else { t.export };
                        if ui.add(theme::secondary(replace)).clicked() {
                            p.ask_overwrite = false;
                            start = true;
                        }
                        if ui.add(theme::secondary(t.cancel)).clicked() {
                            p.ask_overwrite = false;
                        }
                    });
                } else {
                    let goes_on = p.goes_on();
                    ui.horizontal(|ui| {
                        // Refused for room: the estimate is an upper bound, so it may be tried.
                        if p.space_refused && ui.add(theme::secondary(t.space_anyway)).clicked() {
                            p.space_anyway = true;
                            start = true;
                        }
                        // A stopped render with these settings: going on is what is wanted.
                        if goes_on
                            && ui
                                .add(theme::primary(RichText::new(t.resume).size(17.0)))
                                .clicked()
                        {
                            p.resume_next = true;
                            start = true;
                        }
                        let main =
                            RichText::new(if p.sequence { t.export_frames } else { t.export }).size(17.0);
                        let main = if goes_on {
                            theme::secondary(main)
                        } else {
                            theme::primary(main)
                        };
                        let clash = p.name_clash().is_some() || p.bad_frame_name();
                        if ui
                            .add_enabled(!clash, main)
                            .on_disabled_hover_text(t.name_clash_tip)
                            .clicked()
                        {
                            if exists {
                                p.ask_overwrite = true;
                            } else {
                                start = true;
                            }
                        }
                        // What the queued job does with what is there, as said under "Save as".
                        let there = p.frames_there();
                        let foreign = p.render_there().is_some_and(|r| r.foreign);
                        let label = match (exists, p.sequence, there.first()) {
                            (false, _, _) => t.queue_add.to_owned(),
                            // Refused (another name): nothing there is replaced.
                            _ if clash => t.queue_add.to_owned(),
                            (true, true, _) if goes_on => t.queue_add_resume.to_owned(),
                            // Nothing to replace (an empty folder), or files that are not ours.
                            (true, true, None) => t.queue_add.to_owned(),
                            (true, true, Some(_)) if foreign => t.queue_add.to_owned(),
                            (true, true, Some((f, n))) => t
                                .queue_add_frames
                                .replace("{n}", &n.to_string())
                                .replace("{fmt}", f.name()),
                            (true, _, _) => t.queue_add_replace.to_owned(),
                        };
                        if ui
                            .add_enabled(has_cli && !clash, theme::secondary(label))
                            .on_hover_text(t.queue_hint)
                            .on_disabled_hover_text(if clash { t.name_clash_tip } else { t.queue_no_cli })
                            .clicked()
                        {
                            queue = true;
                        }
                    });
                }
            }
            if !app_queue_empty {
                ui.add_space(16.0);
                clicks = queue_ui(&app.queue, app.lang, ui);
            }
        });
    if start {
        app.start_export(false);
    }
    if queue {
        app.queue_export();
    }
    if verify {
        app.start_verify();
    }
    clicks.apply(&mut app.queue);
    if let Some(path) = reveal {
        App::reveal(&path);
    }
}

/// A queued render's step (as the command line names it), in words.
fn queue_step(t: &Texts, step: &str) -> &'static str {
    match step {
        "measured" => t.queue_step_measure,
        "checked" => t.queue_step_check,
        "verified" => t.queue_step_verify,
        "dust" => t.queue_step_dust,
        "rendered" => t.queue_step_render,
        _ => t.queue_step_start,
    }
}

/// What was clicked in the queue card.
#[derive(Default)]
pub struct QueueClicks {
    stop: bool,
    remove: Option<usize>,
    clear: bool,
    /// A job to run again: its index, and whether to replace what is there.
    again: Option<(usize, bool)>,
    /// A job paused since the app last ran, to start in its turn.
    start: Option<usize>,
}

impl QueueClicks {
    pub fn apply(self, queue: &mut crate::queue::Queue) {
        if self.stop {
            queue.stop();
        }
        if let Some(i) = self.remove {
            queue.remove(i);
        }
        if let Some((i, replace)) = self.again {
            queue.requeue(i, replace);
        }
        if let Some(i) = self.start {
            queue.resume_paused(i);
        }
        if self.clear {
            queue.clear_finished();
        }
    }
}

/// The render queue (start page): what runs, what waits, what ended; stop, remove, clear.
pub fn queue_card(app: &mut App, ui: &mut egui::Ui) {
    let clicks = queue_ui(&app.queue, app.lang, ui);
    clicks.apply(&mut app.queue);
}

/// The queue card; what was clicked is applied by the caller.
pub fn queue_ui(queue: &crate::queue::Queue, lang: crate::text::Lang, ui: &mut egui::Ui) -> QueueClicks {
    let t = lang.t();
    use crate::queue::State;
    let mut stop = false;
    let mut remove = None;
    let mut clear = false;
    let mut again = None;
    let mut start = None;
    theme::card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(theme::title(t.queue_title));
            if queue
                .jobs
                .iter()
                .any(|j| !matches!(j.state, State::Waiting | State::Running(..) | State::Paused))
                && ui.small_button(t.queue_clear).clicked()
            {
                clear = true;
            }
        });
        ui.label(theme::small(t.queue_hint));
        ui.add_space(6.0);
        for (i, j) in queue.jobs.iter().enumerate() {
            // Name and target on one line, state and buttons on the next: at a normal window
            // width a long path left no room for the reason and the buttons.
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&j.name).strong());
                // Where it goes, from the photo folder on (the name alone repeated the project's).
                // A long path (a drive's) by its end, so it never pushes the row out of view.
                let path = j.out.strip_prefix(&j.folder).unwrap_or(&j.out).display().to_string();
                let count = path.chars().count();
                let shown = if count > 56 {
                    format!("…{}", path.chars().skip(count - 55).collect::<String>())
                } else {
                    path
                };
                ui.label(theme::muted(format!("→ {shown}")))
                    .on_hover_text(j.out.display().to_string());
            });
            ui.horizontal_wrapped(|ui| {
                match &j.state {
                    State::Waiting => {
                        // Waiting for another render in its folder, or only its turn.
                        ui.label(theme::muted(if queue.held(i) {
                            t.queue_held
                        } else {
                            t.queue_waiting
                        }));
                        if ui.small_button(t.queue_remove).clicked() {
                            remove = Some(i);
                        }
                    }
                    State::Paused => {
                        // Waiting when the app was closed: started only when asked.
                        ui.label(RichText::new(t.queue_paused).color(theme::WARN));
                        if ui.small_button(t.queue_start).clicked() {
                            start = Some(i);
                        }
                        if ui.small_button(t.queue_remove).clicked() {
                            remove = Some(i);
                        }
                    }
                    State::Running(step, f) => {
                        // The words beside the bar (inside a narrow one they were cut off).
                        let what = queue_step(t, step);
                        ui.add(egui::ProgressBar::new(*f).desired_width(120.0));
                        ui.label(theme::small(format!("{what} {:.0} %", f * 100.0)));
                        if ui.small_button(t.cancel).clicked() {
                            stop = true;
                        }
                    }
                    State::Done => {
                        ui.label(RichText::new(t.queue_finished).color(theme::GOOD));
                        if let Ok(hdr) =
                            lapsewerk_video::frames::read_text(j.out.join(lapsewerk_video::frames::HDR10_FILE))
                        {
                            ui.label(theme::small(
                                hdr.lines().skip(1).take(2).collect::<Vec<_>>().join(" · "),
                            ));
                        }
                        if ui.small_button(t.show_in_folder).clicked() {
                            App::reveal(&j.out);
                        }
                    }
                    State::Stopped | State::Failed(_) => {
                        if let State::Failed(e) = &j.state {
                            ui.label(RichText::new(t.queue_error).color(theme::BAD))
                                .on_hover_text(e);
                            ui.label(theme::small(crate::text::reason(lang, e)));
                        } else {
                            ui.label(RichText::new(t.queue_stopped).color(theme::WARN));
                        }
                        // Frames there: go on with them (not when they are of another format);
                        // or start again, replacing them.
                        let other_format = matches!(&j.state, State::Failed(e) if e.contains("another file format") || e.contains("frames already"));
                        if j.out.is_dir() && !other_format && ui.small_button(t.queue_resume).clicked() {
                            again = Some((i, false));
                        }
                        if ui.small_button(t.queue_replace).clicked() {
                            again = Some((i, true));
                        }
                        if ui.small_button(t.queue_remove).clicked() {
                            remove = Some(i);
                        }
                    }
                }
            });
            ui.add_space(4.0);
        }
    });
    QueueClicks {
        stop,
        remove,
        clear,
        again,
        start,
    }
}

/// Who writes into a folder, in the app's words: "Prozess 4242 auf mac-studio, gestartet 22:09".
pub(crate) fn holder_text(t: &Texts, h: Option<&lapsewerk_video::frames::LockHolder>) -> String {
    let Some(h) = h else {
        return String::from("?");
    };
    let on = h
        .computer
        .as_deref()
        .map_or_else(|| t.lock_here.to_owned(), |n| t.lock_on.replace("{name}", n));
    let when = |s: Option<i64>| {
        s.and_then(|s| chrono::DateTime::from_timestamp(s, 0)).map(|d| {
            let d = d.with_timezone(&chrono::Local);
            if d.date_naive() == chrono::Local::now().date_naive() {
                d.format("%H:%M").to_string()
            } else {
                d.format("%d.%m. %H:%M").to_string()
            }
        })
    };
    match when(h.since) {
        Some(since) => {
            let mut text = t
                .lock_who
                .replace("{pid}", &h.pid.to_string())
                .replace("{on}", &on)
                .replace("{since}", &since);
            // Its last sign of life (renewed every few minutes): a render on another computer
            // that stopped answering shows here before its lock counts as left over.
            if let Some(seen) = when(h.seen) {
                text.push_str(&t.lock_seen.replace("{seen}", &seen));
            }
            text
        }
        None => t
            .lock_who
            .split(',')
            .next()
            .unwrap_or("")
            .replace("{pid}", &h.pid.to_string())
            .replace("{on}", &on),
    }
}

/// A line of `lapsewerk verify` ("name: changed") in the app's words.
/// What "Check delivery" found wrong, the files named (the first five), with what to do: go on
/// for this render's frames, make them anew for frames of `other` settings.
pub fn verify_bad_text(t: &Texts, bad: &[String], other: bool) -> String {
    let mut names: Vec<&str> = bad
        .iter()
        .take(5)
        .map(|b| b.rsplit_once(": ").map_or(b.as_str(), |(n, _)| n))
        .collect();
    if bad.len() > 5 {
        names.push("…");
    }
    if other { t.verify_bad_other } else { t.verify_bad }
        .replace("{n}", &bad.len().to_string())
        .replace("{names}", &names.join(", "))
}

fn verify_line(t: &Texts, line: &str) -> String {
    let Some((name, why)) = line.rsplit_once(": ") else {
        return line.to_owned();
    };
    let text = match why {
        "changed" => t.verify_changed,
        w if w.starts_with("cut short") => t.verify_cut,
        "missing" => t.verify_missing,
        w if w == lapsewerk_video::frames::EXTRA => t.verify_extra,
        _ => return line.to_owned(),
    };
    text.replace("{name}", name)
}

/// "Keyframes changed: 23 of 60 frames measured again (29-52)" in the app's words.
fn again_text(t: &Texts, line: &str) -> String {
    // "…; edited: keyframe(s) 1, 300" at the end: named after what is measured.
    let (line, which) = line.split_once("; edited: ").unwrap_or((line, ""));
    // Without what is said after them in brackets (read-only ones).
    let which = which.split(" (").next().unwrap_or_default();
    let list: Vec<&str> = which
        .split(|c: char| !c.is_ascii_digit())
        .filter(|w| !w.is_empty())
        .collect();
    let text = again_count_text(t, line);
    if list.is_empty() {
        text
    } else {
        format!(
            "{text} {}",
            t.measure_again_which.replace("{list}", &list.join(", "))
        )
    }
}

/// [`again_text`] without the edited keyframes.
fn again_count_text(t: &Texts, line: &str) -> String {
    let numbers: Vec<usize> = line
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|w| w.parse().ok())
        .collect();
    if line.contains("in exposure only") {
        return t.measure_again_exposure.to_owned();
    }
    // The photos themselves changed (retouched, replaced), or the measuring.
    if line.contains("photos changed") {
        return match numbers.as_slice() {
            [count, first, last, again, n, ..] => t
                .measure_again_photos_some
                .replace("{count}", &count.to_string())
                .replace("{range}", &format!("{first}–{last}"))
                .replace("{again}", &again.to_string())
                .replace("{n}", &n.to_string()),
            [count, first, last, ..] => t
                .measure_again_photos
                .replace("{count}", &count.to_string())
                .replace("{range}", &format!("{first}–{last}")),
            _ => t.measure_again_setup.to_owned(),
        };
    }
    if !line.contains("keyframes changed") {
        return t.measure_again_setup.to_owned();
    }
    if line.contains("nothing measured again") {
        return t.measure_again_none.to_owned();
    }
    if line.contains("all ") {
        return t.measure_again_all.to_owned();
    }
    match numbers.as_slice() {
        [count, n, first, last, ..] => t
            .measure_again
            .replace("{count}", &count.to_string())
            .replace("{n}", &n.to_string())
            .replace("{range}", &format!("{first}–{last}")),
        _ => t.measure_again_all.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measuring_again_names_the_edited_keyframe() {
        let line = "analysis:      keyframes changed since the last analysis: 297 of 300 frames measured again (2-299), the others kept; edited: keyframe 1";
        assert_eq!(
            again_text(&crate::text::DE, line),
            "Keyframes wurden geändert: 297 von 300 Bildern werden nachgemessen (Bild 2–299), die übrigen bleiben. Bearbeitet: Keyframe 1."
        );
        let photos = "analysis:      photos changed since the last analysis: 3 (frames 101-103): all frames measured again";
        assert!(
            again_text(&crate::text::DE, photos).contains("101–103"),
            "{}",
            again_text(&crate::text::DE, photos)
        );
        let some = "analysis:      photos changed since the last analysis: 3 (frames 101-103): 5 of 300 frames measured again (100-104), the others kept";
        assert!(
            again_text(&crate::text::DE, some).contains("5 von 300"),
            "{}",
            again_text(&crate::text::DE, some)
        );
        let none = "analysis:      keyframes changed since the last analysis, only in what is not measured (tone curve, contrast, shadows, highlights): nothing measured again; edited: keyframes 1, 300";
        assert!(again_text(&crate::text::EN, none).ends_with("Edited: keyframe 1, 300."));
    }

    #[test]
    fn a_delivery_checked_is_told_in_the_apps_words() {
        let de = &crate::text::DE;
        assert_eq!(
            verify_line(de, "A_000020.exr: changed"),
            "A_000020.exr: geändert (passt nicht mehr zu seiner Prüfsumme)"
        );
        assert_eq!(verify_line(de, "A_000025.exr: missing"), "A_000025.exr: fehlt");
        let extra = format!("A_000040.exr: {}", lapsewerk_video::frames::EXTRA);
        assert!(verify_line(de, &extra).contains("zusätzliches Bild"));
    }
}
