//! 集/卫空档界面；任务名称沿用 API 提供的展示名称。

use super::{NORMAL_CONTENT_H, show_message_row, window_buttons};
use crate::app::{AppState, MowerHUD};
use crate::model::{MowerStatus, TaskInfo};

pub(super) fn show(
    app: &mut MowerHUD,
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    status: MowerStatus,
    offline: bool,
    task: Option<&TaskInfo>,
) -> Option<egui::Vec2> {
    if app.config.leisure.enabled && app.leisure.active {
        if offline {
            return Some(show_message_row(
                ui,
                ctx,
                app,
                "连接中断，接管时间未知",
                if ui.visuals().dark_mode {
                    egui::Color32::YELLOW
                } else {
                    egui::Color32::from_rgb(155, 80, 0)
                },
            ));
        }
        if status == MowerStatus::Sleeping
            && let Some(task) = task
        {
            let seconds = task
                .current_time
                .signed_duration_since(chrono::Local::now())
                .num_seconds();
            if seconds > 0 {
                let warning = seconds <= i64::from(app.config.leisure.warning_minutes) * 60;
                let color = match (warning, ui.visuals().dark_mode) {
                    (true, true) => egui::Color32::from_rgb(255, 150, 80),
                    (false, true) => egui::Color32::from_rgb(110, 220, 150),
                    (true, false) => egui::Color32::from_rgb(170, 65, 0),
                    (false, false) => egui::Color32::from_rgb(20, 125, 65),
                };
                let content = ui.horizontal_centered(|ui| {
                    window_buttons(ui, ctx, app, NORMAL_CONTENT_H);
                    ui.vertical(|ui| {
                        ui.set_max_width(360.0);
                        ui.label(
                            egui::RichText::new(if warning {
                                "集/卫 · 请准备结束游玩"
                            } else {
                                "集/卫 · 检测到休眠空档"
                            })
                            .color(color)
                            .strong(),
                        );
                        ui.label("距离 Mower 预计接管");
                        ui.label(
                            egui::RichText::new(format!(
                                "{:02}:{:02}:{:02}",
                                seconds / 3600,
                                seconds / 60 % 60,
                                seconds % 60
                            ))
                            .size(50.0)
                            .monospace()
                            .color(color),
                        );
                        ui.add(
                            egui::Label::new(format!(
                                "{} · {}",
                                task.current_time.format("%H:%M:%S"),
                                task.current_name
                            ))
                            .truncate(),
                        );
                        if ui.small_button("设置").clicked() {
                            app.cancel_auto_connect();
                            app.state = AppState::Configuration;
                        }
                    });
                });
                return Some(content.response.rect.size());
            }
        }
    }
    None
}
