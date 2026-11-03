//! Responsibility: Let this PC sign up or log in to the owner's sync server, and a Windows
//! account log in to the login (and out again), so its access grants, blocks and AI usage are
//! the same on every PC (`machine::login`, `docs/sync.md`).
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
                            .hint_text("예: 3.35.0.1:47820"),
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
        if sync.is_none() {
            egui::CollapsingHeader::new("여러 기기 동기화: 서버에 로그인하거나 회원가입하기")
                .id_salt("sync_join")
                .show(ui, |ui| {
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
            return;
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
