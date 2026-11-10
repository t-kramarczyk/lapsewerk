//! Masks of the keyframe being edited: the list with its own sliders (in the controls), and the
//! selected mask drawn on the picture with handles to drag (a gradient's start and end, an
//! ellipse's centre and size).

use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use lapsewerk_develop::DevelopSettings;
use lapsewerk_develop::masks::{self, Kind, Shape};

use crate::text::{Lang, Texts};
use crate::theme;

/// The brush: radius (fraction of the long edge), feather 0..100, painting or erasing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrushTool {
    pub size: f64,
    pub feather: f64,
    pub erase: bool,
}

impl Default for BrushTool {
    fn default() -> Self {
        Self {
            size: 0.04,
            feather: 50.0,
            erase: false,
        }
    }
}

/// What the user did with the masks.
pub enum Action {
    /// A finished stroke on brush mask `.0`, in frame coordinates (0..1).
    Stroke(usize, Vec<(f64, f64)>),
    /// Every stroke of brush mask `.0` taken off.
    ClearStrokes(usize),
    /// The brush changed.
    Tool(BrushTool),
    Add(Kind),
    Remove(usize),
    Invert(usize),
    Select(Option<usize>),
    /// A mask value, `mask.<i>.<field>`.
    Set(String, f64),
}

#[must_use]
pub fn kind_name(kind: Kind, t: &Texts) -> &'static str {
    match kind {
        Kind::Linear => t.mask_linear,
        Kind::Radial => t.mask_radial,
        Kind::Sky => t.mask_sky,
        Kind::Luminance => t.mask_luminance,
        Kind::Brush => t.mask_brush,
    }
}

fn adjustment_name(field: &str, lang: Lang) -> &'static str {
    let de = lang == Lang::De;
    match field {
        "exposure" => {
            if de {
                "Belichtung"
            } else {
                "Exposure"
            }
        }
        "temp" => {
            if de {
                "Temperatur"
            } else {
                "Temperature"
            }
        }
        "tint" => {
            if de {
                "Tönung"
            } else {
                "Tint"
            }
        }
        "contrast" => {
            if de {
                "Kontrast"
            } else {
                "Contrast"
            }
        }
        "highlights" => {
            if de {
                "Lichter"
            } else {
                "Highlights"
            }
        }
        "shadows" => {
            if de {
                "Tiefen"
            } else {
                "Shadows"
            }
        }
        "whites" => {
            if de {
                "Weiß"
            } else {
                "Whites"
            }
        }
        "blacks" => {
            if de {
                "Schwarz"
            } else {
                "Blacks"
            }
        }
        "texture" => {
            if de {
                "Struktur"
            } else {
                "Texture"
            }
        }
        "clarity" => {
            if de {
                "Klarheit"
            } else {
                "Clarity"
            }
        }
        "dehaze" => {
            if de {
                "Dunst entfernen"
            } else {
                "Dehaze"
            }
        }
        _ => {
            if de {
                "Sättigung"
            } else {
                "Saturation"
            }
        }
    }
}

/// One slider row: name, slider, value; pushes a change.
#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut egui::Ui,
    lang: Lang,
    label_w: f32,
    name: &str,
    id: String,
    value: f64,
    range: (f64, f64),
    decimals: usize,
    scale: f64,
    out: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(label_w, 26.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_width(label_w);
                ui.add(egui::Label::new(theme::muted(name)).truncate());
            },
        );
        let mut v = value * scale;
        let r = ui.add(theme::localized(
            egui::Slider::new(&mut v, range.0 * scale..=range.1 * scale),
            lang,
            decimals,
        ));
        if r.changed() && (v - value * scale).abs() > 1e-9 {
            out.push(Action::Set(id, v / scale));
        }
    });
}

