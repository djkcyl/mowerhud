use super::*;

fn hud() -> MowerHUD {
    let config = Config {
        notify_enabled: false,
        sound_enabled: false,
        leisure: LeisureConfig {
            enabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    MowerHUD {
        leisure: LeisureState::default(),
        leisure_popup: false,
        temp_leisure: config.leisure.clone(),
        state: AppState::Running,
        temp_address: String::new(),
        temp_use_https: false,
        temp_username: String::new(),
        temp_password: String::new(),
        temp_token: String::new(),
        temp_notify_enabled: false,
        temp_sound_enabled: false,
        config,
        task_info: Arc::new(Mutex::new(Some(TaskInfo {
            current_name: "基建换班".into(),
            current_time: Local::now() + chrono::Duration::minutes(60),
            current_countdown: "01:00:00".into(),
            is_expired: false,
            remaining_seconds: 3600,
            current_rooms: None,
            next_task: None,
        }))),
        task_error: Arc::new(Mutex::new(None)),
        mower_status: Arc::new(Mutex::new(MowerStatus::Working)),
        tray_status: Arc::new(Mutex::new(String::new())),
        tray_mode: Arc::new(AtomicU8::new(0)),
        last_update: Instant::now() - Duration::from_secs(2),
        connection_status: Arc::new(Mutex::new(String::new())),
        scan_state: Arc::new(Mutex::new(ScanState::Done(Vec::new()))),
        selected_local: None,
        auto_connect_pending: false,
        auto_select_single: false,
        next_auto_scan: Instant::now(),
        always_on_top: true,
        notified_2min: false,
        notified_30sec: false,
        last_task_time: None,
        last_sent_size: None,
        last_saved_pos: None,
        in_rainbow_window: false,
        display_mode: DisplayMode::Normal,
        last_visible: None,
        #[cfg(windows)]
        tray: None,
        fetch_task: None,
    }
}

#[test]
fn clicking_leisure_popup_does_not_start_native_window_drag() {
    let mut app = hud();
    app.tick_leisure();
    *app.mower_status.lock().unwrap() = MowerStatus::Sleeping;
    app.tick_leisure();
    let context = egui::Context::default();
    context.all_styles_mut(|style| style.interaction.selectable_labels = false);
    let pos = egui::pos2(100.0, 70.0);
    for events in [
        vec![],
        vec![egui::Event::PointerMoved(pos)],
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }],
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    ] {
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(280.0, 160.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                crate::ui::task_view::show(&mut app, ui);
            },
        );
        assert!(
            !output.viewport_output.values().any(|viewport| viewport
                .commands
                .contains(&egui::ViewportCommand::StartDrag)),
            "a stationary click on the popup must not enter the native move loop"
        );
    }
}

#[test]
fn dragging_leisure_popup_starts_native_drag_once_and_stops_on_release() {
    let mut app = hud();
    app.tick_leisure();
    *app.mower_status.lock().unwrap() = MowerStatus::Sleeping;
    app.tick_leisure();
    let context = egui::Context::default();
    context.all_styles_mut(|style| style.interaction.selectable_labels = false);
    let start = egui::pos2(100.0, 70.0);
    let moved = egui::pos2(120.0, 80.0);
    let frames = [
        (vec![], 0),
        (vec![egui::Event::PointerMoved(start)], 0),
        (
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            0,
        ),
        (vec![egui::Event::PointerMoved(moved)], 1),
        (vec![egui::Event::PointerMoved(egui::pos2(125.0, 85.0))], 0),
        (
            vec![egui::Event::PointerButton {
                pos: moved,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            0,
        ),
        (vec![], 0),
    ];
    for (events, expected_drags) in frames {
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(280.0, 160.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                crate::ui::task_view::show(&mut app, ui);
            },
        );
        let drags = output
            .viewport_output
            .values()
            .flat_map(|v| &v.commands)
            .filter(|cmd| **cmd == egui::ViewportCommand::StartDrag)
            .count();
        assert_eq!(
            drags, expected_drags,
            "only a new intentional drag may enter the native move loop"
        );
    }
}

#[cfg(windows)]
#[test]
fn leisure_popup_settings_button_remains_clickable() {
    let mut app = hud();
    app.tick_leisure();
    *app.mower_status.lock().unwrap() = MowerStatus::Sleeping;
    app.tick_leisure();
    let context = egui::Context::default();
    crate::ui::fonts::setup_chinese_fonts(&context);
    context.all_styles_mut(|style| style.interaction.selectable_labels = false);
    let render = |app: &mut MowerHUD, events| {
        context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(320.0, 180.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                crate::ui::task_view::show(app, ui);
            },
        )
    };
    let output = render(&mut app, vec![]);
    let pos = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "设置" => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .expect("popup must render a settings button");
    for events in [
        vec![egui::Event::PointerMoved(pos)],
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }],
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    ] {
        let output = render(&mut app, events);
        assert!(
            !output
                .viewport_output
                .values()
                .any(|v| v.commands.contains(&egui::ViewportCommand::StartDrag))
        );
    }
    assert!(matches!(app.state, AppState::Configuration));
}

