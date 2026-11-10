//! The project manager: start a new project, or pick up one of the last.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use egui::{Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, Vec2};
use lapsewerk_studio::Picture;

use crate::app::{App, STEPS};
use crate::recent::{self, Recent};
use crate::text::Lang;
use crate::theme;

/// Card pictures of the recent projects: from the settings folder, else made from the middle
/// frame of the folder on a background thread (one at a time) and kept there.
#[derive(Default)]
pub struct Thumbs {
    textures: HashMap<PathBuf, Option<egui::TextureHandle>>,
    making: Option<(PathBuf, Receiver<Option<Picture>>)>,
    wanted: Vec<PathBuf>,
}

impl Thumbs {
    #[must_use]
    pub fn busy(&self) -> bool {
        self.making.is_some() || !self.wanted.is_empty()
    }

    fn get(&mut self, folder: &Path) -> Option<&egui::TextureHandle> {
        if !self.textures.contains_key(folder) && !self.wanted.iter().any(|w| w == folder) {
            self.wanted.push(folder.to_path_buf());
        }
        self.textures.get(folder).and_then(Option::as_ref)
    }

    pub fn poll(&mut self, ctx: &egui::Context, recent: &Recent) {
        if let Some((folder, rx)) = &self.making
            && let Ok(pic) = rx.try_recv()
        {
            let tex = pic.map(|p| {
                if let Some(path) = recent.thumb_path(folder) {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let _ = image::save_buffer(
                        &path,
                        &p.rgba,
                        p.width as u32,
                        p.height as u32,
                        image::ExtendedColorType::Rgba8,
                    );
                }
                load(ctx, folder, &p)
            });
            self.textures.insert(folder.clone(), tex);
            self.making = None;
        }
        if self.making.is_some() {
            return;
        }
        let Some(folder) = self.wanted.pop() else { return };
        // Kept from before?
        if let Some(path) = recent.thumb_path(&folder)
            && let Ok(img) = image::open(&path)
        {
            let rgba = img.to_rgba8();
            let p = Picture {
                width: rgba.width() as usize,
                height: rgba.height() as usize,
                rgba: rgba.into_raw(),
            };
            self.textures.insert(folder.clone(), Some(load(ctx, &folder, &p)));
            return;
        }
        let (tx, rx) = channel();
        let f = folder.clone();
        let ctx2 = ctx.clone();
        let started = std::thread::Builder::new()
            .name("lapsewerk-thumb".into())
            .spawn(move || {
                let pic = make_thumb(&f);
                let _ = tx.send(pic);
                ctx2.request_repaint();
            });
        if started.is_ok() {
            self.making = Some((folder, rx));
        } else {
            self.textures.insert(folder, None);
        }
    }
}

fn load(ctx: &egui::Context, folder: &Path, p: &Picture) -> egui::TextureHandle {
    let image = egui::ColorImage::from_rgba_unmultiplied([p.width, p.height], &p.rgba);
    ctx.load_texture(
        format!("thumb-{}", folder.display()),
        image,
        egui::TextureOptions::LINEAR,
    )
}

/// The middle frame of the folder, small.
fn make_thumb(folder: &Path) -> Option<Picture> {
    let files = lapsewerk_studio::frame_files(folder).ok()?.files;
    let middle = files.get(files.len() / 2)?.clone();
    let mut seq = lapsewerk_develop::Sequence::open(&[middle]).ok()?;
    let plan = seq.plan(lapsewerk_core::keyframes::Interpolation::Smooth).ok()?;
    let job = lapsewerk_studio::preview_job(&mut seq, &plan, 0, 0.0, 420).ok()?;
    lapsewerk_studio::run_preview(job).ok()
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::symmetric(40, 32)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let width = ui.available_width().min(1040.0);
                    ui.vertical_centered(|ui| {
                        ui.set_max_width(width);
                        header(app, ui);
                        // A message (an error stays until closed) where it is seen: at the top.
                        let mut close = false;
                        if let Some(m) = &app.message {
                            ui.add_space(14.0);
                            theme::card().show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                // The close button first (right), the text wrapping in the room left.
                                ui.horizontal_top(|ui| {
                                    let text_width = (ui.available_width() - 40.0).max(80.0);
                                    ui.vertical(|ui| {
                                        ui.set_max_width(text_width);
                                        ui.add(
                                            egui::Label::new(RichText::new(&m.text).color(if m.error {
                                                theme::BAD
                                            } else {
                                                theme::GOOD
                                            }))
                                            .wrap(),
                                        );
                                    });
                                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                        if ui.small_button("×").clicked() {
                                            close = true;
                                        }
                                    });
                                });
                            });
                        }
                        if close {
                            app.message = None;
                        }
                        ui.add_space(26.0);
                        new_project(app, ui);
                        ui.add_space(12.0);
                        calibrate(app, ui);
                        if !app.queue.jobs.is_empty() {
                            ui.add_space(12.0);
                            crate::pages::queue_card(app, ui);
                        }
                        ui.add_space(24.0);
                        ui.horizontal(|ui| {
                            ui.label(theme::title(t.recent));
                        });
                        ui.add_space(6.0);
                        recent_grid(app, ui);
                        // The legal notices (GPL-3.0 §5d), at the foot of the start page.
                        ui.add_space(28.0);
                        ui.label(theme::small(
                            t.about.replace("{version}", env!("CARGO_PKG_VERSION")),
                        ));
                    });
                });
        });
}

