use enforcer::BlockRule;

/// Settings / actions UI for a [`BlockRule`].
///
/// Only types that already implement [`BlockRule`] can implement this trait.
/// Identity (`id` / `name` / `description`) comes from [`BlockRule`].
pub trait RuleUi: BlockRule {
    /// Draw this rule's settings panel (shown when its tab is selected).
    fn settings_ui(&self, ui: &mut egui::Ui);

    /// Optional per-process actions in the shared process list (e.g. "+BL").
    fn process_row_ui(&self, ui: &mut egui::Ui, process_name: &str) {
        let _ = (ui, process_name);
    }
}
