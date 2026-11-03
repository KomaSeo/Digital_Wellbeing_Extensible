//! Responsibility: Let this PC sign up or log in to the owner's sync server, and a Windows
//! account log in to the login (and out again), so its access grants, blocks and AI usage are
//! the same on every PC (`machine::login`, `docs/sync.md`).
//!
//! The first time the Focus app opens on a PC that does not sync, it asks first whether to
//! join a sync server (log in / sign up / later) before anything else ([`LoginForm::welcome_ui`]).
//!
//! Signing up and logging in run the service's `sync-login` with administrator approval
//! ([`crate::elevate::run_sync_login`]). Logging out is refused while the settings are
//! protected.

use eframe::egui;
use machine::login::{LoginRequestFile, LoginStatus};
use machine::user_api::{call_value, SyncStatus};
use serde_json::json;
use std::sync::mpsc;

const RED: egui::Color32 = egui::Color32::from_rgb(200, 60, 60);
const GREEN: egui::Color32 = egui::Color32::from_rgb(60, 150, 60);

/// Responsibility: Hold what the user typed in the login boxes, and the login under way.
#[derive(Default)]
pub(crate) struct LoginForm {
    server_url: String,
    name: String,
    password: String,
    password_again: String,
    message: Option<Result<String, String>>,
    /// The elevated `sync-login` running on its own thread.
    pending: Option<mpsc::Receiver<Result<String, String>>>,
    /// The first-run question was answered (read from [`welcome_path`] on first use).
    welcome_done: Option<bool>,
    /// This PC just joined from the first-run question: show the outcome there once more.
    joined_from_welcome: bool,
}

/// Where this Windows account notes that it answered the first-run question (the Focus app's
/// own folder; an uninstall that deletes the owner's data deletes it too, so a fresh install
/// asks again).
fn welcome_path() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("com", "DigitalWellbeing", "DigitalWellbeing")
        .map(|project_directories| project_directories.data_local_dir().join("welcome.json"))
}

fn welcome_answered() -> bool {
    welcome_path().is_some_and(|path| path.exists())
}

