//! Responsibility: Connect the monitor to the enforcer (BlockRule) and draw the AI focus
//! settings tab (RuleUi).

use crate::config::AiFocusStore;
use crate::grants::{GrantEvent, GrantState};
use crate::judge::Verdict;
use crate::monitor::{FocusMonitor, SavedBlocks};
use crate::usage::{Totals, UsageLog};
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
    /// When the admin view last re-read the blocks files.
    blocks_read: Option<std::time::Instant>,
    /// Admin view: (user, blocks) read from every file in the blocks folder.
    admin_blocks: Vec<(String, SavedBlocks)>,
    /// When the admin view last re-read the usage files.
    usage_read: Option<std::time::Instant>,
    /// Admin view: (user, token usage) read from every file in the usage folder.
    admin_usage: Vec<(String, UsageLog)>,
    show_key: bool,
    /// Result of the last "Check key", written by a background thread.
    key_check: Arc<Mutex<Option<String>>>,
    key_checking: Arc<std::sync::atomic::AtomicBool>,
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
    /// Admin view of a machine install: folder with one blocks file per user.
    admin_blocks_dir: Option<PathBuf>,
    /// Admin view of a machine install: folder with one token-usage file per user.
    admin_usage_dir: Option<PathBuf>,
    ui_state: Mutex<AiUiState>,
}

impl AiFocusRule {
    pub fn new(monitor: Arc<FocusMonitor>, session_controls: bool) -> Self {
        Self {
            admin_grants_dir: None,
            admin_blocks_dir: None,
            admin_usage_dir: None,
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

    /// Enter, check and clear the Anthropic API key. Always shown: on a machine
    /// install the agents run as SYSTEM and cannot see the environment
    /// variables of the account this window runs in, so the key must be saved.
    fn api_key_ui(&self, ui: &mut egui::Ui, config: &crate::AiFocusConfig, s: &mut AiUiState) {
        let saved = config.api_key.trim();
        if saved.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(200, 80, 80), "No key saved.");
        } else {
            let tail: String = saved
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            ui.label(format!("A key is saved (…{tail})."));
        }
        let env_key = std::env::var("ANTHROPIC_API_KEY").is_ok_and(|k| !k.trim().is_empty());
        if env_key && self.session_controls {
            ui.small(
                "ANTHROPIC_API_KEY is set for this account; it is used instead of the saved key.",
            );
        }
        ui.horizontal(|ui| {
            let key = s.api_key_input.get_or_insert_with(String::new);
            ui.add(
                egui::TextEdit::singleline(key)
                    .password(!s.show_key)
                    .hint_text("sk-ant-…")
                    .desired_width(260.0),
            );
            ui.checkbox(&mut s.show_key, "show");
        });
        ui.horizontal(|ui| {
            let typed = s.api_key_input.clone().unwrap_or_default();
            if ui
                .add_enabled(!typed.trim().is_empty(), egui::Button::new("Save"))
                .clicked()
            {
                self.save(s, |c| c.api_key = typed.trim().to_string());
                s.api_key_input = Some(String::new());
                *s.key_check.lock() = None;
            }
            if ui
                .add_enabled(!saved.is_empty(), egui::Button::new("Clear"))
                .clicked()
            {
                self.save(s, |c| c.api_key.clear());
                *s.key_check.lock() = None;
            }
            let checking = s.key_checking.load(std::sync::atomic::Ordering::SeqCst);
            // Check the typed key if there is one, otherwise the saved one.
            let to_check = if typed.trim().is_empty() {
                saved.to_string()
            } else {
                typed.trim().to_string()
            };
            if ui
                .add_enabled(
                    !checking && !to_check.is_empty(),
                    egui::Button::new("Check key"),
                )
                .clicked()
            {
                let (result, busy, ctx) = (
                    Arc::clone(&s.key_check),
                    Arc::clone(&s.key_checking),
                    ui.ctx().clone(),
                );
                let model = config.model.clone();
                busy.store(true, std::sync::atomic::Ordering::SeqCst);
                std::thread::spawn(move || {
                    let message =
                        match crate::claude::ClaudeClient::new(to_check, model.clone()).check() {
                            Ok(()) => format!("✔ The key works and model {model} is available."),
                            Err(crate::JudgeError::Api { status: 401, .. }) => {
                                "✖ The key was rejected (401).".into()
                            }
                            Err(crate::JudgeError::Api { status: 404, .. }) => {
                                format!("✖ The key works, but model {model} was not found (404).")
                            }
                            Err(err) => format!("✖ {err}"),
                        };
                    *result.lock() = Some(message);
                    busy.store(false, std::sync::atomic::Ordering::SeqCst);
                    ctx.request_repaint();
                });
            }
            if checking {
                ui.spinner();
            }
        });
        if let Some(message) = s.key_check.lock().as_ref() {
            ui.label(message.as_str());
        }
        ui.small("The key is saved without encryption in ai_focus.json. On a machine install only administrators can open that folder.");
    }