/// The masks section of the controls.
pub fn panel(
    ui: &mut egui::Ui,
    lang: Lang,
    d: &DevelopSettings,
    selected: Option<usize>,
    tool: BrushTool,
    label_w: f32,
    out: &mut Vec<Action>,
) {
    let t = lang.t();
    let title = if d.masks.is_empty() {
        t.masks.to_owned()
    } else {
        format!("{}  ({})", t.masks, d.masks.len())
    };
    egui::CollapsingHeader::new(RichText::new(title).strong())
        .id_salt("masks")
        .default_open(true)
        .show(ui, |ui| {
            ui.label(theme::small(t.masks_hint));
            ui.horizontal_wrapped(|ui| {
                for kind in [
                    Kind::Linear,
                    Kind::Radial,
                    Kind::Sky,
                    Kind::Luminance,
                    Kind::Brush,
                ] {
                    if ui
                        .add(egui::Button::new(format!("+ {}", kind_name(kind, t))).small())
                        .on_hover_text(match kind {
                            Kind::Linear => t.mask_linear_tip,
                            Kind::Radial => t.mask_radial_tip,
                            Kind::Sky => t.mask_sky_tip,
                            Kind::Luminance => t.mask_luminance_tip,
                            Kind::Brush => t.mask_brush_tip,
                        })
                        .clicked()
                    {
                        out.push(Action::Add(kind));
                    }
                }
            });
            ui.add_space(4.0);
            for (i, m) in d.masks.iter().enumerate() {
                let known = Kind::of(m);
                ui.horizontal(|ui| {
                    let name = if m.name.is_empty() {
                        format!("{} {}", t.masks, i + 1)
                    } else {
                        m.name.clone()
                    };
                    if ui.selectable_label(selected == Some(i), name).clicked() {
                        out.push(Action::Select(if selected == Some(i) { None } else { Some(i) }));
                    }
                    if known.is_none() {
                        ui.label(theme::small(t.mask_from_lightcraft));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button(t.mask_delete)
                            .on_hover_text(t.mask_remove)
                            .clicked()
                        {
                            out.push(Action::Remove(i));
                        }
                        let mut inv = m.invert;
                        if ui.checkbox(&mut inv, t.mask_invert).changed() {
                            out.push(Action::Invert(i));
                        }
                    });
                });
            }
            let Some((i, m)) = selected.and_then(|i| d.masks.get(i).map(|m| (i, m))) else {
                return;
            };
            ui.separator();
            ui.spacing_mut().slider_width = (ui.available_width() - label_w - 100.0).max(60.0);
            let id = |f: &str| format!("{}{i}.{f}", masks::PREFIX);
            // The shape first, then what the mask does.
            match Kind::of(m) {
                Some(Kind::Linear) => {
                    ui.label(theme::small(t.mask_drag_linear));
                }
                Some(Kind::Radial) => {
                    ui.label(theme::small(t.mask_drag_radial));
                    for (f, name, hi, decimals) in [
                        ("rx", t.mask_width, 1.0, 2),
                        ("ry", t.mask_height, 1.0, 2),
                        ("feather", t.mask_feather, 100.0, 0),
                    ] {
                        let v = masks::get(m, f).unwrap_or(0.0);
                        row(ui, lang, label_w, name, id(f), v, (0.0, hi), decimals, 1.0, out);
                    }
                }
                Some(Kind::Luminance) => {
                    for (f, name) in [
                        ("lumLo", t.mask_lum_from),
                        ("lumHi", t.mask_lum_to),
                        ("lumLoFeather", t.mask_feather),
                    ] {
                        let v = masks::get(m, f).unwrap_or(0.0);
                        row(ui, lang, label_w, name, id(f), v, (0.0, 1.0), 0, 100.0, out);
                    }
                }
                Some(Kind::Sky) => {
                    ui.label(theme::small(t.mask_sky_tip));
                }
                Some(Kind::Brush) => {
                    ui.label(theme::small(t.mask_paint_hint));
                    let mut tl = tool;
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut tl.erase, false, t.brush_paint);
                        ui.selectable_value(&mut tl.erase, true, t.brush_erase);
                        if ui.small_button(t.brush_clear).clicked() {
                            out.push(Action::ClearStrokes(i));
                        }
                    });
                    let mut size = tl.size * 100.0;
                    ui.add(
                        theme::localized(egui::Slider::new(&mut size, 0.5..=25.0), lang, 1)
                            .text(t.brush_size),
                    );
                    tl.size = size / 100.0;
                    ui.add(
                        egui::Slider::new(&mut tl.feather, 0.0..=100.0)
                            .fixed_decimals(0)
                            .text(t.mask_feather),
                    );
                    if tl != tool {
                        out.push(Action::Tool(tl));
                    }
                }
                None => {}
            }
            for (f, lo, hi) in masks::ADJUSTMENTS {
                let v = masks::get(m, f).unwrap_or(0.0);
                let decimals = if *f == "exposure" { 2 } else { 0 };
                row(
                    ui,
                    lang,
                    label_w,
                    adjustment_name(f, lang),
                    id(f),
                    v,
                    (*lo, *hi),
                    decimals,
                    1.0,
                    out,
                );
            }
        });
}

