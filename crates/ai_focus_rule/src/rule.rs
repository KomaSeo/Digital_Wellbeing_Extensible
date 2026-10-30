//! Responsibility: Connect the monitor to the enforcer (BlockRule) and draw the AI focus
//! settings tab (RuleUi).

use crate::config::AiFocusStore;
use crate::grants::{GrantEvent, GrantState};
use crate::judge::Verdict;
use crate::monitor::FocusMonitor;
use chrono::{Local, Utc};
use enforcer::{normalize_process_name, BlockEvent, BlockRule, ProcessSnapshot};
use parking_lot::Mutex;
use rule_ui::RuleUi;
use std::path::PathBuf;
use std::sync::Arc;

/// Responsibility: Keep the AI focus tab's edit buffers and status message between frames.
#[derive(Default)]
struct AiUiState {
    task_input: Option<String>,
    api_key_input: Option<String>,
    model_input: Option<String>,
    allow_input: String,
    private_input: String,
    status: String,
    /// When the admin view last re-read the grants files.
    grants_read: Option<std::time::Instant>,
    /// Admin view: (user, grants) read from every file in the grants folder.
    admin_grants: Vec<(String, GrantState)>,
}

/// Responsibility: Report the monitor's blocked processes to the enforcer, and draw the tab:
/// session, status, grants and settings.
///
/// Blocks apps that an AI judge finds unrelated to the declared task.
pub struct AiFocusRule {
    monitor: Arc<FocusMonitor>,
    /// `false` in the admin UI of a machine install: the session runs in the
    /// user's agent, so this panel only edits settings.
    session_controls: bool,
    /// Admin view of a machine install: folder with one grants file per user.
    admin_grants_dir: Option<PathBuf>,
    ui_state: Mutex<AiUiState>,
}

impl AiFocusRule {
    pub fn new(monitor: Arc<FocusMonitor>, session_controls: bool) -> Self {
        Self {
            admin_grants_dir: None,
            monitor,
            session_controls,
            ui_state: Mutex::new(AiUiState::default()),
        }
    }

    fn store(&self) -> &AiFocusStore {
        self.monitor.store()
    }

    fn save(&self, state: &mut AiUiState, f: impl FnOnce(&mut crate::AiFocusConfig)) {
        match self.store().update(f) {
            Ok(_) => state.status.clear(),
            Err(err) => state.status = err.to_string(),
        }
    }

    /// Access grants in force and their history; the admin can revoke.
    fn grants_ui(&self, ui: &mut egui::Ui, config: &crate::AiFocusConfig, s: &mut AiUiState) {
        // The admin view reads the agents' files; refresh them now and then.
        // (With session controls this process owns the store: always current.)
        let sources: Vec<(String, GrantState)> = match &self.admin_grants_dir {
            Some(dir) => {
                if s.grants_read
                    .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(3))
                {
                    s.admin_grants = read_grant_files(dir);
                    s.grants_read = Some(std::time::Instant::now());
                }
                s.admin_grants.clone()
            }
            None => vec![(String::new(), self.monitor.grants().get())],
        };
        let now = Utc::now();

