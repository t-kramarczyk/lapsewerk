//! Help for beginners: a short introduction on the first start (four slides, again from the start
//! page), and a help panel for the step one is in (the "?" at the top, or F1).

use egui::{RichText, Vec2};

use crate::app::App;
use crate::theme;

/// The introduction, while it is open.
pub fn intro(app: &mut App, ctx: &egui::Context) {
    let Some(slide) = app.intro else { return };
    let t = app.lang.t();
    let last = t.intro.len().saturating_sub(1);
    let slide = slide.min(last);
    let (title, body) = t.intro.get(slide).copied().unwrap_or(("", ""));
    let mut go = None;
    let mut close = false;
    // A dark backdrop: the start page must not show through the introduction.
    egui::Modal::new(egui::Id::unique("intro"))
        .backdrop_color(egui::Color32::from_black_alpha(225))
        .show(ctx, |ui| {
            ui.set_width(520.0);
            ui.horizontal(|ui| {
                crate::start::logo(ui, 28.0);
                ui.label(RichText::new(title).size(22.0).strong());
            });
            ui.add_space(10.0);
            ui.label(RichText::new(body).size(15.0));
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                // Where one is: a dot a slide.
                for k in 0..=last {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), egui::Sense::hover());
                    let c = if k == slide { theme::ACCENT } else { theme::LINE };
                    ui.painter().circle_filled(r.center(), 4.0, c);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if slide == last {
                        if ui.add(theme::primary(t.intro_go)).clicked() {
                            close = true;
                        }
                    } else if ui.add(theme::primary(t.intro_next)).clicked() {
                        go = Some(slide + 1);
                    }
                    if slide > 0 && ui.add(theme::secondary(t.intro_back)).clicked() {
                        go = Some(slide - 1);
                    }
                    if slide < last && ui.add(egui::Button::new(t.intro_skip).frame(false)).clicked() {
                        close = true;
                    }
                });
            });
        });
    if close {
        app.close_intro();
    } else if let Some(s) = go {
        app.intro = Some(s);
    }
}

/// The help for `step` (1..6), with tips for shooting, in a side panel.
pub fn panel(app: &mut App, ui: &mut egui::Ui, step: usize) {
    let t = app.lang.t();
    ui.horizontal(|ui| {
        ui.label(theme::title(format!(
            "{} {step}: {}",
            if app.lang == crate::text::Lang::De {
                "Schritt"
            } else {
                "Step"
            },
            t.steps.get(step.saturating_sub(1)).copied().unwrap_or("")
        )));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("×").clicked() {
                app.help = false;
            }
        });
    });
    ui.add_space(6.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
        for para in t
            .help_steps
            .get(step.saturating_sub(1))
            .copied()
            .unwrap_or("")
            .split("\n\n")
        {
            ui.label(RichText::new(para).size(14.0));
            ui.add_space(8.0);
        }
        ui.separator();
        ui.label(RichText::new(t.help_shooting_title).strong());
        ui.label(theme::small(t.help_shooting));
        ui.add_space(10.0);
        if ui.link(t.intro_again).clicked() {
            app.intro = Some(0);
        }
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.label(theme::small(t.coffee_line));
            ui.hyperlink_to(
                RichText::new(t.coffee_link).color(theme::ACCENT).size(12.5),
                crate::text::COFFEE_URL,
            );
        });
    });
}
