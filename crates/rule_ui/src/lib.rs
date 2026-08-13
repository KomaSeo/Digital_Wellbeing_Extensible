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