        ui.separator();
        ui.heading("Access grants");
        ui.label(
            "Apps and sites the AI let the user use for a stated purpose. They stay observed: \
             off-purpose use revokes the grant and blocks again.",
        );
        let active: Vec<_> = sources
            .iter()
            .flat_map(|(user, state)| state.grants.iter().map(move |g| (user, g)))
            .filter(|(_, g)| g.is_active(now, config))
            .collect();
        if active.is_empty() {
            ui.label("None.");
        }
        for (user, g) in active {
            ui.horizontal_wrapped(|ui| {
                if !user.is_empty() {
                    ui.strong(user.as_str());
                }
                let until = g
                    .until
                    .map(|u| {
                        u.with_timezone(&Local)
                            .format("until %m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_else(|| "permanent".into());
                ui.label(format!("{} ({until}) — {}", g.target.label(), g.purpose));
                if ui.small_button("revoke").clicked() {
                    let id = g.id.clone();
                    // Through the config file, so this UI never writes a grants file.
                    self.save(s, |c| {
                        c.revoked_grants.insert(id);
                    });
                }
            });
        }
        ui.collapsing("History", |ui| {
            let mut log: Vec<_> = sources
                .iter()
                .flat_map(|(user, state)| state.log.iter().map(move |e| (user, e)))
                .collect();
            log.sort_by(|a, b| b.1.at.cmp(&a.1.at));
            for (user, e) in log {
                let what = match &e.event {
                    GrantEvent::Requested {
                        approved: true,
                        granted_minutes: Some(m),
                        ..
                    } => format!("granted {m} min"),
                    GrantEvent::Requested { approved: true, .. } => "granted permanently".into(),
                    GrantEvent::Requested { .. } => "refused".into(),
                    GrantEvent::RevokedForMisuse => "revoked (misuse)".into(),
                    GrantEvent::RevokedByAdmin => "revoked by admin".into(),
                };
                ui.label(format!(
                    "{}  {}{}  {} — \"{}\" {}",
                    e.at.with_timezone(&Local).format("%m-%d %H:%M"),
                    if user.is_empty() {
                        String::new()
                    } else {
                        format!("{user}: ")
                    },
                    e.target.label(),
                    what,
                    e.purpose,
                    e.message
                ));
            }
        });
    }

    fn session_ui(&self, ui: &mut egui::Ui, config: &crate::AiFocusConfig, s: &mut AiUiState) {
        // --- Session -------------------------------------------------------
        ui.heading("Focus session");
        ui.label("What are you working on? (be specific — the AI judges against this)");
        let task = s.task_input.get_or_insert_with(|| config.task.clone());
        ui.add(
            egui::TextEdit::multiline(task)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text("e.g. 졸업논문 3장 작성 — Overleaf, 논문 PDF, Google Scholar 필요"),
        );
        ui.horizontal(|ui| {
            if config.session_active {
                ui.colored_label(egui::Color32::from_rgb(80, 170, 90), "● running");
                if ui.button("Update task").clicked() {
                    let task = s.task_input.clone().unwrap_or_default();
                    self.save(s, |c| c.task = task);
                }
                if ui.button("Stop session").clicked() {
                    self.save(s, |c| c.session_active = false);
                }
            } else {
                ui.label("○ stopped");
                let task = s.task_input.clone().unwrap_or_default();
                if ui
                    .add_enabled(!task.trim().is_empty(), egui::Button::new("Start session"))
                    .clicked()
                {
                    self.save(s, |c| {
                        c.task = task;
                        c.session_active = true;
                    });
                }
            }
        });

        let (last_check, last_error, verdicts, process_blocks, tab_blocks) = {
            let state = self.monitor.state();
            (
                state.last_check,
                state.last_error.clone(),
                state
                    .verdicts
                    .iter()
                    .rev()
                    .take(30)
                    .cloned()
                    .collect::<Vec<_>>(),
                state
                    .blocked_processes
                    .iter()
                    .map(|(k, b)| (k.clone(), b.clone()))
                    .collect::<Vec<_>>(),
                state
                    .blocked_tabs
                    .iter()
                    .map(|(k, b)| (k.clone(), b.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        if let Some(at) = last_check {
            ui.label(format!(
                "Last check: {}",
                at.with_timezone(&Local).format("%H:%M:%S")
            ));
        }
        if let Some(err) = last_error {
            ui.colored_label(egui::Color32::from_rgb(200, 80, 80), err);
        }

        // --- Blocked now -----------------------------------------------------
        ui.separator();
        ui.heading("Blocked now");
        if process_blocks.is_empty() && tab_blocks.is_empty() {
            ui.label("Nothing.");
        }
        for (key, block) in process_blocks.iter().chain(tab_blocks.iter()) {
            ui.horizontal(|ui| {
                let until = block
                    .until
                    .map(|u| u.with_timezone(&Local).format("until %H:%M").to_string())
                    .unwrap_or_else(|| "until session ends".into());
                ui.label(format!("{key}  ({until}) — {}", block.reason));
                if ui.small_button("unblock").clicked() {
                    self.monitor.state().unblock(key);
                }
            });
        }

        // --- Verdict log ------------------------------------------------------
        ui.separator();
        ui.heading("Recent AI checks");
        egui::ScrollArea::vertical()
            .id_salt("ai_focus_verdicts")
            .max_height(200.0)
            .show(ui, |ui| {
                if verdicts.is_empty() {
                    ui.label("No checks yet.");
                }
                for v in &verdicts {
                    let (color, label) = match v.verdict {
                        Verdict::OnTask => (egui::Color32::from_rgb(80, 170, 90), "on task"),
                        Verdict::OffTask => (egui::Color32::from_rgb(200, 80, 80), "off task"),
                        Verdict::Unsure => (egui::Color32::GRAY, "unsure"),
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(v.at.with_timezone(&Local).format("%H:%M:%S").to_string());
                        ui.colored_label(color, label);
                        ui.label(format!("{} \"{}\" — {}", v.process_name, v.title, v.reason));
                        if !v.action.is_empty() {
                            ui.strong(format!("[{}]", v.action));
                        }
                    });
                }
            });
    }
}

impl BlockRule for AiFocusRule {
    fn id(&self) -> &str {
        "ai_focus_rule"
    }

    fn name(&self) -> &str {
        "AI focus"
    }

    fn description(&self) -> &str {
        "Periodically shows the AI your recent windows and a screenshot, and blocks \
         apps or closes browser tabs it judges unrelated to your current task"
    }

    fn evaluate(&self, processes: &[ProcessSnapshot]) -> Vec<BlockEvent> {
        let blocks = self.monitor.state().active_process_blocks(Utc::now());
        if blocks.is_empty() {
            return Vec::new();
        }
        processes
            .iter()
            .filter_map(|p| {
                let name = normalize_process_name(&p.name);
                blocks
                    .iter()
                    .find(|(blocked, _)| *blocked == name)
                    .map(|(_, reason)| BlockEvent::new(p, format!("off-task: {reason}")))
            })
            .collect()
    }
}

impl RuleUi for AiFocusRule {
    fn settings_ui(&self, ui: &mut egui::Ui, processes: &[ProcessSnapshot]) {
        let config = self.store().get();
        let mut s = self.ui_state.lock();

        ui.label(BlockRule::description(self));
        ui.separator();

        if self.session_controls {
            self.session_ui(ui, &config, &mut s);
        } else {
            ui.label(
                "Machine install: each user starts and stops their own focus session in the \
                 Digital Wellbeing Focus app. Settings below apply to every managed account.",
            );
        }

        // --- Settings -----------------------------------------------------------
        ui.separator();
        ui.heading("Settings");
        egui::Grid::new("ai_focus_settings")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Check every (seconds)");
                let mut interval = config.check_interval_secs;
                if ui
                    .add(egui::DragValue::new(&mut interval).range(15..=1800))
                    .changed()
                {
                    self.save(&mut s, |c| c.check_interval_secs = interval);
                }
                ui.end_row();

                ui.label("Off-task checks before block");
                let mut strikes = config.strikes_to_block;
                if ui
                    .add(egui::DragValue::new(&mut strikes).range(1..=10))
                    .changed()
                {
                    self.save(&mut s, |c| c.strikes_to_block = strikes);
                }
                ui.end_row();

                ui.label("Block for (minutes, 0 = session)");
                let mut minutes = config.block_minutes;
                if ui
                    .add(egui::DragValue::new(&mut minutes).range(0..=600))
                    .changed()
                {
                    self.save(&mut s, |c| c.block_minutes = minutes);
                }
                ui.end_row();

                ui.label("Send screenshot");
                let mut shot = config.send_screenshot;
                if ui.checkbox(&mut shot, "").changed() {
                    self.save(&mut s, |c| c.send_screenshot = shot);
                }
                ui.end_row();

                ui.label("Model");
                ui.horizontal(|ui| {
                    let model = s.model_input.get_or_insert_with(|| config.model.clone());
                    ui.text_edit_singleline(model);
                    if ui.button("Save").clicked() {
                        let model = s.model_input.clone().unwrap_or_default();
                        if !model.trim().is_empty() {
                            self.save(&mut s, |c| c.model = model.trim().to_string());
                        }
                    }
                });
                ui.end_row();

                ui.label("Users may ask for permanent access");
                let mut permanent = config.allow_permanent_grants;
                if ui.checkbox(&mut permanent, "").changed() {
                    self.save(&mut s, |c| c.allow_permanent_grants = permanent);
                }
                ui.end_row();

                ui.label("Longest timed access (minutes)");
                let mut max = config.max_grant_minutes;
                if ui
                    .add(egui::DragValue::new(&mut max).range(5..=600))
                    .changed()
                {
                    self.save(&mut s, |c| c.max_grant_minutes = max);
                }
                ui.end_row();

                ui.label("API key");
                ui.horizontal(|ui| {
                    if std::env::var("ANTHROPIC_API_KEY").is_ok_and(|k| !k.is_empty()) {
                        ui.label("using ANTHROPIC_API_KEY");
                    } else {
                        let key = s
                            .api_key_input
                            .get_or_insert_with(|| config.api_key.clone());
                        ui.add(egui::TextEdit::singleline(key).password(true));
                        if ui.button("Save").clicked() {
                            let key = s.api_key_input.clone().unwrap_or_default();
                            self.save(&mut s, |c| c.api_key = key.trim().to_string());
                        }
                    }
                });
                ui.end_row();
            });
        ui.small("A saved key is stored in plain text in ai_focus.json; prefer the ANTHROPIC_API_KEY environment variable.");

        self.grants_ui(ui, &config, &mut s);

        // --- Lists ------------------------------------------------------------------
        ui.separator();
        name_list(
            ui,
            "Always allow (never judged)",
            "ai_focus_allow",
            &config.always_allow,
            &mut s.allow_input,
            processes,
            |name, add| {
                let _ = self.store().update(|c| {
                    if add {
                        c.always_allow.insert(name);
                    } else {
                        c.always_allow.remove(&name);
                    }
                });
            },
        );
        ui.separator();
        name_list(
            ui,
            "Private apps (title only, no screenshot)",
            "ai_focus_private",
            &config.private_apps,
            &mut s.private_input,
            processes,
            |name, add| {
                let _ = self.store().update(|c| {
                    if add {
                        c.private_apps.insert(name);
                    } else {
                        c.private_apps.remove(&name);
                    }
                });
            },
        );

        if !s.status.is_empty() {
            ui.separator();
            ui.colored_label(egui::Color32::from_rgb(200, 80, 80), &s.status);
        }
    }
}

/// Editable set of process names with a picker from running processes.
fn name_list(
    ui: &mut egui::Ui,
    title: &str,
    id: &str,
    names: &std::collections::BTreeSet<String>,
    input: &mut String,
    processes: &[ProcessSnapshot],
    mut change: impl FnMut(String, bool),
) {
    ui.label(title);
    for name in names {
        ui.horizontal(|ui| {
            ui.label(name);
            if ui.small_button("✕").clicked() {
                change(name.clone(), false);
            }
        });
    }
    ui.horizontal(|ui| {
        ui.text_edit_singleline(input);
        if ui.button("Add").clicked() && !input.trim().is_empty() {
            change(normalize_process_name(input), true);
            input.clear();
        }
        egui::ComboBox::from_id_salt(id)
            .selected_text("running…")
            .show_ui(ui, |ui| {
                let mut seen = std::collections::BTreeSet::new();
                for p in processes {
                    let name = normalize_process_name(&p.name);
                    if seen.insert(name.clone()) && ui.selectable_label(false, &name).clicked() {
                        change(name, true);
                    }
                }
            });
    });
}

/// Every `<user>.json` in `dir` as (user, grants); unreadable files are skipped.
fn read_grant_files(dir: &std::path::Path) -> Vec<(String, GrantState)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, GrantState)> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let user = p.file_stem()?.to_string_lossy().to_string();
            let state = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
            Some((user, state))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Responsibility: Hand out the same AiFocusRule as a block rule and as a settings tab, plus
/// its monitor.
///
/// Shared handle so the app can register block + UI sides of the same rule.
#[derive(Clone)]
pub struct AiFocusRuleHandle {
    inner: Arc<AiFocusRule>,
    monitor: Arc<FocusMonitor>,
}

impl AiFocusRuleHandle {
    pub fn new(monitor: Arc<FocusMonitor>) -> Self {
        Self::build(monitor, true)
    }

    /// Settings panel only, for editing a machine install's config from the admin UI.
    /// The monitor is never started; grants are read from every file in `grants_dir`.
    pub fn settings_only(monitor: Arc<FocusMonitor>, grants_dir: PathBuf) -> Self {
        let mut rule = AiFocusRule::new(Arc::clone(&monitor), false);
        rule.admin_grants_dir = Some(grants_dir);
        Self {
            inner: Arc::new(rule),
            monitor,
        }
    }

    fn build(monitor: Arc<FocusMonitor>, session_controls: bool) -> Self {
        Self {
            inner: Arc::new(AiFocusRule::new(Arc::clone(&monitor), session_controls)),
            monitor,
        }
    }

    pub fn rule(&self) -> Arc<dyn BlockRule> {
        Arc::clone(&self.inner) as Arc<dyn BlockRule>
    }

    pub fn ui(&self) -> Arc<dyn RuleUi> {
        Arc::clone(&self.inner) as Arc<dyn RuleUi>
    }

    pub fn monitor(&self) -> &Arc<FocusMonitor> {
        &self.monitor
    }
}
