//! Responsibility: Let a Windows account log in to the owner's sync login (and out again), so
//! its access grants, blocks and AI usage are the same on every PC (`machine::login`).

use eframe::egui;
use machine::login::LoginStatus;
use machine::user_api::call_value;
use serde_json::json;

const RED: egui::Color32 = egui::Color32::from_rgb(200, 60, 60);

/// Responsibility: Hold what the user typed in the login box.
#[derive(Default)]
pub(crate) struct LoginForm {
    name: String,
    password: String,
    message: String,
}

impl LoginForm {
    /// The login box, shown above every tab while this account has not logged in.
    pub(crate) fn prompt_ui(&mut self, ui: &mut egui::Ui, session: u32, login: &LoginStatus) {
        if !login.available || login.user.is_some() {
            return;
        }
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.heading("로그인");
            ui.label(
                "이 PC는 여러 기기 동기화를 씁니다. 로그인하면 접근 허용, 걸린 차단, AI 사용량이 \
                 모든 PC에서 같아집니다. 이름과 비밀번호는 동기화 서버의 관리자가 만듭니다. 이 \
                 Windows 계정은 한 번만 로그인하면 됩니다.",
            );
            egui::Grid::new("login_form").num_columns(2).show(ui, |ui| {
                ui.label("이름");
                ui.text_edit_singleline(&mut self.name);
                ui.end_row();
                ui.label("비밀번호");
                let password =
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                ui.end_row();
                let pressed_enter =
                    password.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if ui.button("로그인").clicked() || pressed_enter {
                    match call_value(
                        session,
                        "login",
                        json!({ "name": self.name.trim(), "password": self.password }),
                    ) {
                        Ok(_) => {
                            self.password.clear();
                            self.message =
                                "로그인했습니다. 몇 초 안에 이 계정의 파일로 다시 시작합니다."
                                    .into();
                        }
                        Err(error) => self.message = error,
                    }
                }
                ui.end_row();
            });
            if !self.message.is_empty() {
                ui.colored_label(RED, &self.message);
            }
        });
        ui.separator();
    }

    /// Who is logged in, with a logout button (Settings tab).
    pub(crate) fn account_ui(&mut self, ui: &mut egui::Ui, session: u32, login: &LoginStatus) {
        let Some(user) = &login.user else {
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("이 Windows 계정은 \"{user}\"로 로그인했습니다."));
            if ui.button("로그아웃").clicked() {
                if let Err(error) = call_value(session, "logout", json!({})) {
                    self.message = error;
                }
            }
        });
        ui.small(
            "로그아웃하면 다시 로그인할 때까지 이 계정은 Windows 계정의 파일을 쓰고, 설정이 보호되어 \
             있으면 모든 앱이 잠깁니다.",
        );
        if !self.message.is_empty() {
            ui.colored_label(RED, &self.message);
        }
    }
}