fn header(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    ui.horizontal(|ui| {
        logo(ui, 34.0);
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.label(RichText::new("Lapsewerk").size(30.0).strong());
            ui.label(theme::muted(t.tagline));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            lang_switch(app, ui);
            ui.add_space(8.0);
            if ui.add(egui::Button::new(t.intro_again).frame(false)).clicked() {
                app.intro = Some(0);
            }
        });
    });
}

/// Making a colour profile of one's camera: a button, or the progress while it runs.
fn calibrate(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    let running = app
        .task
        .as_ref()
        .filter(|(k, _)| *k == crate::app::TaskKind::Calibrate)
        .map(|(_, task)| task.status());
    ui.horizontal(|ui| {
        match running {
            Some(s) => {
                let f = s.fraction().unwrap_or(0.0);
                ui.add(
                    egui::ProgressBar::new(f)
                        .desired_width(320.0)
                        .text(format!("{} {}/{}", t.calibrate, s.done, s.total)),
                );
                if ui.add(theme::secondary(t.cancel)).clicked()
                    && let Some((_, task)) = &app.task
                {
                    task.cancel();
                }
            }
            None => {
                if ui
                    .add_enabled(app.task.is_none(), theme::secondary(format!("{} …", t.calibrate)))
                    .on_hover_text(t.calibrate_tip)
                    .clicked()
                    && let Some(folders) = rfd::FileDialog::new().set_title(t.calibrate_tip).pick_folders()
                {
                    app.start_calibrate(folders);
                }
            }
        }
        ui.label(theme::small(t.calibrate_hint));
    });
}

/// The mark: a sun on the horizon, drawn.
pub fn logo(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same((size * 0.28) as u8), theme::ACCENT_DIM);
    let c = egui::pos2(rect.center().x, rect.bottom() - size * 0.32);
    p.circle_filled(c, size * 0.26, theme::ACCENT);
    p.rect_filled(
        egui::Rect::from_min_max(egui::pos2(rect.left(), c.y), rect.max),
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: (size * 0.28) as u8,
            se: (size * 0.28) as u8,
        },
        theme::ACCENT_DIM,
    );
    p.line_segment(
        [
            egui::pos2(rect.left() + size * 0.14, c.y),
            egui::pos2(rect.right() - size * 0.14, c.y),
        ],
        Stroke::new(2.0, theme::ACCENT),
    );
}

pub fn lang_switch(app: &mut App, ui: &mut egui::Ui) {
    for lang in [Lang::En, Lang::De] {
        let on = app.lang == lang;
        let label = RichText::new(lang.code().to_uppercase())
            .color(if on { theme::TEXT } else { theme::FAINT })
            .strong();
        if ui
            .add(
                egui::Button::new(label)
                    .fill(if on {
                        theme::SURFACE_HI
                    } else {
                        Color32::TRANSPARENT
                    })
                    .min_size(Vec2::new(36.0, 28.0)),
            )
            .clicked()
            && !on
        {
            app.set_lang(lang);
        }
    }
}

fn new_project(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    let hovering_files = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
    let frame = egui::Frame::new()
        .fill(if hovering_files {
            theme::ACCENT_DIM
        } else {
            theme::SURFACE
        })
        .corner_radius(CornerRadius::same(16))
        .stroke(Stroke::new(
            1.5,
            if hovering_files {
                theme::ACCENT
            } else {
                theme::LINE
            },
        ))
        .inner_margin(egui::Margin::same(26));
    let r = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (plus, _) = ui.allocate_exact_size(Vec2::splat(52.0), Sense::hover());
            ui.painter().circle_filled(plus.center(), 26.0, theme::ACCENT);
            ui.painter().text(
                plus.center(),
                egui::Align2::CENTER_CENTER,
                "+",
                egui::FontId::proportional(32.0),
                theme::ON_ACCENT,
            );
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(t.new_project).size(20.0).strong());
                ui.label(theme::muted(t.new_project_hint));
                ui.label(theme::small(t.drop_here));
            });
        });
    });
    let response = ui.interact(r.response.rect, ui.scope_id().with("new-project"), Sense::click());
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked()
        && let Some(folder) = rfd::FileDialog::new().set_title(t.new_project_hint).pick_folder()
    {
        app.open_folder(&folder);
    }
}

