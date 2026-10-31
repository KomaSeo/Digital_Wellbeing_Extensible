//! Responsibility: Turn ListStore into a BlockRule and draw its settings tab.

use crate::{ListMode, ListStore};
use enforcer::{BlockEvent, BlockRule, ProcessSnapshot};
use parking_lot::Mutex;
use rule_ui::RuleUi;
use std::sync::Arc;

/// Responsibility: Keep the list tab's text inputs and status message between frames.
#[derive(Default)]
struct ListUiState {
    name_input: String,
    strategy_input: String,
    process_filter: String,
    status: String,
}

/// Responsibility: Block processes by the configured blacklist or whitelist, and draw the list
/// editor.
///
/// Blacklist / whitelist [`BlockRule`], backed by [`ListStore`].
pub struct ListBasedRule {
    store: ListStore,
    ui_state: Mutex<ListUiState>,
}

impl ListBasedRule {
    pub fn new(store: ListStore) -> Self {
        Self {
            store,
            ui_state: Mutex::new(ListUiState::default()),
        }
    }

    pub fn store(&self) -> &ListStore {
        &self.store
    }

    fn set_status(ui_state: &mut ListUiState, result: Result<(), String>) {
        match result {
            Ok(()) => ui_state.status.clear(),
            Err(message) => ui_state.status = message,
        }
    }
}

impl BlockRule for ListBasedRule {
    fn id(&self) -> &str {
        "list_based_rule"
    }

    fn name(&self) -> &str {
        "List-based rule"
    }

    fn description(&self) -> &str {
        "Blocks processes according to the configured blacklist or whitelist"
    }

    fn evaluate(&self, processes: &[ProcessSnapshot]) -> Vec<BlockEvent> {
        let config = self.store.get();
        let reason = match config.mode {
            ListMode::Blacklist => "blacklist",
            ListMode::Whitelist => "whitelist",
        };
        processes
            .iter()
            .filter(|process| config.should_block(&process.name))
            .map(|process| BlockEvent::new(process, reason))
            .collect()
    }
}

impl RuleUi for ListBasedRule {
    fn settings_ui(&self, ui: &mut egui::Ui, processes: &[ProcessSnapshot]) {
        let config = self.store.get();
        let mut state = self.ui_state.lock();

        ui.label(BlockRule::description(self));
        ui.separator();

        ui.horizontal(|ui| {
            ui.label("Mode:");
            let mut mode = config.mode;
            if ui
                .selectable_value(&mut mode, ListMode::Blacklist, "Blacklist")
                .clicked()
            {
                let _ = self.store.set_mode(ListMode::Blacklist);
            }
            if ui
                .selectable_value(&mut mode, ListMode::Whitelist, "Whitelist")
                .clicked()
            {
                let _ = self.store.set_mode(ListMode::Whitelist);
            }
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut state.name_input);
        });
        ui.horizontal(|ui| {
            if ui.button("Add blacklist").clicked() && !state.name_input.trim().is_empty() {
                let name = state.name_input.trim().to_string();
                let result = self
                    .store
                    .add_blacklist(&name)
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                Self::set_status(&mut state, result);
                state.name_input.clear();
            }
            if ui.button("Add whitelist").clicked() && !state.name_input.trim().is_empty() {
                let name = state.name_input.trim().to_string();
                let result = self
                    .store
                    .add_whitelist(&name)
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                Self::set_status(&mut state, result);
                state.name_input.clear();
            }
        });

        ui.separator();
        ui.label("Blacklist");
        egui::ScrollArea::vertical()
            .id_salt("list_based_rule_bl")
            .max_height(160.0)
            .show(ui, |ui| {
                for name in &config.blacklist {
                    ui.horizontal(|ui| {
                        ui.label(name);
                        if ui.small_button("✕").clicked() {
                            let _ = self.store.remove_blacklist(name);
                        }
                    });
                }
            });

        ui.separator();
        ui.label("Whitelist");
        egui::ScrollArea::vertical()
            .id_salt("list_based_rule_wl")
            .max_height(160.0)
            .show(ui, |ui| {
                for name in &config.whitelist {
                    ui.horizontal(|ui| {
                        ui.label(name);
                        if ui.small_button("✕").clicked() {
                            let _ = self.store.remove_whitelist(name);
                        }
                    });
                }
            });

        ui.separator();
        ui.heading("Running processes");
        ui.text_edit_singleline(&mut state.process_filter);
        let filter = state.process_filter.to_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("list_based_rule_procs")
            .max_height(220.0)
            .show(ui, |ui| {
                for process in processes {
                    if !filter.is_empty() && !process.name.to_lowercase().contains(&filter) {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        ui.label(format!("{} ({})", process.name, process.pid));
                        if ui.small_button("+BL").clicked() {
                            let _ = self.store.add_blacklist(&process.name);
                        }
                        if ui.small_button("+WL").clicked() {
                            let _ = self.store.add_whitelist(&process.name);
                        }
                    });
                }
            });

        ui.separator();
        ui.heading("Strategies");
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut state.strategy_input);
        });
        ui.horizontal(|ui| {
            if ui.button("Save current").clicked() && !state.strategy_input.trim().is_empty() {
                let name = state.strategy_input.trim().to_string();
                let result = self
                    .store
                    .save_strategy(&name)
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                Self::set_status(&mut state, result);
            }
            if ui.button("Apply").clicked() && !state.strategy_input.trim().is_empty() {
                let name = state.strategy_input.trim().to_string();
                let result = self
                    .store
                    .apply_strategy(&name)
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                Self::set_status(&mut state, result);
            }
        });
        for strategy in &config.strategies {
            ui.horizontal(|ui| {
                ui.label(&strategy.name);
                if ui.small_button("load").clicked() {
                    state.strategy_input = strategy.name.clone();
                    let result = self
                        .store
                        .apply_strategy(&strategy.name)
                        .map(|_| ())
                        .map_err(|error| error.to_string());
                    Self::set_status(&mut state, result);
                }
            });
        }

        if !state.status.is_empty() {
            ui.separator();
            ui.colored_label(egui::Color32::from_rgb(200, 80, 80), &state.status);
        }
    }
}

/// Responsibility: Hand out the same ListBasedRule as a block rule and as a settings tab.
///
/// Shared handle so the app can register block + UI sides of the same rule.
#[derive(Clone)]
pub struct ListBasedRuleHandle {
    inner: Arc<ListBasedRule>,
}

impl ListBasedRuleHandle {
    pub fn new(store: ListStore) -> Self {
        Self {
            inner: Arc::new(ListBasedRule::new(store)),
        }
    }

    pub fn rule(&self) -> Arc<dyn BlockRule> {
        Arc::clone(&self.inner) as Arc<dyn BlockRule>
    }

    pub fn ui(&self) -> Arc<dyn RuleUi> {
        Arc::clone(&self.inner) as Arc<dyn RuleUi>
    }

    pub fn store(&self) -> &ListStore {
        self.inner.store()
    }
}
