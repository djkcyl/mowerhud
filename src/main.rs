#![windows_subsystem = "windows"]

mod api;
mod app;
mod config;
mod discovery;
mod leisure;
mod model;
mod notify;
#[cfg(windows)]
mod tray;
mod ui;

use app::MowerHUD;
use eframe::egui;

#[tokio::main]
async fn main() -> Result<(), eframe::Error> {
    // 读取上次记忆的窗口位置（若有），用于恢复到原处
    let saved = config::load_config().ok();

    let mut viewport = egui::ViewportBuilder::default()
        // 启动即配置界面，初始尺寸贴近配置界面，避免开屏时从 HUD 尺寸跳变
        .with_inner_size([360.0, 440.0])
        // 下限取得足够小：HUD 宽高按内容自适应（可窄到 ~180），否则在部分平台/高 DPI 下
        // 程序设置的 InnerSize 会被 winit 钳到 min_inner_size，导致又留白。
        .with_min_inner_size([180.0, 48.0])
        .with_max_inner_size([1000.0, 700.0])
        .with_resizable(false)
        .with_maximized(false)
        .with_always_on_top()
        .with_taskbar(false) // overlay/隐身：不进任务栏与 Alt-Tab
        .with_decorations(false); // 隐藏标题栏

    // 恢复上次记忆的窗口位置（配置界面即在此处打开）。隐身只在运行态用
    // Visible(false) 实现，启动时一律显示配置界面，无需屏幕外起步的歪招。
    if let Some(cfg) = &saved
        && let (Some(x), Some(y)) = (cfg.window_x, cfg.window_y)
    {
        viewport = viewport.with_position([x, y]);
    }

    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "MowerHUD",
        options,
        Box::new(|cc| Ok(Box::new(MowerHUD::new(cc)))),
    )
}