#[test]
fn snapshot_lock_contention_preserves_active_leisure_window() {
    for field in ["status", "error", "task"] {
        let mut app = hud();
        app.tick_leisure();
        *app.mower_status.lock().unwrap() = MowerStatus::Sleeping;
        app.tick_leisure();
        assert!(app.leisure.active && app.leisure_popup);
        let status = app.mower_status.clone();
        let error = app.task_error.clone();
        let task = app.task_info.clone();
        let status_guard = (field == "status").then(|| status.lock().unwrap());
        let error_guard = (field == "error").then(|| error.lock().unwrap());
        let task_guard = (field == "task").then(|| task.lock().unwrap());
        app.tick_leisure();
        assert!(
            app.leisure.active && app.leisure_popup,
            "{field} lock contention must not change the window"
        );
        drop((status_guard, error_guard, task_guard));
        app.tick_leisure();
        assert!(app.leisure.active && app.leisure_popup);
    }
}

#[test]
fn readable_empty_queue_still_ends_active_leisure_window() {
    let mut app = hud();
    app.tick_leisure();
    *app.mower_status.lock().unwrap() = MowerStatus::Sleeping;
    app.tick_leisure();
    assert!(app.leisure.active && app.leisure_popup);
    *app.task_info.lock().unwrap() = None;
    app.tick_leisure();
    assert!(!app.leisure.active && !app.leisure_popup);
}

fn start_sleep_window(app: &mut MowerHUD) {
    let task = app.task_info.lock().unwrap().clone();
    app.start_task_fetching();
    *app.task_info.lock().unwrap() = task;
    *app.mower_status.lock().unwrap() = MowerStatus::Working;
    app.tick_leisure();
    *app.mower_status.lock().unwrap() = MowerStatus::Sleeping;
    app.tick_leisure();
    assert!(app.leisure.active && app.leisure_popup);
}

#[tokio::test]
async fn reminder_only_changes_keep_polling_and_current_sleep_window() {
    let mut app = hud();
    start_sleep_window(&mut app);
    let tasks = app.task_info.clone();
    app.config.sound_enabled = true;
    app.config.leisure.warning_minutes = 7;
    app.start_task_fetching();
    assert!(
        app.leisure.active && app.leisure_popup,
        "saving reminders must not discard the sleep window"
    );
    assert!(
        Arc::ptr_eq(&tasks, &app.task_info),
        "unchanged connection must keep its poller"
    );
    app.config.sound_enabled = false;
    app.task_info.lock().unwrap().as_mut().unwrap().current_time =
        Local::now() + chrono::Duration::minutes(6);
    app.tick_leisure();
    assert!(app.leisure.active);
    assert_eq!(
        app.leisure
            .update(&app.config.leisure, MowerStatus::Sleeping, true, Some(360)),
        None,
        "saved warning threshold must still fire once in this window"
    );
    app.task_info.lock().unwrap().as_mut().unwrap().current_time =
        Local::now() - chrono::Duration::seconds(1);
    app.tick_leisure();
    assert!(!app.leisure.active && !app.leisure_popup);
}

#[tokio::test]
async fn changing_connection_still_resets_current_sleep_window() {
    let changes: [fn(&mut Config); 8] = [
        |c| c.host = "192.0.2.1".into(),
        |c| c.port += 1,
        |c| c.use_https = true,
        |c| c.username = Some("other-user".into()),
        |c| c.password = Some("other-password".into()),
        |c| c.token = Some("other-token".into()),
        |c| c.local_instance_id = Some("other-instance".into()),
        |c| c.local_hud_port = Some(58001),
    ];
    for change in changes {
        let mut app = hud();
        start_sleep_window(&mut app);
        let tasks = app.task_info.clone();
        change(&mut app.config);
        app.start_task_fetching();
        assert!(!app.leisure.active && !app.leisure_popup);
        assert!(!Arc::ptr_eq(&tasks, &app.task_info));
    }
}

#[tokio::test]
async fn settings_page_continues_warning_and_deadline_processing() {
    use eframe::App;
    let mut app = hud();
    start_sleep_window(&mut app);
    app.state = AppState::Configuration;
    let ctx = egui::Context::default();
    let mut frame = eframe::Frame::_new_kittest();
    app.task_info.lock().unwrap().as_mut().unwrap().current_time =
        Local::now() + chrono::Duration::minutes(2);
    app.last_update = Instant::now() - Duration::from_secs(2);
    app.logic(&ctx, &mut frame);
    assert_eq!(
        app.leisure
            .update(&app.config.leisure, MowerStatus::Sleeping, true, Some(120)),
        None,
        "settings page must already have processed the warning"
    );
    app.task_info.lock().unwrap().as_mut().unwrap().current_time =
        Local::now() - chrono::Duration::seconds(1);
    app.last_update = Instant::now() - Duration::from_secs(2);
    app.logic(&ctx, &mut frame);
    assert!(
        !app.leisure.active && !app.leisure_popup,
        "deadline must be processed without leaving settings"
    );
    assert!(matches!(app.state, AppState::Configuration));
}
