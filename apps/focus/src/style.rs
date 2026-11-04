//! Responsibility: Keep the Focus app's look in one place: the theme (text sizes, spacing,
//! corners, colours) and the few shared pieces every tab draws with, so that a screen is
//! tidied by using them instead of styling each widget where it is drawn.
//!
//! - [`apply`]: the theme, once at start.
//! - [`section`]: a section title, with a line and room above it unless it opens the screen
//!   ([`section_with`]: the same with something beside the title, such as a button).
//! - [`notice`]: a tinted box for a message that must be seen (information, warning, error).
//! - [`primary`]: the one button a screen most wants pressed.

use eframe::egui::{self, Color32, CornerRadius, FontId, Margin, RichText, Stroke, TextStyle};

/// The accent colour of primary buttons and the selected tab.
pub const ACCENT: Color32 = Color32::from_rgb(58, 120, 200);
const SECTION_GAP: f32 = 14.0;

/// Responsibility: Name how strongly a [`notice`] asks to be read.
#[derive(Clone, Copy)]
pub enum Tone {
    Information,
    Warning,
    Error,
}

/// The theme: readable text sizes, more room between rows and inside buttons, round corners.
pub fn apply(context: &egui::Context) {
    context.style_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::proportional(20.0)),
            (TextStyle::Body, FontId::proportional(15.0)),
            (TextStyle::Button, FontId::proportional(15.0)),
            (TextStyle::Small, FontId::proportional(13.0)),
            (TextStyle::Monospace, FontId::monospace(14.0)),
        ]
        .into();
        let spacing = &mut style.spacing;
        spacing.item_spacing = egui::vec2(8.0, 7.0);
        spacing.button_padding = egui::vec2(10.0, 5.0);
        spacing.interact_size.y = 26.0;
        spacing.window_margin = Margin::same(12);
        let visuals = &mut style.visuals;
        let corners = CornerRadius::same(3);
        for widget in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.corner_radius = corners;
        }
        visuals.selection.bg_fill = ACCENT;
        visuals.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
        visuals.hyperlink_color = Color32::from_rgb(120, 170, 240);
    });
}

/// A section title, with a line and room above it unless it opens the screen.
pub fn section(ui: &mut egui::Ui, title: impl Into<String>) {
    section_with(ui, title, |_| {});
}

/// [`section`] with `beside` drawn on the title's line.
pub fn section_with(
    ui: &mut egui::Ui,
    title: impl Into<String>,
    beside: impl FnOnce(&mut egui::Ui),
) {
    if ui.min_rect().height() > 1.0 {
        ui.add_space(SECTION_GAP / 2.0);
        ui.separator();
        ui.add_space(SECTION_GAP / 2.0);
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new(title.into()).heading().strong());
        beside(ui);
    });
    ui.add_space(2.0);
}

/// A message in a tinted box the width of the screen.
pub fn notice(ui: &mut egui::Ui, tone: Tone, text: impl Into<String>) {
    let (text_colour, fill) = match tone {
        Tone::Information => (
            Color32::from_rgb(170, 200, 240),
            Color32::from_rgba_unmultiplied(58, 120, 200, 40),
        ),
        Tone::Warning => (
            Color32::from_rgb(235, 195, 90),
            Color32::from_rgba_unmultiplied(220, 170, 40, 36),
        ),
        Tone::Error => (
            Color32::from_rgb(240, 120, 120),
            Color32::from_rgba_unmultiplied(220, 60, 60, 40),
        ),
    };
    egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(text.into()).color(text_colour)).wrap());
        });
}

/// The one button a screen most wants pressed: filled with the accent colour while it can be
/// pressed (a plain button while it cannot, so it does not look pressable).
pub fn primary(text: impl Into<String>, enabled: bool) -> egui::Button<'static> {
    if enabled {
        egui::Button::new(RichText::new(text.into()).strong().color(Color32::WHITE)).fill(ACCENT)
    } else {
        egui::Button::new(text.into())
    }
}
