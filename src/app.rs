//! 应用状态机：持有全局状态、驱动每帧逻辑、桥接 UI 与后台任务。

use crate::api;
use crate::config::{Config, DisplayMode, load_config, save_config};
use crate::discovery::{self, MowerInstance, ScanState};
use crate::leisure::{Alert, LeisureConfig, LeisureState};
use crate::model::{MowerStatus, TaskInfo, calculate_countdown_with_status};
use crate::notify::{play_beep_sound, send_notification};
use crate::ui;
use chrono::{DateTime, Local};
use eframe::egui;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 提前 2 分钟提醒的阈值（秒）。
const NOTIFY_AHEAD_2MIN: i64 = 120;
/// 提前 30 秒提醒的阈值（秒）。
const NOTIFY_AHEAD_30SEC: i64 = 30;

/// 应用所处界面。
pub enum AppState {
    Configuration,
    Running,
}

/// 按开关发送提醒：通知与提示音相互独立。
fn fire_alert(notify_enabled: bool, sound_enabled: bool, task_name: &str, message: &str) {
    if notify_enabled {
        send_notification(task_name, message);
    }
    if sound_enabled {
        play_beep_sound();
    }
}

pub struct MowerHUD {
    pub leisure: LeisureState,
    leisure_popup: bool,
    pub temp_leisure: LeisureConfig,
    pub state: AppState,
    pub config: Config,
    pub task_info: Arc<Mutex<Option<TaskInfo>>>,
    pub task_error: Arc<Mutex<Option<String>>>,
    /// mower 运行状态（来自 `/status`）。
    pub mower_status: Arc<Mutex<MowerStatus>>,
    /// 托盘菜单状态项的文本：app 每帧据当前状态/任务/倒计时格式化写入，
    /// 托盘线程每秒读出刷新——这样隐身时右键托盘也能看状态。
    tray_status: Arc<Mutex<String>>,
    /// 当前展示模式给托盘线程（0=普通 1=单列 2=隐身），用于在菜单里标 `●`。
    tray_mode: Arc<AtomicU8>,
    pub last_update: Instant,
    // 配置界面临时字段
    pub temp_address: String,
    pub temp_use_https: bool,
    pub temp_username: String,
    pub temp_password: String,
    pub temp_token: String,
    pub temp_notify_enabled: bool,
    pub temp_sound_enabled: bool,
    pub connection_status: Arc<Mutex<String>>,
    /// 本地 Mower 自动发现的结果。
    pub scan_state: Arc<Mutex<ScanState>>,
    pub selected_local: Option<MowerInstance>,
    auto_connect_pending: bool,
    auto_select_single: bool,
    next_auto_scan: Instant,
    pub always_on_top: bool,
    // 通知状态
    pub notified_2min: bool,
    pub notified_30sec: bool,
    /// 用任务时间（而非名称）标识当前任务，避免同名任务漏报提醒。
    pub last_task_time: Option<DateTime<Local>>,
    /// 上次发给系统的窗口尺寸，用于避免每帧重复发送 InnerSize。
    last_sent_size: Option<egui::Vec2>,
    /// 上次存盘的窗口位置，用于仅在位置变化时才写配置。
    last_saved_pos: Option<egui::Pos2>,
    /// 是否处于彩虹动画窗口（剩余 ≤2min）。在 tick() 里更新（不每帧抢锁），
    /// 供 ui() 决定重绘节奏：在内时 ~60fps 流畅动画，其余时间 ~5fps 省 CPU。
    in_rainbow_window: bool,
    /// 当前展示模式（普通/单列/隐身）。
    pub display_mode: DisplayMode,
    /// 上次发给系统的窗口可见性，避免每帧重复发送 Visible 命令。
    last_visible: Option<bool>,
    /// 隐身模式的系统托盘（仅 Windows，创建失败为 None）。持有以保活。
    #[cfg(windows)]
    tray: Option<crate::tray::Tray>,
    /// 当前后台轮询任务句柄。重新启动时先 abort 上一个，避免多个 loop 抢写共享状态。
    fetch_task: Option<tokio::task::JoinHandle<()>>,
}

impl MowerHUD {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        ui::fonts::setup_chinese_fonts(&cc.egui_ctx);
        // 关闭 label 文本选择：HUD 上拖动窗口时不应把文字选中
        cc.egui_ctx
            .all_styles_mut(|s| s.interaction.selectable_labels = false);