fn recent_grid(app: &mut App, ui: &mut egui::Ui) {
    let t = app.lang.t();
    if app.recent.entries.is_empty() {
        ui.add_space(20.0);
        ui.label(theme::muted(t.no_recent));
        return;
    }
    // Cards of about 260-320 px: at 1024 three side by side, the name under the picture in view.
    let cols = ((ui.available_width() + 16.0) / 272.0).floor().max(1.0) as usize;
    let card_w = (ui.available_width() - 16.0 * (cols as f32 - 1.0)) / cols as f32;
    let entries = app.recent.entries.clone();
    let mut open = None;
    let mut remove = None;
    for row in entries.chunks(cols) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            for e in row {
                let missing = !e.folder.is_dir();
                let thumb = if missing {
                    None
                } else {
                    app.thumbs.get(&e.folder).cloned()
                };
                let (clicked, removed) = card(ui, card_w, e, thumb.as_ref(), missing, t);
                if clicked && !missing {
                    open = Some(e.folder.clone());
                }
                if removed {
                    remove = Some(e.folder.clone());
                }
            }
        });
        ui.add_space(16.0);
    }
    if let Some(f) = remove {
        app.recent.remove(&f);
        app.recent.save();
    }
    if let Some(f) = open {
        app.open_folder(&f);
    }
}

/// A project card; returns (clicked, remove clicked).
fn card(
    ui: &mut egui::Ui,
    width: f32,
    e: &recent::Entry,
    thumb: Option<&egui::TextureHandle>,
    missing: bool,
    t: &crate::text::Texts,
) -> (bool, bool) {
    let pic_h = width * 9.0 / 16.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, pic_h + 86.0), Sense::click());
    let hovered = response.hovered() && !missing;
    let p = ui.painter_at(rect);
    p.rect(
        rect,
        CornerRadius::same(14),
        if hovered {
            theme::SURFACE_HI
        } else {
            theme::SURFACE
        },
        Stroke::new(1.0, if hovered { theme::ACCENT } else { theme::LINE }),
        egui::StrokeKind::Inside,
    );
    let pic = egui::Rect::from_min_size(rect.min + Vec2::splat(1.0), Vec2::new(width - 2.0, pic_h));
    let radius = CornerRadius {
        nw: 13,
        ne: 13,
        sw: 0,
        se: 0,
    };
    match thumb {
        Some(tex) => {
            // Cover the area, cropping the long side.
            let [tw, th] = tex.size().map(|v| v as f32);
            let (sx, sy) = (pic.width() / tw.max(1.0), pic.height() / th.max(1.0));
            let s = sx.max(sy);
            let (u, v) = (
                (pic.width() / (tw * s)).min(1.0),
                (pic.height() / (th * s)).min(1.0),
            );
            let uv = egui::Rect::from_center_size(egui::pos2(0.5, 0.5), Vec2::new(u, v));
            egui::Image::new((tex.id(), pic.size()))
                .uv(uv)
                .corner_radius(radius)
                .paint_at(ui, pic);
        }
        None => {
            p.rect_filled(pic, radius, theme::SURFACE_HI);
            let label = if missing { t.folder_missing } else { "…" };
            p.text(
                pic.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(14.0),
                theme::FAINT,
            );
        }
    }
    let mut y = pic.bottom() + 12.0;
    let x = rect.left() + 14.0;
    p.text(
        egui::pos2(x, y),
        egui::Align2::LEFT_TOP,
        &e.name,
        egui::FontId::proportional(16.0),
        theme::TEXT,
    );
    y += 24.0;
    let meta = format!(
        "{} {} · {} {} / {} · {}",
        e.frames,
        t.frames,
        t.step_word,
        e.step,
        STEPS,
        recent::ago(e.opened, &t.opened_ago)
    );
    p.text(
        egui::pos2(x, y),
        egui::Align2::LEFT_TOP,
        meta,
        egui::FontId::proportional(12.5),
        theme::MUTED,
    );
    y += 20.0;
    let path = e.folder.display().to_string();
    // About 6 px a character at this size: the end of the path, as much as the card holds.
    let fits = (((width - 28.0) / 6.0).max(12.0)) as usize;
    let path = if path.chars().count() > fits {
        format!(
            "…{}",
            path.chars()
                .rev()
                .take(fits - 2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<String>()
        )
    } else {
        path
    };
    p.text(
        egui::pos2(x, y),
        egui::Align2::LEFT_TOP,
        path,
        egui::FontId::proportional(11.5),
        theme::FAINT,
    );
    // Remove: a small × at the top right while hovered.
    let x_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 20.0, rect.top() + 20.0),
        Vec2::splat(26.0),
    );
    let mut removed = false;
    if response.hovered() || missing {
        let xr = ui
            .interact(x_rect, ui.scope_id().with(("remove", &e.folder)), Sense::click())
            .on_hover_text(t.remove_from_list);
        ui.painter()
            .circle_filled(x_rect.center(), 13.0, Color32::from_black_alpha(170));
        ui.painter().text(
            x_rect.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            egui::FontId::proportional(17.0),
            theme::TEXT,
        );
        removed = xr.clicked();
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    (response.clicked() && !removed, removed)
}
