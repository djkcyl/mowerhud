//! 配置界面：自动发现 / 填写地址 / 鉴权，测试连接并保存启动。

use crate::app::{AppState, MowerHUD};
use crate::config::DisplayMode;
use crate::discovery::ScanState;

/// 绘制配置界面，返回内容实际高度（供外层把窗口高度调整到刚好容纳内容）。
pub fn show(app: &mut MowerHUD, ui: &mut egui::Ui) -> f32 {
    let ctx = ui.ctx().clone();

    let resp = egui::CentralPanel::default().show_inside(ui, |ui| {
        let left_margin = 20.0;

        // 顶部控制栏（可拖动）
        let title_bar_rect = {
            let mut rect = ui.available_rect_before_wrap();
            rect.set_height(30.0);
            rect
        };

        let title_bar_response = ui.allocate_rect(title_bar_rect, egui::Sense::click_and_drag());

        // egui 已判定为拖拽，直接发起系统窗口拖动
        if title_bar_response.dragged() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        // 在拖动区域上绘制控制栏内容
        ui.scope_builder(egui::UiBuilder::new().max_rect(title_bar_rect), |ui| {
            ui.horizontal(|ui| {
                ui.add_space(left_margin);
                ui.heading("MowerHUD");
                ui.add_space(5.0);
                ui.label(
                    egui::RichText::new("by A60")
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(10.0);
                    // 关闭按钮（不放最小化：无任务栏/不进 Alt-Tab 的窗口最小化后无法找回）
                    if ui.add_sized([30.0, 20.0], egui::Button::new("X")).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        });

        ui.add_space(8.0);

        // 标题区域
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("连接设置").size(16.0).strong());
            ui.add_space(3.0);
            ui.label("选择本地实例，或手动填写连接信息");
        });

        ui.add_space(12.0);

        // 地址行
        ui.horizontal(|ui| {
            ui.add_space(left_margin);
            egui::Grid::new("main_config_grid")
                .num_columns(2)
                .spacing([15.0, 8.0])
                .striped(false)
                .show(ui, |ui| {
                    ui.label("地址:");
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        // 协议切换按钮
                        let protocol_text = if app.temp_use_https { "https" } else { "http" };
                        if ui
                            .add_sized([45.0, 18.0], egui::Button::new(protocol_text))
                            .clicked()
                        {
                            app.temp_use_https = !app.temp_use_https;
                            app.cancel_auto_connect();
                        }
                        ui.label("://");
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut app.temp_address)
                                    .hint_text("127.0.0.1 或 127.0.0.1:3289")
                                    .desired_width(190.0),
                            )
                            .changed()
                        {
                            app.cancel_auto_connect();
                        }
                    });
                    ui.end_row();
                });
        });

        ui.add_space(10.0);

        // 自动发现的本地 Mower 实例
        ui.horizontal(|ui| {
            ui.add_space(left_margin);
            ui.label("本地 Mower:");
            if ui
                .add_sized([70.0, 18.0], egui::Button::new("重新扫描"))
                .clicked()
            {
                app.start_scan();
            }
        });
        ui.add_space(4.0);

        // 克隆出扫描结果后立即释放锁，避免渲染期间持锁
        let scan = app.scan_state.try_lock().ok().map(|g| g.clone());
        match scan {
            Some(ScanState::Scanning) => {
                ui.horizontal(|ui| {
                    ui.add_space(left_margin);
                    ui.spinner();
                    ui.add_space(5.0);
                    ui.label(
                        egui::RichText::new("正在扫描本地端口...")
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    );
                });
            }
            Some(ScanState::Done(list)) if !list.is_empty() => {
                // 实例较多时滚动，保持窗口高度有界
                egui::ScrollArea::vertical()
                    .id_salt("mower_scan_list")
                    .max_height(84.0)
                    .show(ui, |ui| {
                        for inst in &list {
                            let mut label = format!("127.0.0.1:{}", inst.port);
                            if !inst.name.is_empty() {
                                label.push_str(" · ");
                                label.push_str(&inst.name);
                            }
                            if inst.local_id.is_some() {
                                label.push_str(" · 无需 Token");
                            }
                            let st = inst.status.label();
                            if !st.is_empty() {
                                label.push_str(" · ");
                                label.push_str(st);
                            }
                            if inst.requires_token {
                                label.push_str(" · 需要 Token");
                            }
                            if let Some(t) = &inst.next_task_time {
                                // 只取时间部分，避免按钮过宽
                                let short = t.rsplit(' ').next().unwrap_or(t);
                                label.push_str(" · 下个 ");
                                label.push_str(short);
                            }
                            ui.horizontal(|ui| {
                                ui.add_space(left_margin);
                                if ui
                                    .add_sized([290.0, 20.0], egui::Button::new(label))
                                    .clicked()
                                {
                                    app.select_instance(inst);
                                    if inst.local_id.is_some() && app.save_and_start() {
                                        app.state = AppState::Running;
                                        app.start_task_fetching();
                                    }
                                }
                            });
                        }
                    });
            }
            Some(ScanState::Done(_)) => {
                ui.horizontal(|ui| {
                    ui.add_space(left_margin);
                    ui.label(
                        egui::RichText::new("未发现本地实例，可手动填写地址")
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    );
                });
            }
            None => {}
        }

        ui.add_space(10.0);

        // Token 为 Mower 自带，用户名/密码为反向代理的 Basic 认证
        ui.horizontal(|ui| {
            ui.add_space(left_margin);
            ui.label("手动连接鉴权");
        });

        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.add_space(left_margin);
            egui::Grid::new("auth_grid")
                .num_columns(2)
                .spacing([15.0, 8.0])
                .striped(false)
                .show(ui, |ui| {
                    ui.label("用户名:");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut app.temp_username).desired_width(258.0),
                        )
                        .changed()
                    {
                        app.cancel_auto_connect();
                    }
                    ui.end_row();

                    ui.label("密码:");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut app.temp_password)
                                .password(true)
                                .desired_width(258.0),
                        )
                        .changed()
                    {
                        app.cancel_auto_connect();
                    }
                    ui.end_row();

                    ui.label("Token:");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut app.temp_token)
                                .password(true)
                                .hint_text("Mower 实例 Token")
                                .desired_width(258.0),
                        )
                        .changed()
                    {
                        app.cancel_auto_connect();
                    }
                    ui.end_row();
                });
        });

        ui.add_space(10.0);

        // 提醒方式开关（通知 / 提示音相互独立）
        ui.horizontal(|ui| {
            ui.add_space(left_margin);
            ui.checkbox(&mut app.temp_notify_enabled, "启用通知");
            ui.add_space(15.0);
            ui.checkbox(&mut app.temp_sound_enabled, "启用提示音");
        });

        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.add_space(left_margin);
            ui.checkbox(&mut app.temp_leisure.enabled, "集/卫模式");
        });
        if app.temp_leisure.enabled {
            ui.horizontal(|ui| {
                ui.add_space(left_margin);
                ui.label("最短空档");
                ui.add(
                    egui::DragValue::new(&mut app.temp_leisure.min_idle_minutes)
                        .range(2..=1440)
                        .suffix(" 分钟"),
                );
            });
            app.temp_leisure.warning_minutes = app.temp_leisure.warning_minutes.clamp(
                1,
                app.temp_leisure.min_idle_minutes.saturating_sub(1).max(1),
            );
            ui.horizontal(|ui| {
                ui.add_space(left_margin);
                ui.label("提前提醒");
                let maximum = app.temp_leisure.min_idle_minutes.saturating_sub(1).max(1);
                ui.add(
                    egui::DragValue::new(&mut app.temp_leisure.warning_minutes)
                        .range(1..=maximum)
                        .suffix(" 分钟"),
                );
            });
            ui.label("观察到运行转为休眠后判断空档；接管时间为估计。");
            ui.label("触发后显示悬浮窗；模式内使用上述提前提醒。");
            if !app.temp_notify_enabled && !app.temp_sound_enabled {
                ui.colored_label(
                    if ui.visuals().dark_mode {
                        egui::Color32::YELLOW
                    } else {
                        egui::Color32::from_rgb(155, 80, 0)
                    },
                    "通知和提示音均已关闭，届时仅显示倒计时。",
                );
            }
        }

        // 展示模式：普通 / 单列 / 隐身（也可在运行界面右键切换）
        ui.horizontal(|ui| {
            ui.add_space(left_margin);
            ui.label("展示模式:");
            for (mode, label) in [
                (DisplayMode::Normal, "普通"),
                (DisplayMode::NormalSingle, "单列"),
                (DisplayMode::Stealth, "隐身"),
            ] {
                if ui
                    .selectable_label(app.display_mode == mode, label)
                    .clicked()
                {
                    app.set_display_mode(mode);
                }
            }
        });

        ui.add_space(12.0);

        // 连接状态显示
        let status = app.connection_status.lock().ok().map(|s| s.clone());
        if let Some(status) = status.filter(|s| !s.is_empty()) {
            ui.horizontal(|ui| {
                ui.add_space(left_margin);
                ui.label(status);
            });
            ui.add_space(6.0);
        }

        // 按钮区域
        ui.horizontal(|ui| {
            ui.add_space(left_margin);
            if ui
                .add_sized([95.0, 26.0], egui::Button::new("测试连接"))
                .clicked()
            {
                app.test_connection();
            }

            ui.add_space(15.0);

            if ui
                .add_sized([95.0, 26.0], egui::Button::new("保存并启动"))
                .clicked()
                && app.save_and_start()
            {
                app.state = AppState::Running;
                app.start_task_fetching();
            }
        });

        // 返回内容已占用的高度，供外层自适应窗口高度
        ui.min_rect().height()
    });
    resp.inner
}
