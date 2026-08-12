use crate::{ListMode, ListStore};
use enforcer::{BlockEvent, BlockRule, ProcessSnapshot};
use parking_lot::Mutex;
use rule_ui::RuleUi;
use std::sync::Arc;

#[derive(Default)]
struct ListUiState {
    name_input: String,
    strategy_input: String,
    status: String,
}

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
            Err(err) => ui_state.status = err,
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
            .filter(|p| config.should_block(&p.name))
            .map(|p| BlockEvent::new(p, reason))
            .collect()
    }
}

impl RuleUi for ListBasedRule {
    fn settings_ui(&self, ui: &mut egui::Ui) {
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
                    .map_err(|e| e.to_string());
                Self::set_status(&mut state, result);
                state.name_input.clear();
            }
            if ui.button("Add whitelist").clicked() && !state.name_input.trim().is_empty() {
                let name = state.name_input.trim().to_string();
                let result = self
                    .store
                    .add_whitelist(&name)
                    .map(|_| ())
                    .map_err(|e| e.to_string());
                Self::set_status(&mut state, result);
                state.name_input.clear();
            }
        });

        ui.separator();
        ui.label("Blacklist");
        egui::ScrollArea::vertical()
            .id_salt("list_based_rule_bl")
            .max_height(200.0)
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
            .max_height(220.0)
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
                    .map_err(|e| e.to_string());
                Self::set_status(&mut state, result);
            }
            if ui.button("Apply").clicked() && !state.strategy_input.trim().is_empty() {
                let name = state.strategy_input.trim().to_string();
                let result = self
                    .store
                    .apply_strategy(&name)
                    .map(|_| ())
                    .map_err(|e| e.to_string());
                Self::set_status(&mut state, result);
            }
        });
        for s in &config.strategies {
            ui.horizontal(|ui| {
                ui.label(&s.name);
                if ui.small_button("load").clicked() {
                    state.strategy_input = s.name.clone();
                    let result = self
                        .store
                        .apply_strategy(&s.name)
                        .map(|_| ())
                        .map_err(|e| e.to_string());
                    Self::set_status(&mut state, result);
                }
            });
        }

        if !state.status.is_empty() {
            ui.separator();
            ui.colored_label(egui::Color32::from_rgb(200, 80, 80), &state.status);
        }
    }

    fn process_row_ui(&self, ui: &mut egui::Ui, process_name: &str) {
        if ui.small_button("+BL").clicked() {
            let _ = self.store.add_blacklist(process_name);
        }
        if ui.small_button("+WL").clicked() {
            let _ = self.store.add_whitelist(process_name);
        }
    }
}

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