    /// Admin view: every user's blocks, each with an "unblock" button. The
    /// button names the block in the config file (`unblocked`); the user's
    /// agent then drops it, so this UI never writes an agent's blocks file.
    fn admin_blocks_ui(&self, ui: &mut egui::Ui, config: &crate::AiFocusConfig, s: &mut AiUiState) {
        let Some(dir) = &self.admin_blocks_dir else {
            return;
        };
        if s.blocks_read
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(3))
        {
            let (blocks, all_read) = read_user_files::<SavedBlocks>(dir);
            s.blocks_read = Some(std::time::Instant::now());
            // Forget ids no blocks file holds any more: their agent has
            // dropped them. Only when every file could be read, or a block
            // in an unreadable file would come back.
            if all_read {
                let present: std::collections::BTreeSet<&String> = blocks
                    .iter()
                    .flat_map(|(_, b)| b.processes.values().chain(b.tabs.values()))
                    .map(|b| &b.id)
                    .collect();
                if config.unblocked.iter().any(|id| !present.contains(id)) {
                    let keep: std::collections::BTreeSet<String> = config
                        .unblocked
                        .iter()
                        .filter(|id| present.contains(id))
                        .cloned()
                        .collect();
                    self.save(s, |c| c.unblocked = keep);
                }
            }
            s.admin_blocks = blocks;
        }

        ui.separator();
        ui.heading("Blocked now");
        ui.label(
            "Blocks do not expire. A user lifts one by asking for access with a purpose;              here you can lift one directly.",
        );
        let blocks: Vec<(String, String, crate::BlockedEntry)> = s
            .admin_blocks
            .iter()
            .flat_map(|(user, b)| {
                b.processes
                    .iter()
                    .chain(b.tabs.iter())
                    .map(move |(key, e)| (user.clone(), key.clone(), e.clone()))
            })
            .collect();
        if blocks.is_empty() {
            ui.label("Nothing.");
        }
        for (user, key, block) in blocks {
            ui.horizontal_wrapped(|ui| {
                ui.strong(user.as_str());
                ui.label(format!("{key} — {}", block.reason));
                if config.unblocked.contains(&block.id) {
                    ui.weak("unblocking…");
                } else if !block.id.is_empty() && ui.small_button("unblock").clicked() {
                    let id = block.id.clone();
                    self.save(s, |c| {
                        c.unblocked.insert(id);
                    });
                }
            });
        }
    }

    /// Token usage and estimated cost of the Claude calls (today, this month, by purpose and
    /// user), and the daily budget for focus checks. See `usage.rs`.
    fn usage_ui(&self, ui: &mut egui::Ui, config: &crate::AiFocusConfig, s: &mut AiUiState) {
        let sources: Vec<(String, UsageLog)> = match &self.admin_usage_dir {
            Some(dir) => {
                if s.usage_read
                    .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(3))
                {
                    s.admin_usage = read_user_files(dir).0;
                    s.usage_read = Some(std::time::Instant::now());
                }
                s.admin_usage.clone()
            }
            None => self
                .monitor
                .usage()
                .map(|u| vec![(String::new(), u.log())])
                .unwrap_or_default(),
        };
        let today_date = Local::now().date_naive();
        let mut today = Totals::default();
        let mut month = Totals::default();
        let mut by_purpose: std::collections::BTreeMap<String, Totals> = Default::default();
        for (_, log) in &sources {
            let day = log.day(today_date);
            today.merge(&day.total);
            month.merge(&log.month(today_date));
            for (purpose, totals) in day.by_purpose {
                by_purpose.entry(purpose).or_default().merge(&totals);
            }
        }

        ui.separator();
        ui.heading("AI usage");
        egui::Grid::new("ai_usage").num_columns(5).show(ui, |ui| {
            ui.label("");
            ui.label("calls");
            ui.label("input tokens");
            ui.label("output tokens");
            ui.label("estimated cost");
            ui.end_row();
            let row = |ui: &mut egui::Ui, name: &str, t: &Totals| {
                ui.label(name);
                ui.label(t.calls.to_string());
                ui.label((t.input_tokens + t.cache_read_tokens + t.cache_write_tokens).to_string());
                ui.label(t.output_tokens.to_string());
                ui.label(format!("${:.3}", t.cost_usd));
                ui.end_row();
            };
            row(ui, "Today", &today);
            row(ui, "This month", &month);
            for (purpose, totals) in &by_purpose {
                row(ui, &format!("  today: {purpose}"), totals);
            }
        });
        if sources.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                ui.label("Today by user:");
                for (user, log) in &sources {
                    ui.label(format!("{user} ${:.3}", log.day(today_date).total.cost_usd));
                }
            });
        }
        ui.horizontal(|ui| {
            ui.label("Daily budget for focus checks (USD, 0 = none)");
            let mut budget = config.daily_budget_usd;
            if ui
                .add(
                    egui::DragValue::new(&mut budget)
                        .range(0.0..=1000.0)
                        .speed(0.05)
                        .fixed_decimals(2),
                )
                .changed()
            {
                self.save(s, |c| c.daily_budget_usd = budget);
            }
        });
        if config.daily_budget_usd > 0.0 && today.cost_usd >= config.daily_budget_usd {
            ui.colored_label(
                egui::Color32::from_rgb(200, 80, 80),
                "Today's budget is spent: focus checks are paused until tomorrow. Blocks stay; \
                 planner and access requests still work.",
            );
        }
        ui.small(
            "Estimated from the token counts the API returns and the price table in usage.rs. \
             The Claude Console shows the real bill and the remaining credit; the API offers \
             no way to read the credit.",
        );
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
                    s.admin_grants = read_user_files(dir).0;
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
            log.sort_by_key(|(_, e)| std::cmp::Reverse(e.at));
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
                ui.label(format!("{key} — {}", block.reason));
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
        let blocks = self.monitor.state().process_blocks();
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

                ui.label("Skip re-checking an on-task window for (minutes, 0 = never)");
                let mut recheck = config.recheck_minutes;
                if ui
                    .add(egui::DragValue::new(&mut recheck).range(0..=240))
                    .changed()
                {
                    self.save(&mut s, |c| c.recheck_minutes = recheck);
                }
                ui.end_row();

                ui.label("Off-task checks before an app is closed (tabs: first)");
                let mut strikes = config.strikes_to_block;
                if ui
                    .add(egui::DragValue::new(&mut strikes).range(1..=10))
                    .changed()
                {
                    self.save(&mut s, |c| c.strikes_to_block = strikes);
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
                ui.vertical(|ui| self.api_key_ui(ui, &config, &mut s));
                ui.end_row();
            });

        self.admin_blocks_ui(ui, &config, &mut s);
        self.grants_ui(ui, &config, &mut s);
        self.usage_ui(ui, &config, &mut s);

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