fn note_welcome_answered() {
    if let Some(path) = welcome_path() {
        if let Some(folder) = path.parent() {
            let _ = std::fs::create_dir_all(folder);
        }
        let _ = std::fs::write(&path, r#"{ "sync_question_answered": true }"#);
    }
}

/// This Windows account's name.
fn windows_user() -> Option<String> {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .filter(|name| !name.is_empty())
}

impl LoginForm {
    /// Start the elevated `sync-login` (Windows asks for approval) without holding up the UI.
    fn start(&mut self, ui: &egui::Ui, server_url: &str, signup: bool) {
        if signup && self.password != self.password_again {
            self.message = Some(Err("두 비밀번호가 다릅니다".into()));
            return;
        }
        let request = LoginRequestFile {
            server_url: server_url.trim().to_string(),
            name: self.name.trim().to_string(),
            password: std::mem::take(&mut self.password),
            signup,
            windows_user: windows_user(),
        };
        self.password_again.clear();
        let (sender, receiver) = mpsc::channel();
        let context = ui.ctx().clone();
        std::thread::spawn(move || {
            let _ = sender.send(crate::elevate::run_sync_login(&request));
            context.request_repaint();
        });
        self.pending = Some(receiver);
        self.message = None;
    }

    /// Take in the outcome of a finished login.
    fn poll(&mut self) {
        if let Some(receiver) = &self.pending {
            if let Ok(outcome) = receiver.try_recv() {
                self.pending = None;
                self.message = Some(outcome.map(|_| {
                    "로그인했습니다. 몇 초 안에 이 계정의 파일로 다시 시작합니다.".to_string()
                }));
            }
        }
    }

    fn message_ui(&self, ui: &mut egui::Ui) {
        if self.pending.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("관리자 승인과 서버의 답을 기다립니다…");
            });
        }
        match &self.message {
            Some(Ok(message)) => {
                ui.colored_label(GREEN, message);
            }
            Some(Err(message)) => {
                ui.colored_label(RED, message);
            }
            None => {}
        }
    }

    fn credentials_ui(&mut self, ui: &mut egui::Ui, with_server: bool, signup_fields: bool) {
        egui::Grid::new(("login_form", with_server))
            .num_columns(2)
            .show(ui, |ui| {
                if with_server {
                    ui.label("서버 주소");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.server_url)
                            .hint_text("예: 3.35.0.1 (포트를 빼면 47820)"),
                    );
                    ui.end_row();
                }
                ui.label("이름");
                ui.text_edit_singleline(&mut self.name);
                ui.end_row();
                ui.label("비밀번호");
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                ui.end_row();
                if signup_fields {
                    ui.label("비밀번호 확인(회원가입만)");
                    ui.add(egui::TextEdit::singleline(&mut self.password_again).password(true));
                    ui.end_row();
                }
            });
    }

    /// The first screen on a PC that does not sync yet: join a sync server now (log in or sign
    /// up), or later (the same is in the Settings tab). Returns whether it took the screen.
    pub(crate) fn welcome_ui(&mut self, ui: &mut egui::Ui, sync: Option<&SyncStatus>) -> bool {
        self.poll();
        let done = *self.welcome_done.get_or_insert_with(welcome_answered);
        // A PC that already syncs with an account is not asked (unless it joined here just
        // now: the outcome is shown here once more, also while the login command is ending).
        // A PC paired with a code is asked: with an account it comes back by itself after
        // the server is set up again.
        let with_account = sync.is_some_and(|sync| sync.account.is_some());
        if done || (with_account && !self.joined_from_welcome && self.pending.is_none()) {
            return false;
        }
        if with_account || matches!(self.message, Some(Ok(_))) {
            self.joined_from_welcome = true;
        }
        self.prefill_server(sync);
        ui.heading("Digital Wellbeing에 오신 것을 환영합니다");
        ui.add_space(6.0);
        if let Some(sync) = sync.filter(|_| !self.joined_from_welcome) {
            ui.label(format!(
                "이 PC는 예전 방법(짝 코드)으로 {} 서버에 붙어 있습니다. 계정으로 로그인하면, 서버를 \
                 처음부터 다시 만들어도 이 PC가 저절로 다시 붙습니다.",
                sync.server_url
            ));
            if let Some(error) = &sync.last_error {
                ui.colored_label(RED, format!("마지막 동기화가 실패했습니다: {error}"));
            }
        }
        if self.joined_from_welcome {
            self.message_ui(ui);
            ui.label(
                "이 PC가 동기화 서버에 붙었습니다. 20초 안에 첫 동기화가 일어나고, 설정 탭의 \"기기 동기화\"에서 마지막 동기화 시각을 볼 수 있습니다. Anthropic API 키는 다른 PC가 쓰는 키를 받아 오므로 따로 넣지 않아도 됩니다. 이 PC가 처음이라면 설정 탭에서 키를 넣으면 다른 PC도 그 키를 씁니다.",
            );
            if ui.button("시작하기").clicked() {
                note_welcome_answered();
                self.welcome_done = Some(true);
            }
            return true;
        }
        ui.label(
            "이 PC를 여러 기기 동기화 서버에 붙일까요? 서버에 붙으면 이 PC가 다른 PC와 설정, 할 일 \
             목록, 지금 할 일, 차단을 같이 씁니다.",
        );
        ui.label(
            "• 서버를 새로 만들었다면, 가장 잘 설정해 둔 PC에서 먼저 회원가입합니다. 그 PC의 설정이 \
             서버로 올라갑니다.\n• 다른 PC에서는 같은 이름과 비밀번호로 로그인합니다. 그 PC는 서버의 \
             설정을 받아 옵니다.\n• 둘 다 관리자 승인(Windows가 관리자 비밀번호를 묻는 창)이 필요합니다. \
             비밀번호는 8자 이상이고, 관리자 비밀번호처럼 따로 보관합니다.",
        );
        ui.add_space(6.0);
        self.credentials_ui(ui, true, true);
        let busy = self.pending.is_some();
        ui.horizontal(|ui| {
            let server_url = self.server_url.clone();
            if ui
                .add_enabled(!busy, egui::Button::new("로그인 (관리자 승인)"))
                .clicked()
            {
                self.start(ui, &server_url, false);
            }
            if ui
                .add_enabled(!busy, egui::Button::new("회원가입 (관리자 승인)"))
                .clicked()
            {
                self.start(ui, &server_url, true);
            }
            if ui
                .add_enabled(!busy, egui::Button::new("나중에 (동기화 없이 쓰기)"))
                .clicked()
            {
                note_welcome_answered();
                self.welcome_done = Some(true);
            }
        });
        self.message_ui(ui);
        ui.add_space(6.0);
        ui.weak(
            "서버 주소는 동기화 서버의 고정 IP(AWS Lightsail이면 인스턴스의 네트워킹 탭에 있는 고정 \
             IP)나 도메인입니다. 나중에 붙이려면 설정 탭 맨 위의 \"여러 기기 동기화: 서버에 로그인하거나 회원가입하기\"를 \
             엽니다.",
        );
        true
    }

    /// The address this PC already syncs with, when the box is still empty.
    fn prefill_server(&mut self, sync: Option<&SyncStatus>) {
        if self.server_url.trim().is_empty() {
            if let Some(sync) = sync {
                self.server_url = sync.server_url.trim_start_matches("http://").to_string();
            }
        }
    }

    /// The login box, shown above every tab while this PC syncs, the server has logins, and
    /// this account has not logged in.
    pub(crate) fn prompt_ui(
        &mut self,
        ui: &mut egui::Ui,
        login: &LoginStatus,
        sync: Option<&SyncStatus>,
    ) {
        self.poll();
        let Some(sync) = sync else {
            return;
        };
        if !login.available || login.user.is_some() {
            return;
        }
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.heading("로그인");
            ui.label(
                "이 PC는 여러 기기 동기화를 씁니다. 로그인하면 접근 허용, 걸린 차단, AI 사용량이 \
                 모든 PC에서 같아집니다. 로그인하려면 관리자 승인이 필요하고, 이 Windows 계정은 \
                 한 번만 로그인하면 됩니다.",
            );
            self.credentials_ui(ui, false, false);
            let busy = self.pending.is_some();
            if ui
                .add_enabled(!busy, egui::Button::new("로그인 (관리자 승인)"))
                .clicked()
            {
                self.start(ui, &sync.server_url, false);
            }
            self.message_ui(ui);
        });
        ui.separator();
    }

    /// Settings tab: sign up or log in while this PC does not sync; who is logged in, with a
    /// logout button (none while the settings are protected).
    pub(crate) fn account_ui(
        &mut self,
        ui: &mut egui::Ui,
        session: u32,
        login: &LoginStatus,
        sync: Option<&SyncStatus>,
        settings_protected: bool,
    ) {
        self.poll();
        // Not syncing, or paired with a code (no account to come back with).
        if sync.is_none_or(|sync| sync.account.is_none()) {
            self.prefill_server(sync);
            egui::CollapsingHeader::new("여러 기기 동기화: 서버에 로그인하거나 회원가입하기")
                .id_salt("sync_join")
                .show(ui, |ui| {
                    if sync.is_some() {
                        ui.label(
                            "이 PC는 예전 방법(짝 코드)으로 서버에 붙어 있습니다. 계정으로 로그인하면 \
                             서버를 처음부터 다시 만들어도 저절로 다시 붙습니다.",
                        );
                    }
                    ui.label(
                        "동기화 서버에 로그인하면 이 PC가 다른 PC와 설정, 할 일, 차단을 같이 씁니다. \
                         서버를 새로 만들었다면 한 PC에서 먼저 회원가입하고(이 PC의 설정이 서버로 \
                         올라갑니다), 다른 PC에서는 같은 이름과 비밀번호로 로그인합니다(서버의 \
                         설정을 받아 옵니다). 둘 다 관리자 승인이 필요합니다. 비밀번호는 8자 \
                         이상이고, 관리자 비밀번호처럼 따로 보관합니다.",
                    );
                    self.credentials_ui(ui, true, true);
                    let busy = self.pending.is_some();
                    ui.horizontal(|ui| {
                        let server_url = self.server_url.clone();
                        if ui
                            .add_enabled(!busy, egui::Button::new("로그인 (관리자 승인)"))
                            .clicked()
                        {
                            self.start(ui, &server_url, false);
                        }
                        if ui
                            .add_enabled(!busy, egui::Button::new("회원가입 (관리자 승인)"))
                            .clicked()
                        {
                            self.start(ui, &server_url, true);
                        }
                    });
                    self.message_ui(ui);
                });
            if sync.is_none() {
                return;
            }
        }
        let Some(user) = &login.user else {
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("이 Windows 계정은 \"{user}\"로 로그인했습니다."));
            if !settings_protected && ui.button("로그아웃").clicked() {
                self.message = call_value(session, "logout", json!({}))
                    .map(|_| "로그아웃했습니다.".to_string())
                    .err()
                    .map(Err);
            }
        });
        if settings_protected {
            ui.small("설정이 보호되어 있는 동안에는 로그아웃할 수 없습니다.");
        } else {
            ui.small(
                "로그아웃하면 다시 로그인할 때까지 이 계정은 Windows 계정의 파일을 쓰고, 설정이 \
                 보호되면 모든 앱이 잠깁니다.",
            );
        }
        self.message_ui(ui);
    }
}