        let stored_config = load_config().ok();
        let first_start = stored_config.is_none();
        let config = stored_config.unwrap_or_default();
        let auto_connect_pending = config.auto_connect_to_local(first_start);
        let auto_select_single = auto_connect_pending && config.local_instance_id.is_none();
        let config_display_mode = config.display_mode;

        // 共享状态先建出来，好同时交给结构体和托盘线程（托盘需要读状态文本）。
        let task_info = Arc::new(Mutex::new(None));
        let task_error = Arc::new(Mutex::new(None));
        let mower_status = Arc::new(Mutex::new(MowerStatus::Unknown));
        let tray_status = Arc::new(Mutex::new("加载中…".to_string()));
        let tray_mode = Arc::new(AtomicU8::new(config_display_mode as u8));

        #[cfg(windows)]
        let tray =
            crate::tray::Tray::new(cc.egui_ctx.clone(), tray_status.clone(), tray_mode.clone());

        let hud = Self {
            leisure: LeisureState::default(),
            leisure_popup: false,
            temp_leisure: config.leisure.clone(),
            state: AppState::Configuration,
            temp_address: format!("{}:{}", config.host, config.port),
            temp_use_https: config.use_https,
            temp_username: config.username.clone().unwrap_or_default(),
            temp_password: config.password.clone().unwrap_or_default(),
            temp_token: config.token.clone().unwrap_or_default(),
            temp_notify_enabled: config.notify_enabled,
            temp_sound_enabled: config.sound_enabled,
            config,
            task_info,
            task_error,
            mower_status,
            tray_status,
            tray_mode,
            last_update: Instant::now(),
            connection_status: Arc::new(Mutex::new(String::new())),
            scan_state: Arc::new(Mutex::new(ScanState::Scanning)),
            selected_local: None,
            auto_connect_pending,
            auto_select_single,
            next_auto_scan: Instant::now() + Duration::from_secs(5),
            always_on_top: true,
            notified_2min: false,
            notified_30sec: false,
            last_task_time: None,
            last_sent_size: None,
            last_saved_pos: None,
            in_rainbow_window: false,
            display_mode: config_display_mode,
            last_visible: None,
            #[cfg(windows)]
            tray,
            fetch_task: None,
        };
        // 启动即在后台自动发现本地 Mower 实例
        hud.start_scan();
        hud
    }

    /// 切换展示模式：更新状态、存盘。窗口可见性/尺寸由 ui() 每帧据此调整。
    pub fn set_display_mode(&mut self, mode: DisplayMode) {
        self.leisure_popup = false;
        if self.display_mode == mode {
            return;
        }
        self.display_mode = mode;
        self.config.display_mode = mode;
        let _ = save_config(&self.config);
    }

    /// 后台扫描本地 Mower 实例，结果写入 `scan_state`。
    pub fn start_scan(&self) {
        let scan_state = self.scan_state.clone();
        if let Ok(mut guard) = scan_state.lock() {
            *guard = ScanState::Scanning;
        }
        tokio::spawn(async move {
            let found = discovery::scan_local_mowers().await;
            if let Ok(mut guard) = scan_state.lock() {
                *guard = ScanState::Done(found);
            }
        });
    }

    pub fn cancel_auto_connect(&mut self) {
        self.auto_connect_pending = false;
    }

    pub fn select_instance(&mut self, instance: &MowerInstance) {
        self.cancel_auto_connect();
        self.temp_address = format!("127.0.0.1:{}", instance.port);
        self.temp_use_https = false;
        if instance.local_id.is_some() && instance.local_port.is_some() {
            self.temp_username.clear();
            self.temp_password.clear();
            self.temp_token.clear();
            self.selected_local = Some(instance.clone());
        } else {
            self.selected_local = None;
        }
    }

    fn try_auto_connect(&mut self) {
        if !self.auto_connect_pending || !matches!(self.state, AppState::Configuration) {
            return;
        }
        let scan = self.scan_state.try_lock().ok().map(|state| state.clone());
        if let Some(ScanState::Done(instances)) = scan {
            if let Some(instance) =
                discovery::auto_select(&self.config, self.auto_select_single, &instances)
            {
                self.select_instance(instance);
                if self.save_and_start() {
                    self.state = AppState::Running;
                    self.start_task_fetching();
                }
            } else if Instant::now() >= self.next_auto_scan {
                self.start_scan();
                self.next_auto_scan = Instant::now() + Duration::from_secs(5);
            }
        }
    }

    /// 当前 mower 状态（读不到锁时回退 `Unknown`）。
    pub fn current_status(&self) -> MowerStatus {
        self.mower_status
            .try_lock()
            .map(|g| *g)
            .unwrap_or(MowerStatus::Unknown)
    }

    /// 据当前状态/任务/倒计时格式化一行文本，写给托盘菜单的状态项（托盘线程每秒读出刷新）。
    /// 同时把当前模式写给托盘（用于菜单里标 `●`）。仅在内容变化时写。锁竞争时保留上次文本。
    fn update_tray_status(&self) {
        self.tray_mode
            .store(self.display_mode as u8, Ordering::Relaxed);

        let Ok(task_guard) = self.task_info.try_lock() else {
            return;
        };
        let task = task_guard.clone();
        drop(task_guard);

        let status = self
            .mower_status
            .try_lock()
            .map(|g| *g)
            .unwrap_or(MowerStatus::Unknown);
        let offline = self
            .task_error
            .try_lock()
            .map(|g| g.is_some())
            .unwrap_or(false);

        let text = if offline {
            "⚠ 离线（连接失败）".to_string()
        } else if status == MowerStatus::Stopped {
            "Mower 已停止".to_string()
        } else if let Some(t) = &task {
            let label = status.label();
            let prefix = if label.is_empty() {
                String::new()
            } else {
                format!("{label} · ")
            };
            format!("{prefix}{} · {}", t.current_name, t.current_countdown)
        } else if status == MowerStatus::Unknown {
            "加载中…".to_string()
        } else {
            "暂无任务".to_string()
        };

        if let Ok(mut g) = self.tray_status.lock()
            && *g != text
        {
            *g = text;
        }
    }

    /// 仅在窗口位置变化时把它存盘，用于跨重启保持位置。每帧调用，开销可忽略。
    fn maybe_persist_window_pos(&mut self, ctx: &egui::Context) {
        let Some(rect) = ctx.input(|i| i.viewport().outer_rect) else {
            return;
        };
        let pos = rect.min;
        let changed = self
            .last_saved_pos
            .is_none_or(|p| (p.x - pos.x).abs() > 1.0 || (p.y - pos.y).abs() > 1.0);
        if changed {
            self.config.window_x = Some(pos.x);
            self.config.window_y = Some(pos.y);
            self.last_saved_pos = Some(pos);
            let _ = save_config(&self.config);
        }
    }

    /// 启动后台任务轮询。先停掉上一轮，并清掉旧的任务/错误/状态，
    /// 避免「改配置后重启」时新旧两个 loop 同时写共享状态导致 UI 闪烁。
    pub fn start_task_fetching(&mut self) {
        self.leisure = LeisureState::default();
        self.leisure_popup = false;
        if let Some(handle) = self.fetch_task.take() {
            handle.abort();
        }
        // 换一组全新的共享句柄：abort 是协作式的，旧 loop 可能还有在途 HTTP 响应没返回，
        // 取消前它已读到数据就会写回。若沿用同一 Arc，这次「迟到的写」会盖掉刚清空的状态
        // 造成闪一下旧数据。改成换新 Arc 后，旧 loop 只会写到被弃用的旧 Arc，UI 与新 loop
        // 读写新 Arc，互不干扰；旧 Arc 在旧 loop 结束后自然释放。
        self.task_info = Arc::new(Mutex::new(None));
        self.task_error = Arc::new(Mutex::new(None));
        self.mower_status = Arc::new(Mutex::new(MowerStatus::Unknown));

        let task_info = self.task_info.clone();
        let task_error = self.task_error.clone();
        let mower_status = self.mower_status.clone();
        let config = self.config.clone();
        self.fetch_task = Some(tokio::spawn(async move {
            api::fetch_state_loop(task_info, task_error, mower_status, config).await;
        }));
    }

    /// 用临时字段测试 API 连接，结果写入 `connection_status`。
    pub fn test_connection(&mut self) {
        match self.build_config_from_temp() {
            Ok(config) => {
                if let Ok(mut status) = self.connection_status.lock() {
                    *status = "正在测试连接...".to_string();
                }
                let status_clone = self.connection_status.clone();
                tokio::spawn(async move {
                    let msg = match api::test_api_connection(&config).await {
                        Ok(_) => "连接测试成功！".to_string(),
                        Err(e) => format!("连接测试失败: {}", e),
                    };
                    if let Ok(mut status) = status_clone.lock() {
                        *status = msg;
                    }
                });
            }
            Err(e) => {
                if let Ok(mut status) = self.connection_status.lock() {
                    *status = e;
                }
            }
        }
    }

    /// 校验并保存配置；成功返回 true。
    pub fn save_and_start(&mut self) -> bool {
        match self.build_config_from_temp() {
            Ok(config) => {
                self.config = config;
                if let Err(e) = save_config(&self.config) {
                    self.set_status(format!("保存配置失败: {}", e));
                    return false;
                }
                self.set_status("配置已保存".to_string());
                true
            }
            Err(e) => {
                self.set_status(e);
                false
            }
        }
    }

    fn build_config_from_temp(&self) -> Result<Config, String> {
        let mut config = Config::from_address(
            &self.temp_address,
            self.temp_use_https,
            &self.temp_username,
            &self.temp_password,
        )?;
        config.notify_enabled = self.temp_notify_enabled;
        config.leisure = self.temp_leisure.clone();
        if config.leisure.min_idle_minutes < 2
            || config.leisure.min_idle_minutes > 1440
            || config.leisure.warning_minutes == 0
            || config.leisure.warning_minutes >= config.leisure.min_idle_minutes
        {
            return Err("集/卫模式：空档须为 2–1440 分钟，提前提醒须小于空档且至少 1 分钟".into());
        }
        config.sound_enabled = self.temp_sound_enabled;
        let token = self.temp_token.trim();
        config.token = (!token.is_empty()).then(|| token.to_string());
        if let Some(instance) = &self.selected_local
            && self.temp_address.trim() == format!("127.0.0.1:{}", instance.port)
            && !self.temp_use_https
            && config.username.is_none()
            && config.password.is_none()
            && config.token.is_none()
        {
            config.local_instance_id = instance.local_id.clone();
            config.local_hud_port = instance.local_port;
        }
        // 保留已记忆的窗口位置与展示模式（from_address 会重置为默认值）
        config.window_x = self.config.window_x;
        config.window_y = self.config.window_y;
        config.display_mode = self.config.display_mode;
        Ok(config)
    }

    fn set_status(&self, msg: String) {
        if let Ok(mut status) = self.connection_status.lock() {
            *status = msg;
        }
    }

    /// 仅在窗口尺寸发生变化时才请求调整，避免每帧抖动。
    fn request_window_size(&mut self, ctx: &egui::Context, size: egui::Vec2) {
        if self.last_sent_size != Some(size) {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            self.last_sent_size = Some(size);
        }
    }

    pub fn set_leisure_enabled(&mut self, enabled: bool) {
        self.config.leisure.enabled = enabled;
        self.temp_leisure = self.config.leisure.clone();
        self.leisure = LeisureState::default();
        self.leisure_popup = false;
        if let Err(error) = save_config(&self.config) {
            self.set_status(format!("保存模式设置失败: {error}"));
        }
    }

    fn tick_leisure(&mut self) {
        let status = self.current_status();
        let online = self
            .task_error
            .try_lock()
            .map(|error| error.is_none())
            .unwrap_or(false);
        let task = self.task_info.try_lock().ok().and_then(|task| task.clone());
        let remaining = task.as_ref().map(|task| {
            task.current_time
                .signed_duration_since(Local::now())
                .num_seconds()
        });
        let alert = self
            .leisure
            .update(&self.config.leisure, status, online, remaining);
        let seconds = remaining.unwrap_or(0).max(0);
        if alert == Some(Alert::Available) {
            self.leisure_popup = true;
        } else if !self.leisure.active {
            self.leisure_popup = false;
        }
        let message = match alert {
            Some(Alert::Available) => format!(
                "检测到休眠，预计距 Mower 接管还有 {} 分钟，可安排集/卫游玩",
                seconds / 60
            ),
            Some(Alert::ReturnSoon) => format!(
                "Mower 预计将在 {} 分 {} 秒后接管，请准备结束游玩",
                seconds / 60,
                seconds % 60
            ),
            Some(Alert::TakingOver) => "Mower 已恢复运行，可能提前接管账号或设备".to_string(),
            Some(Alert::Due) => "预计接管时间已到，请留意 Mower 运行状态".to_string(),
            Some(Alert::Uncertain) => "Mower 状态或连接已改变，预计接管时间暂不可确认".to_string(),
            None => return,
        };
        fire_alert(
            self.config.notify_enabled,
            self.config.sound_enabled,
            "集/卫模式",
            &message,
        );
    }

    /// 每秒推进倒计时并按阈值发送提醒。
    fn tick(&mut self) {
        if self.last_update.elapsed() < Duration::from_secs(1) {
            return;
        }

        self.tick_leisure();
        // 提前取出提醒开关，避免在持有 task_info 锁时再借用 self
        let notify_enabled = self.config.notify_enabled;
        let sound_enabled = self.config.sound_enabled;
        // 离线时不发提醒：任务可能已被重排或取消，无法确认
        let connection_ok = self
            .task_error
            .try_lock()
            .map(|g| g.is_none())
            .unwrap_or(false);

        let Ok(mut guard) = self.task_info.try_lock() else {
            return; // 锁竞争：不消耗 1 秒预算，下一帧重试
        };
        let Some(task) = guard.as_mut() else {
            return;
        };

        // 成功取到任务才推进计时，避免竞争帧白白吃掉 1 秒预算
        self.last_update = Instant::now();

        let (countdown, is_expired, remaining) =
            calculate_countdown_with_status(&task.current_time);
        task.current_countdown = countdown;
        task.is_expired = is_expired;
        task.remaining_seconds = remaining;

        // 缓存彩虹动画窗口状态，供 ui() 决定重绘节奏（避免每帧抢锁判断导致动画卡顿）
        self.in_rainbow_window =
            !is_expired && remaining > 0 && remaining <= ui::theme::RAINBOW_THRESHOLD_SECS;

        // 用任务时间（而非名称）标识边界：同名相邻任务也能正确重置提醒
        if self.last_task_time != Some(task.current_time) {
            self.last_task_time = Some(task.current_time);
            self.notified_2min = false;
            self.notified_30sec = false;
        }

        if !self.config.leisure.enabled && connection_ok && !is_expired && remaining > 0 {
            if !self.notified_2min
                && remaining <= NOTIFY_AHEAD_2MIN
                && remaining > NOTIFY_AHEAD_30SEC
            {
                fire_alert(
                    notify_enabled,
                    sound_enabled,
                    &task.current_name,
                    "还有2分钟开始",
                );
                self.notified_2min = true;
            }
            if !self.notified_30sec && remaining <= NOTIFY_AHEAD_30SEC {
                fire_alert(
                    notify_enabled,
                    sound_enabled,
                    &task.current_name,
                    "还有30秒开始",
                );
                self.notified_30sec = true;
            }
        }

        // 倒计时回到 2 分钟外则重置通知状态，便于下一轮提醒
        if remaining > NOTIFY_AHEAD_2MIN {
            self.notified_2min = false;
            self.notified_30sec = false;
        }

        if let Some(next) = task.next_task.as_mut() {
            let (next_countdown, _, _) = calculate_countdown_with_status(&next.target_time);
            next.countdown = next_countdown;
        }
    }

    /// 隐身用真正的 `Visible(false)` 隐藏窗口（不再用「移到屏幕外」的歪招）。
    /// 之所以能这样：状态逻辑都搬进了 `logic()`，而 `logic()` 即便窗口隐藏也会被 `logic()`
    /// 末尾的 request_repaint_after 定时链驱动照常运行（提醒/托盘均不受影响）；位置由系统
    /// 保留，重新显示即回到原处。仅在可见性变化时发送命令，避免每帧重复。
    fn set_window_visible(&mut self, ctx: &egui::Context, visible: bool) {
        if self.last_visible != Some(visible) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(visible));
            self.last_visible = Some(visible);
        }
    }

    /// 是否应隐藏窗口：仅当选了隐身、且确有托盘能把它找回时才隐藏。否则（托盘创建失败、
    /// 或非 Windows 无托盘）退化为正常显示——避免窗口与托盘双失、彻底无法操作的死局。
    fn should_hide(&self) -> bool {
        if self.config.leisure.enabled && self.leisure.active && self.leisure_popup {
            return false;
        }
        if self.display_mode != DisplayMode::Stealth {
            return false;
        }
        #[cfg(windows)]
        {
            self.tray.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// 处理托盘队列里的指令（显示/隐藏切换、切模式、退出）。
    #[cfg(windows)]
    fn handle_tray_commands(&mut self, ctx: &egui::Context) {
        use crate::tray::TrayCommand;
        let cmds = self.tray.as_ref().map(|t| t.drain()).unwrap_or_default();
        for cmd in cmds {
            match cmd {
                TrayCommand::SetMode(m) => self.set_display_mode(m),
                TrayCommand::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
    }
}

impl eframe::App for MowerHUD {
    /// 所有「状态逻辑」都放这里：托盘指令、窗口可见性/尺寸、倒计时与提醒。`logic()` 每帧都会
    /// 被调用——包括窗口 `Visible(false)` 隐身时（is_visible 仍为 true，详见 ui() 注释），
    /// 这正是隐身下仍能发提醒、并能从托盘切回普通模式的关键。绘制交给 `ui()`。
    /// 末尾用 request_repaint_after 自驱动重绘节奏（见该处注释）。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(windows)]
        self.handle_tray_commands(ctx);

        self.try_auto_connect();

        let interval_ms = match self.state {
            AppState::Configuration => {
                // 配置界面始终可见；尺寸按内容自适应，放在 ui() 里测量后设置。
                self.set_window_visible(ctx, true);
                // 配置界面也可拖动，需在此记忆位置：否则拖动后未进运行态就退出会丢失位置。
                self.maybe_persist_window_pos(ctx);
                200
            }
            AppState::Running => {
                let hidden = self.should_hide();
                self.set_window_visible(ctx, !hidden);

                // 窗口尺寸改由 ui() 测量内容后自适应设置（见 ui()）；这里只管可见性与位置。
                if !hidden {
                    self.maybe_persist_window_pos(ctx);
                }

                // 提醒逻辑：隐身时也要跑，故无条件 tick
                self.tick();

                // 重绘节奏：隐身 ~4fps（够跑提醒，且从托盘切回普通约 ≤250ms 够跟手）；
                // 可见时彩虹 ~30fps、平时 ~5fps。
                if hidden {
                    250
                } else if self.in_rainbow_window {
                    33
                } else {
                    200
                }
            }
        };
        // 刷新托盘菜单状态项的文本（托盘线程每秒读出）。
        self.update_tray_status();

        // 关键：用「主线程帧内」的 request_repaint_after 自驱动重绘节奏。它走 winit 内部的
        // WaitUntil 定时唤醒，可靠且自维持（一帧调度下一帧），且能挺过托盘菜单的模态消息循环。
        // 绝不能依赖后台线程的跨线程 request_repaint——实测在空闲时它根本唤不醒事件循环，
        // 正是「切模式后界面卡住、要鼠标晃一下、连退出都不灵、彩虹只跳一帧」的真正根因。
        ctx.request_repaint_after(Duration::from_millis(interval_ms));
    }

    /// 负责绘制。注意：eframe 0.34 的 wgpu 路径下 `Visible(false)` 并不改写 ViewportInfo 的
    /// minimized/occluded，故 is_visible 仍判为 true，本方法在隐身时**仍会被调用**——所以
    /// 隐身分支直接 return，不对已隐藏的表面做无谓的布局/取锁。状态逻辑一律在 `logic()`。
    /// （eframe 0.34 用 `ui` 取代了已废弃的 `update`。）
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        match self.state {
            AppState::Configuration => {
                let content_h = crate::ui::config_view::show(self, ui);
                // 窗口高度自适应内容（含 CentralPanel 上下边距余量），并取整去抖
                let height = (content_h + 16.0).ceil().max(ui::theme::CONFIG_MIN_HEIGHT);
                self.request_window_size(&ctx, egui::vec2(ui::theme::CONFIG_WIDTH, height));
            }
            AppState::Running => {
                if self.should_hide() {
                    return; // 隐身：窗口已 Visible(false)，无需绘制隐藏表面
                }
                // 渲染并测得内容尺寸，把窗口调到刚好容纳内容（避免右侧/下方留白）。
                let size = crate::ui::task_view::show(self, ui);
                self.request_window_size(&ctx, size);
            }
        }
    }
}

#[cfg(test)]
mod tests;
