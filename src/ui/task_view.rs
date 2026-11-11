//! 运行界面（HUD）：展示当前 / 下一个任务及倒计时，含 mower 状态与离线提示。
//! 支持普通 / 单列两种可见渲染（隐身模式由 app 隐藏窗口，不走这里）。
//!
//! 各渲染分支返回「内容尺寸」，由 [`show`] 加上面板内边距后回传给 app，app 据此把窗口调到刚好
//! 容纳内容——避免右侧/下方留白。

use crate::app::{AppState, MowerHUD};
use crate::config::DisplayMode;
use crate::model::{MowerStatus, TaskInfo};

/// CentralPanel 四周内边距（也是窗口尺寸要在内容之外额外加上的量）。
const PANEL_MARGIN: i8 = 5;
/// 普通/单列模式的内容高度（容纳 50pt 倒计时与按钮列；窗口宽度仍按内容自适应）。
const NORMAL_CONTENT_H: f32 = 62.0;

fn task_title(ui: &mut egui::Ui, title: &str, size: f32) -> egui::Response {
    ui.add(egui::Label::new(egui::RichText::new(title).size(size).strong()).truncate())
}

/// 一行提示文字 + 「配置 / X」按钮，用于「已停止」「离线无数据」「加载中」等无任务态。
/// 用固定宽度（这些是过渡态，不必紧贴内容），返回内容尺寸。
fn show_message_row(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    app: &mut MowerHUD,
    text: &str,
    color: egui::Color32,
) -> egui::Vec2 {
    let size = egui::vec2(340.0, 26.0);
    ui.allocate_ui_with_layout(
        size,
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(text).size(12.0).color(color));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_sized([56.0, 24.0], egui::Button::new("配置"))
                    .clicked()
                {
                    app.state = AppState::Configuration;
                }
                ui.add_space(6.0);
                if ui.add_sized([26.0, 24.0], egui::Button::new("X")).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        },
    );
    size
}

/// 右键菜单：切换展示模式 / 置顶 / 退出。挂在背景拖动区上，任意位置右键可唤出。
fn context_menu(ui: &mut egui::Ui, ctx: &egui::Context, app: &mut MowerHUD) {
    ui.set_min_width(130.0);
    ui.label(
        egui::RichText::new("展示模式")
            .size(11.0)
            .color(egui::Color32::GRAY),
    );
    for (mode, label) in [
        (DisplayMode::Normal, "普通"),
        (DisplayMode::NormalSingle, "单列"),
        (DisplayMode::Stealth, "隐身（托盘）"),
    ] {
        if ui
            .selectable_label(app.display_mode == mode, label)
            .clicked()
        {
            app.set_display_mode(mode);
            ui.close();
        }
    }
    ui.separator();
    if ui.selectable_label(app.always_on_top, "置顶").clicked() {
        app.always_on_top = !app.always_on_top;
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if app.always_on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        }));
        ui.close();
    }
    if ui.button("退出").clicked() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// 绘制 HUD，返回应有的窗口内尺寸（内容尺寸 + 面板内边距）。