/// Every `<user>.json` in `dir` as (user, contents); unreadable files are
/// skipped. The flag is false if any file was skipped (or `dir` unreadable).
fn read_user_files<T: serde::de::DeserializeOwned>(
    dir: &std::path::Path,
) -> (Vec<(String, T)>, bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // No folder yet (no agent has blocked anything): nothing to miss.
        return (Vec::new(), !dir.exists());
    };
    let mut all_read = true;
    let mut out: Vec<(String, T)> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let user = p.file_stem()?.to_string_lossy().to_string();
            let read = std::fs::read_to_string(&p)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok());
            if read.is_none() {
                all_read = false;
            }
            Some((user, read?))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    (out, all_read)
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
    /// The monitor is never started; grants, blocks and token usage are read from every
    /// file in `grants_dir`, `blocks_dir` and `usage_dir`.
    pub fn settings_only(
        monitor: Arc<FocusMonitor>,
        grants_dir: PathBuf,
        blocks_dir: PathBuf,
        usage_dir: PathBuf,
    ) -> Self {
        let mut rule = AiFocusRule::new(Arc::clone(&monitor), false);
        rule.admin_grants_dir = Some(grants_dir);
        rule.admin_blocks_dir = Some(blocks_dir);
        rule.admin_usage_dir = Some(usage_dir);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{platform, AiFocusConfig, AiFocusStore, JudgeError};

    fn render(rule: &dyn RuleUi) {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| rule.settings_ui(ui, &[]));
        });
    }

    #[test]
    fn settings_tab_renders_in_both_modes() {
        let config = AiFocusConfig {
            api_key: "sk-ant-test-abcd".into(),
            ..Default::default()
        };
        let monitor = Arc::new(FocusMonitor::new(
            AiFocusStore::in_memory(config),
            platform::native(),
            Arc::new(|_: &AiFocusConfig| Err(JudgeError::NoApiKey)),
        ));
        render(AiFocusRuleHandle::new(Arc::clone(&monitor)).ui().as_ref());
        let dir = std::env::temp_dir().join("dw_no_grants_dir");
        render(
            AiFocusRuleHandle::settings_only(monitor, dir.clone(), dir.clone(), dir)
                .ui()
                .as_ref(),
        );
    }
}
