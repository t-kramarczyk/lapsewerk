//! The look: dark (photos read true, nothing glares), calm surfaces, one warm accent (sunrise),
//! rounded corners, generous spacing, Inter.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Vec2};

pub const BG: Color32 = Color32::from_rgb(0x12, 0x13, 0x16);
pub const SURFACE: Color32 = Color32::from_rgb(0x1b, 0x1c, 0x21);
pub const SURFACE_HI: Color32 = Color32::from_rgb(0x25, 0x27, 0x2e);
pub const LINE: Color32 = Color32::from_rgb(0x2f, 0x31, 0x39);
pub const TEXT: Color32 = Color32::from_rgb(0xec, 0xed, 0xf0);
pub const MUTED: Color32 = Color32::from_rgb(0x9a, 0x9d, 0xa8);
pub const FAINT: Color32 = Color32::from_rgb(0x62, 0x65, 0x70);
pub const ACCENT: Color32 = Color32::from_rgb(0xff, 0x9f, 0x43);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x5a, 0x3a, 0x1c);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x1a, 0x10, 0x05);
pub const GOOD: Color32 = Color32::from_rgb(0x4c, 0xd1, 0x8a);
pub const WARN: Color32 = Color32::from_rgb(0xff, 0xc8, 0x57);
pub const BAD: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);
/// Curve colours: as shot, in the video, the scene.
pub const CURVE_SHOT: Color32 = Color32::from_rgb(0x7a, 0x7f, 0x8e);
pub const CURVE_VIDEO: Color32 = ACCENT;
pub const CURVE_SCENE: Color32 = Color32::from_rgb(0x5a, 0xa9, 0xff);

pub const RADIUS: u8 = 10;

/// Installs fonts (Inter, from LightCraft) and the style.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(lightcraft_ui_egui::theme::font_definitions(&[]));
    ctx.global_style_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(26.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(12.5, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(14.0, FontFamily::Monospace)),
        ]
        .into();
        let s = &mut style.spacing;
        s.item_spacing = Vec2::new(10.0, 10.0);
        s.button_padding = Vec2::new(16.0, 9.0);
        s.interact_size = Vec2::new(40.0, 34.0);
        s.slider_width = 220.0;
        s.window_margin = egui::Margin::same(16);
        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = BG;
        v.window_fill = SURFACE;
        v.extreme_bg_color = Color32::from_rgb(0x0c, 0x0d, 0x0f);
        v.faint_bg_color = SURFACE;
        v.override_text_color = Some(TEXT);
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = ACCENT_DIM;
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.slider_trailing_fill = true;
        v.handle_shape = egui::style::HandleShape::Circle;
        let r = CornerRadius::same(RADIUS);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = r;
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        v.widgets.inactive.weak_bg_fill = SURFACE_HI;
        // Check boxes and slider rails (buttons use the weak fill): lighter than a card, so an
        // empty box is seen at once.
        v.widgets.inactive.bg_fill = Color32::from_rgb(0x3d, 0x41, 0x4c);
        v.widgets.inactive.bg_stroke = Stroke::NONE;
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2e, 0x31, 0x3a);
        v.widgets.hovered.bg_fill = Color32::from_rgb(0x2e, 0x31, 0x3a);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x45, 0x48, 0x52));
        v.widgets.active.weak_bg_fill = Color32::from_rgb(0x36, 0x39, 0x43);
        v.widgets.active.bg_fill = ACCENT;
        v.window_corner_radius = CornerRadius::same(14);
        v.menu_corner_radius = CornerRadius::same(RADIUS);
        v.window_stroke = Stroke::new(1.0, LINE);
    });
}

/// A card: a raised surface with padding.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::same(18))
        .stroke(Stroke::new(1.0, LINE))
}

/// The main action of a page: filled with the accent.
pub fn primary(text: impl Into<egui::WidgetText>) -> egui::Button<'static> {
    egui::Button::new(text.into().color(ON_ACCENT).strong())
        .fill(ACCENT)
        .corner_radius(CornerRadius::same(RADIUS))
        .min_size(Vec2::new(0.0, 40.0))
}

/// A quiet button next to a primary one.
pub fn secondary(text: impl Into<egui::WidgetText>) -> egui::Button<'static> {
    egui::Button::new(text)
        .fill(SURFACE_HI)
        .corner_radius(CornerRadius::same(RADIUS))
        .min_size(Vec2::new(0.0, 40.0))
}

pub fn muted(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).color(MUTED)
}

pub fn small(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).color(MUTED).size(12.5)
}

pub fn title(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(17.0).strong()
}

/// A slider showing (and taking) numbers as the language writes them: a decimal comma in German.
pub fn localized(slider: egui::Slider<'_>, lang: crate::text::Lang, decimals: usize) -> egui::Slider<'_> {
    if lang != crate::text::Lang::De {
        return slider.fixed_decimals(decimals);
    }
    slider
        .custom_formatter(move |v, _| format!("{v:.decimals$}").replace('.', ","))
        .custom_parser(|t| t.trim().replace(',', ".").parse().ok())
}

/// [`localized`] for a drag value: a decimal comma in German.
pub fn localized_drag(
    v: egui::DragValue<'_>,
    lang: crate::text::Lang,
    decimals: usize,
) -> egui::DragValue<'_> {
    if lang != crate::text::Lang::De {
        return v.fixed_decimals(decimals);
    }
    v.custom_formatter(move |v, _| format!("{v:+.decimals$}").replace('.', ","))
        .custom_parser(|t| t.trim().replace(',', ".").parse().ok())
}
