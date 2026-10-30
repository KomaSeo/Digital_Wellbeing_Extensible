use enforcer::{BlockRule, ProcessSnapshot};

/// Settings UI for a [`BlockRule`].
///
/// Only types that already implement [`BlockRule`] can implement this trait.
/// Identity (`id` / `name` / `description`) comes from [`BlockRule`].
///
/// If a rule needs process-related controls, it draws them inside [`settings_ui`]
/// using the provided process list — there is no shared per-row hook.
pub trait RuleUi: BlockRule {
    /// Draw this rule's settings panel (shown when its tab is selected).
    fn settings_ui(&self, ui: &mut egui::Ui, processes: &[ProcessSnapshot]);
}

/// Add a system font with Korean glyphs as a fallback (egui's bundled fonts
/// have none, so Hangul would render as boxes). No-op if none is found.
pub fn install_system_fonts(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\malgun.ttf",
        "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
    ];
    let Some(bytes) = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok()) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "system-cjk".into(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("system-cjk".into());
    }
    ctx.set_fonts(fonts);
}

/// A tab in the desktop shell that is not a block rule (e.g. the task planner).
pub trait Panel: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn ui(&self, ui: &mut egui::Ui);
}