pub fn show(app: &mut MowerHUD, ui: &mut egui::Ui) -> egui::Vec2 {
    let ctx = ui.ctx().clone();
    // 用 egui 输入时间驱动彩虹动画（单调递增，且与重绘同步）
    let time_secs = ctx.input(|i| i.time);
    let status = app.current_status();

    let margin = egui::Margin::same(PANEL_MARGIN);
    let frame = egui::Frame::central_panel(ui.style()).inner_margin(margin);
    let inner = egui::CentralPanel::default()
        .frame(frame)
        .show_inside(ui, |ui| {
            // 整个面板作为窗口拖动区（置于内容下层）+ 右键菜单。interact 不占布局空间，不影响测量。
            let bg = ui.interact(
                ui.max_rect(),
                ui.id().with("hud_drag_area"),
                egui::Sense::drag(),
            );
            if bg.dragged() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            bg.context_menu(|ui| context_menu(ui, &ctx, app));

            let error_msg = app.task_error.try_lock().ok().and_then(|g| g.clone());
            // 状态获取失败时，不能把缓存的“已停止”当成当前状态。
            if status == MowerStatus::Stopped && error_msg.is_none() {
                return show_message_row(
                    ui,
                    &ctx,
                    app,
                    "Mower 已停止（未运行）",
                    egui::Color32::from_rgb(255, 180, 120),
                );
            }

            let offline = error_msg.is_some();
            // 克隆任务快照后立即释放锁，便于按模式分函数渲染（且不在渲染期持锁）
            let task = app.task_info.try_lock().ok().and_then(|g| g.clone());

            if let Some(task) = task {
                // 单列：同普通但不画下一个任务那一列（show_next=false）
                let show_next = app.display_mode != DisplayMode::NormalSingle;
                return render_normal(ui, &ctx, app, &task, status, offline, time_secs, show_next);
            }

            // 无任务可展示：离线则显示错误，否则按状态显示加载/暂无
            if let Some(err) = error_msg {
                return show_message_row(
                    ui,
                    &ctx,
                    app,
                    &err,
                    egui::Color32::from_rgb(255, 150, 150),
                );
            }
            let text = match status {
                MowerStatus::Unknown => "正在加载任务数据...",
                MowerStatus::Starting => "Mower 启动中",
                MowerStatus::Recovering => "Mower 恢复中",
                _ => "暂无任务",
            };
            show_message_row(ui, &ctx, app, text, egui::Color32::GRAY)
        });

    let content = inner.inner;
    egui::vec2(
        (content.x + margin.sum().x).max(180.0),
        (content.y + margin.sum().y).max(48.0),
    )
}

/// 竖排窗口按钮列：关闭 / 收进托盘(隐身) / 置顶。普通与单列模式共用。
/// 不放系统最小化：无任务栏/不进 Alt-Tab 的悬浮窗一旦最小化便无法找回。
///
/// `col_h` 为该列应占的高度（= 内容行高）。egui 横向布局不会把「显式分配尺寸的子区域」按
/// 交叉轴居中（只对 label 这类生效），所以这里把列分配成整行高，再用上方留白把按钮组手动居中。
fn window_buttons(ui: &mut egui::Ui, ctx: &egui::Context, app: &mut MowerHUD, col_h: f32) {
    const STACK_H: f32 = 52.0; // 3 × 16px 按钮 + 2 × 2px 间距
    ui.allocate_ui_with_layout(
        egui::vec2(22.0, col_h),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add_space(((col_h - STACK_H) / 2.0).max(0.0));
            if ui
                .add_sized(
                    [20.0, 16.0],
                    egui::Button::new(egui::RichText::new("X").size(12.0)),
                )
                .on_hover_text("关闭")
                .clicked()
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            if ui
                .add_sized(
                    [20.0, 16.0],
                    egui::Button::new(egui::RichText::new("—").size(12.0)),
                )
                .on_hover_text("收进托盘（隐身）")
                .clicked()
            {
                app.set_display_mode(DisplayMode::Stealth);
            }
            // 置顶：▲ 符号（图钉 emoji 在中文字体栈里渲染不出），开启时绿底白字。
            let (bg, fg) = if app.always_on_top {
                (egui::Color32::from_rgb(60, 150, 60), egui::Color32::WHITE)
            } else {
                (egui::Color32::from_rgb(60, 60, 60), egui::Color32::GRAY)
            };
            if ui
                .add_sized(
                    [20.0, 16.0],
                    egui::Button::new(egui::RichText::new("▲").size(11.0).color(fg)).fill(bg),
                )
                .on_hover_text(if app.always_on_top {
                    "取消置顶"
                } else {
                    "置顶"
                })
                .clicked()
            {
                app.always_on_top = !app.always_on_top;
                ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if app.always_on_top {
                    egui::WindowLevel::AlwaysOnTop
                } else {
                    egui::WindowLevel::Normal
                }));
            }
        },
    );
}