/// Draws the selected mask on the picture (`shown`, the whole frame) and lets its handles be dragged.
pub fn overlay(
    ui: &mut egui::Ui,
    shown: Rect,
    d: &DevelopSettings,
    selected: Option<usize>,
    tool: BrushTool,
    painting: &mut Vec<(f64, f64)>,
    out: &mut Vec<Action>,
) {
    let Some((i, m)) = selected.and_then(|i| d.masks.get(i).map(|m| (i, m))) else {
        return;
    };
    let Some(shape) = m.components.first().map(|c| &c.shape) else {
        return;
    };
    let painter = ui.painter_at(shown);
    let to_screen = |x: f64, y: f64| {
        Pos2::new(
            shown.left() + shown.width() * x as f32,
            shown.top() + shown.height() * y as f32,
        )
    };
    let to_norm = |p: Pos2| {
        (
            f64::from((p.x - shown.left()) / shown.width().max(1.0)),
            f64::from((p.y - shown.top()) / shown.height().max(1.0)),
        )
    };
    let tint = Color32::from_rgba_unmultiplied(255, 160, 60, 50);
    let id = |f: &str| format!("{}{i}.{f}", masks::PREFIX);
    // A round handle at `at`; dragging it reports the new place.
    let handle = |ui: &mut egui::Ui, at: Pos2, key: &str| -> Option<(f64, f64)> {
        let r = Rect::from_center_size(at, Vec2::splat(18.0));
        let resp = ui.interact(r, ui.scope_id().with(("mask-handle", i, key)), Sense::drag());
        let p = ui.painter_at(shown);
        p.circle_filled(at, 7.0, theme::ACCENT);
        p.circle_stroke(at, 7.0, Stroke::new(1.5, Color32::WHITE));
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        resp.dragged()
            .then(|| resp.interact_pointer_pos().map(to_norm))
            .flatten()
    };
    match shape {
        Shape::Linear { start, end } => {
            let (a, b) = (to_screen(start.x, start.y), to_screen(end.x, end.y));
            // Shaded from the start (full effect) to the end (none), across the whole frame.
            let dir = b - a;
            let len = dir.length().max(1.0);
            let n = Vec2::new(-dir.y, dir.x) / len * (shown.width() + shown.height());
            let steps = 16;
            for k in 0..steps {
                let (s0, s1) = (k as f32 / steps as f32, (k + 1) as f32 / steps as f32);
                let (p0, p1) = (a + dir * s0, a + dir * s1);
                let alpha = (28.0 * (1.0 - s0)) as u8;
                painter.add(egui::Shape::convex_polygon(
                    vec![p0 - n, p0 + n, p1 + n, p1 - n],
                    Color32::from_rgba_unmultiplied(255, 160, 60, alpha),
                    Stroke::NONE,
                ));
            }
            for p in [a, b] {
                painter.line_segment(
                    [p - n.normalized() * 4000.0, p + n.normalized() * 4000.0],
                    Stroke::new(1.0, theme::ACCENT),
                );
            }
            painter.line_segment([a, b], Stroke::new(1.5, theme::ACCENT));
            if let Some((x, y)) = handle(ui, a, "start") {
                out.push(Action::Set(id("startX"), x));
                out.push(Action::Set(id("startY"), y));
            }
            if let Some((x, y)) = handle(ui, b, "end") {
                out.push(Action::Set(id("endX"), x));
                out.push(Action::Set(id("endY"), y));
            }
        }
        Shape::Radial { center, rx, ry, .. } => {
            // Radii are fractions of the long edge.
            let long = shown.width().max(shown.height());
            let c = to_screen(center.x, center.y);
            let (w, h) = (*rx as f32 * long, *ry as f32 * long);
            let points: Vec<Pos2> = (0..48)
                .map(|k| {
                    let a = k as f32 / 48.0 * std::f32::consts::TAU;
                    c + Vec2::new(w * a.cos(), h * a.sin())
                })
                .collect();
            painter.add(egui::Shape::convex_polygon(points.clone(), tint, Stroke::NONE));
            painter.add(egui::Shape::closed_line(points, Stroke::new(1.5, theme::ACCENT)));
            if let Some((x, y)) = handle(ui, c, "centre") {
                out.push(Action::Set(id("centerX"), x));
                out.push(Action::Set(id("centerY"), y));
            }
            if let Some((x, _)) = handle(ui, c + Vec2::new(w, 0.0), "rx") {
                let v = ((x - center.x) * f64::from(shown.width()) / f64::from(long)).abs();
                out.push(Action::Set(id("rx"), v.max(0.01)));
            }
            if let Some((_, y)) = handle(ui, c + Vec2::new(0.0, h), "ry") {
                let v = ((y - center.y) * f64::from(shown.height()) / f64::from(long)).abs();
                out.push(Action::Set(id("ry"), v.max(0.01)));
            }
        }
        Shape::Brush { .. } => {
            let long = shown.width().max(shown.height());
            let paint = Color32::from_rgba_unmultiplied(255, 160, 60, 70);
            let rubber = Color32::from_rgba_unmultiplied(40, 40, 46, 150);
            let draw = |points: &[(f64, f64)], size: f64, erase: bool| {
                let width = (2.0 * size as f32 * long).max(1.0);
                let pts: Vec<Pos2> = points.iter().map(|(x, y)| to_screen(*x, *y)).collect();
                let color = if erase { rubber } else { paint };
                if let [only] = pts.as_slice() {
                    painter.circle_filled(*only, width / 2.0, color);
                } else {
                    painter.add(egui::Shape::line(pts, Stroke::new(width, color)));
                }
            };
            for (points, size, erase) in masks::strokes(m) {
                draw(&points, size, erase);
            }
            // Painting: the stroke so far, and the brush under the pointer.
            let resp = ui.interact(shown, ui.scope_id().with(("brush", i)), Sense::drag());
            if let Some(p) = resp
                .interact_pointer_pos()
                .filter(|_| resp.dragged() || resp.drag_started())
            {
                let n = to_norm(p);
                let far = painting.last().is_none_or(|(x, y)| {
                    let (dx, dy) = (
                        (n.0 - x) * f64::from(shown.width()),
                        (n.1 - y) * f64::from(shown.height()),
                    );
                    dx.hypot(dy) > 1.5
                });
                if far && painting.len() < masks::MAX_STROKE_POINTS * 4 {
                    painting.push(n);
                }
            }
            if !painting.is_empty() {
                draw(painting, tool.size, tool.erase);
            }
            if resp.drag_stopped() && !painting.is_empty() {
                out.push(Action::Stroke(i, std::mem::take(painting)));
            }
            if let Some(p) = resp.hover_pos() {
                painter.circle_stroke(p, tool.size as f32 * long, Stroke::new(1.0, Color32::WHITE));
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
        }
        Shape::Sky | Shape::LuminanceRange { .. } => {
            let label = if matches!(shape, Shape::Sky) { "☁" } else { "◐" };
            painter.rect_filled(
                Rect::from_min_size(shown.min + Vec2::splat(8.0), Vec2::new(28.0, 24.0)),
                CornerRadius::same(6),
                Color32::from_black_alpha(150),
            );
            painter.text(
                shown.min + Vec2::new(22.0, 20.0),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(14.0),
                theme::ACCENT,
            );
        }
        _ => {}
    }
}