/// 普通模式：窗口按钮 + 当前任务卡片 + 大倒计时 +（`show_next` 时）下一个任务。返回内容尺寸。
/// 单列模式即 `show_next=false`：只留左侧当前任务这一列。
#[allow(clippy::too_many_arguments)] // 渲染函数，参数都是必要的展示数据，拆结构体反而更绕
fn render_normal(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    app: &mut MowerHUD,
    task: &TaskInfo,
    status: MowerStatus,
    offline: bool,
    time_secs: f64,
    show_next: bool,
) -> egui::Vec2 {
    let resp = ui.horizontal_centered(|ui| {
        window_buttons(ui, ctx, app, NORMAL_CONTENT_H);
        ui.add_space(10.0);

        // 第一个任务卡片
        ui.allocate_ui_with_layout(
            egui::vec2(132.0, 50.0),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    let (task_status, task_color) = if task.is_expired {
                        ("已到期", egui::Color32::from_rgb(255, 180, 120))
                    } else {
                        ("待执行", egui::Color32::from_rgb(150, 150, 255))
                    };
                    ui.label(
                        egui::RichText::new(task_status)
                            .size(10.0)
                            .color(task_color),
                    );

                    let badge = status.label();
                    if !badge.is_empty() {
                        let badge_color = match status {
                            MowerStatus::Working => egui::Color32::from_rgb(100, 200, 100),
                            MowerStatus::Sleeping => egui::Color32::from_rgb(150, 150, 255),
                            _ => egui::Color32::GRAY,
                        };
                        ui.label(egui::RichText::new(badge).size(10.0).color(badge_color));
                    }
                    if offline {
                        ui.label(
                            egui::RichText::new("离线")
                                .size(10.0)
                                .color(egui::Color32::from_rgb(255, 170, 80)),
                        );
                    }
                });

                task_title(ui, &task.current_name, 15.0);

                if let Some(rooms) = &task.current_rooms {
                    ui.label(
                        egui::RichText::new(format!("贸易站: {}", rooms))
                            .size(9.0)
                            .color(egui::Color32::from_rgb(255, 200, 120)),
                    );
                } else {
                    ui.label(
                        egui::RichText::new(task.current_time.format("%m-%d %H:%M").to_string())
                            .size(9.0)
                            .color(egui::Color32::GRAY),
                    );
                }
            },
        );

        ui.add_space(14.0);

        let countdown_color = crate::ui::theme::get_countdown_color(
            task.remaining_seconds,
            task.is_expired,
            time_secs,
        );
        // 等宽：避免逐秒数字宽度变化导致自适应窗口右边缘抖动。
        ui.label(
            egui::RichText::new(&task.current_countdown)
                .size(50.0)
                .strong()
                .monospace()
                .color(countdown_color),
        );

        if show_next && let Some(ref next) = task.next_task {
            ui.add_space(18.0);
            ui.separator();
            ui.add_space(18.0);

            ui.allocate_ui_with_layout(
                egui::vec2(120.0, 50.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.label(
                        egui::RichText::new("下一个")
                            .size(10.0)
                            .color(egui::Color32::from_rgb(150, 150, 255)),
                    );
                    task_title(ui, &next.name, 13.0);
                    ui.label(
                        egui::RichText::new(next.target_time.format("%m-%d %H:%M").to_string())
                            .size(9.0)
                            .color(egui::Color32::GRAY),
                    );
                },
            );

            ui.add_space(14.0);

            ui.label(
                egui::RichText::new(&next.countdown)
                    .size(26.0)
                    .strong()
                    .monospace()
                    .color(egui::Color32::from_rgb(255, 100, 100)),
            );
        }

        ui.add_space(10.0);
    });
    egui::vec2(resp.response.rect.width(), NORMAL_CONTENT_H)
}

#[cfg(test)]
mod tests {
    #[test]
    fn long_titles_stay_in_one_row_in_both_task_cards() {
        let context = egui::Context::default();
        for (width, size) in [(132.0, 15.0), (120.0, 13.0)] {
            for title in [
                "基建换班".to_string(),
                "感知信息上班；Mon3tr、焰尾、森蚺下班".repeat(20),
                "回满上班\n甲、乙、丙、丁".repeat(20),
            ] {
                let output = context.run_ui(egui::RawInput::default(), |ui| {
                    egui::CentralPanel::default().show_inside(ui, |ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(width, 50.0),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                let response = super::task_title(ui, &title, size);
                                assert!(response.rect.width() <= width + 0.1);
                                assert!(response.rect.height() <= 30.0);
                            },
                        );
                    });
                });
                let text = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) => Some(text),
                        _ => None,
                    })
                    .expect("任务名称应参与实际文字布局");
                assert_eq!(text.galley.rows.len(), 1);
                assert_eq!(text.galley.job.text, title);
                assert_eq!(text.galley.elided, title != "基建换班");
            }
        }
    }
}
