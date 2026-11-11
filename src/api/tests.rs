use super::*;
use chrono::Duration as ChronoDuration;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

/// A real local HTTP boundary, using synthetic credentials and alpha responses.
fn serve(responses: Vec<(u16, String)>, expected_token: Option<&str>) -> Config {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let expected_token = expected_token.map(str::to_owned);
    let client_token = expected_token.clone();
    thread::spawn(move || {
        for (status, body) in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 2048];
            while !request.ends_with(b"\r\n\r\n") {
                let count = socket.read(&mut chunk).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
            }
            let headers = String::from_utf8(request).unwrap().to_ascii_lowercase();
            if expected_token.is_none() {
                assert!(!headers.lines().any(|line| line.starts_with("token:")));
            }
            let authorized = expected_token.as_ref().is_none_or(|token| {
                headers
                    .lines()
                    .any(|line| line.trim() == format!("token: {}", token.to_ascii_lowercase()))
            });
            let (status, body) = if authorized {
                (status, body)
            } else {
                (403, "forbidden".to_string())
            };
            write!(
                socket,
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    Config {
        port,
        token: client_token,
        ..Config::default()
    }
}

async fn poll_once(
    config: Config,
    previous: Option<TaskInfo>,
) -> (Option<TaskInfo>, Option<String>, MowerStatus) {
    let waits_for_clear = previous.is_some();
    let task = Arc::new(Mutex::new(previous));
    let error = Arc::new(Mutex::new(None));
    let status = Arc::new(Mutex::new(MowerStatus::Unknown));
    let handle = tokio::spawn(fetch_state_loop(
        task.clone(),
        error.clone(),
        status.clone(),
        config,
    ));
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let task_available = task.lock().unwrap().is_some();
            let has_error = error.lock().unwrap().is_some();
            if task_available != waits_for_clear || has_error {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("轮询未在预期时间内更新任务");
    handle.abort();
    let task_snapshot = task.lock().unwrap().clone();
    let error_snapshot = error.lock().unwrap().clone();
    let status_snapshot = *status.lock().unwrap();
    (task_snapshot, error_snapshot, status_snapshot)
}

fn alpha_task(
    time: DateTime<Local>,
    type_name: &str,
    display: &str,
    priority: i64,
) -> serde_json::Value {
    json!({
        "time": time.format("%Y-%m-%dT%H:%M:%S%.6f").to_string(),
        "plan": {},
        "type": {"_value_": type_name, "display_value": display, "priority": priority},
        "meta_data": "",
        "display_name": display,
        "presentation": {"title": display, "rooms": [], "targets": [], "actions": [], "backups": []}
    })
}

#[tokio::test]
async fn alpha_starting_and_recovering_are_visible_states() {
    for (state, expected) in [("starting", "启动中"), ("recovering", "恢复中")] {
        let config = serve(vec![(200, json!({"status": state}).to_string())], None);
        assert_eq!(
            fetch_status(&build_client(), &config)
                .await
                .unwrap()
                .label(),
            expected
        );
    }
}

#[tokio::test]
async fn alpha_queue_uses_server_order_and_task_presentation() {
    let now = Local::now();
    let mut first = alpha_task(now - ChronoDuration::minutes(10), "run_order", "跑单", 1);
    first["display_name"] = json!("恢复原班");
    first["plan"] = json!({"room_2_1": ["但书"]});
    first["presentation"] = json!({"title":"恢复原班", "rooms":[{"id":"room_2_1","label":"贸易站 B201","agents":["但书"]}], "targets":[], "actions":[], "backups":[]});
    let mut second = alpha_task(now - ChronoDuration::minutes(1), "技能专精", "技能专精", 2);
    second["presentation"]["title"] = json!("自动专精");
    let third = alpha_task(now + ChronoDuration::minutes(5), "mail", "领取邮件", 2);
    let config = serve(
        vec![
            (200, json!({"status":"working"}).to_string()),
            (200, json!([first, second, third]).to_string()),
        ],
        Some("test-token"),
    );
    let (task, error, _) = poll_once(config, None).await;
    assert!(error.is_none(), "{error:?}");
    let task = task.unwrap();
    assert_eq!(task.current_name, "恢复原班");
    assert_eq!(task.current_rooms.as_deref(), Some("B201"));
    assert_eq!(task.next_task.unwrap().name, "自动专精");
}

#[tokio::test]
async fn local_service_supplies_status_and_tasks_without_token() {
    let task = alpha_task(
        Local::now() + ChronoDuration::minutes(5),
        "mail",
        "领取邮件",
        2,
    );
    let snapshot = json!({
        "protocol":"mowerhud/1", "id":"local-one", "name":"",
        "mower_port":58000, "status":"sleeping", "next_task_time":null,
        "tasks":[task]
    });
    let server_config = serve(vec![(200, snapshot.to_string())], None);
    let mut value = serde_json::to_value(&server_config).unwrap();
    value["local_instance_id"] = json!("local-one");
    value["local_hud_port"] = json!(server_config.port);
    let config: Config = serde_json::from_value(value).unwrap();
    let (task, error, status) = poll_once(config, None).await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(status, MowerStatus::Sleeping);
    assert_eq!(task.unwrap().current_name, "领取邮件");
}

#[tokio::test]
async fn local_service_does_not_accept_a_different_instance() {
    let snapshot = json!({
        "protocol":"mowerhud/1", "id":"other-instance", "name":"",
        "mower_port":58000, "status":"sleeping", "next_task_time":null,
        "tasks":[]
    });
    let server_config = serve(vec![(200, snapshot.to_string())], None);
    let config = Config {
        local_instance_id: Some("selected-instance".into()),
        local_hud_port: Some(server_config.port),
        ..server_config
    };
    let (task, error, _) = poll_once(config, None).await;
    assert!(task.is_none());
    assert!(error.unwrap().contains("实例已改变"));
}

#[tokio::test]
async fn empty_alpha_queue_clears_stale_task_without_marking_offline() {
    let now = Local::now();
    let previous = TaskInfo {
        current_name: "旧任务".to_string(),
        current_time: now,
        current_countdown: String::new(),
        is_expired: false,
        remaining_seconds: 1,
        current_rooms: None,
        next_task: None,
    };
    let config = serve(
        vec![
            (200, json!({"status":"starting"}).to_string()),
            (200, "[]".into()),
        ],
        None,
    );
    let (task, error, _) = poll_once(config, Some(previous)).await;
    assert!(task.is_none(), "空队列不能继续显示旧任务");
    assert!(error.is_none(), "空队列不代表离线: {error:?}");
}

#[tokio::test]
async fn authentication_failure_explains_that_token_is_required() {
    let config = serve(vec![(403, "forbidden".into())], None);
    let error = fetch_status(&build_client(), &config)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("Token"), "{error}");
}

#[tokio::test]
async fn task_authentication_failure_preserves_previous_task_and_marks_error() {
    let previous = TaskInfo {
        current_name: "上次的任务".into(),
        current_time: Local::now(),
        current_countdown: String::new(),
        is_expired: false,
        remaining_seconds: 0,
        current_rooms: None,
        next_task: None,
    };
    let config = serve(
        vec![
            (200, json!({"status":"working"}).to_string()),
            (403, "forbidden".into()),
        ],
        Some("test-token"),
    );
    let (task, error, _) = poll_once(config, Some(previous)).await;
    assert_eq!(task.unwrap().current_name, "上次的任务");
    assert!(error.unwrap().contains("Token"));
}

#[test]
fn parses_alpha_timestamps_with_or_without_fraction_and_offset() {
    assert!(parse_task_time("2026-11-04T16:05:19").is_some());
    assert!(parse_task_time("2026-11-04T16:05:19.123456").is_some());
    assert!(parse_task_time("2026-11-04 16:05:19").is_some());
    assert_eq!(
        parse_task_time("2026-11-04T16:05:19+08:00"),
        parse_task_time("2026-11-04T08:05:19Z")
    );
}

#[test]
fn legacy_task_without_presentation_uses_metadata() {
    let task: Task = serde_json::from_value(json!({
        "time":"2026-11-04T16:05:19",
        "type":{"display_value":"空任务","priority":2},
        "meta_data":"领取奖励",
        "plan":{}
    }))
    .unwrap();
    assert_eq!(task_label(&task), "领取奖励");
}

#[test]
fn ordinary_shifts_use_a_short_summary_without_changing_special_tasks() {
    for (kind, title, expected) in [
        ("上班", "感知信息上班；Mon3tr、焰尾、森蚺下班", "基建换班"),
        ("下班", "能天使、空弦休息", "基建换班"),
        ("纠错", "甲上班;乙下班", "基建换班"),
        ("空任务", "甲上班；乙休息", "基建换班"),
        ("用尽下班", "甲、乙下班", "用尽下班"),
        ("用尽下班", "轮休协调", "轮休协调"),
        ("上班", "回满上班", "回满上班"),
        ("上班", "甲回满上班；乙下班", "甲回满上班；乙下班"),
        ("跑单", "恢复原班", "恢复原班"),
        ("纠错", "替班交接", "替班交接"),
        ("释放宿舍空位", "释放宿舍空位", "释放宿舍空位"),
        ("肥鸭", "肥鸭 充 能天使", "菲亚梅塔充能"),
        ("趴体", "趴体", "线索交流"),
        ("技能专精", "自动专精", "自动专精"),
        ("领取邮件", "领取邮件", "领取邮件"),
    ] {
        let mut value = alpha_task(Local::now(), "", kind, 2);
        value["presentation"]["title"] = json!(title);
        let task: Task = serde_json::from_value(value).unwrap();
        assert_eq!(task_summary(&task), expected, "{kind}: {title}");
    }
}

#[test]
fn legacy_shifts_and_explicit_full_mood_names_are_preserved() {
    for (kind, display, expected) in [
        ("上班", "上班", "基建换班"),
        ("下班", "下班", "基建换班"),
        ("用尽下班", "甲下班", "用尽下班"),
        ("上班", "回满上班", "回满上班"),
    ] {
        let task: Task = serde_json::from_value(json!({
            "time":"2026-11-04T16:05:19",
            "type":{"display_value":kind,"priority":2},
            "display_name":display
        }))
        .unwrap();
        assert_eq!(task_summary(&task), expected);
    }
    let mut value = alpha_task(Local::now(), "shifit_on", "上班", 2);
    value["display_name"] = json!("回满上班");
    value["presentation"]["title"] = json!("甲上班；乙下班");
    let task: Task = serde_json::from_value(value).unwrap();
    assert_eq!(task_summary(&task), "回满上班");
}

#[test]
fn special_display_names_override_detailed_titles_in_both_task_slots() {
    for (kind, stage, title) in [
        ("用尽下班", "轮休协调", "但书下班"),
        ("纠错", "替班交接", "能天使上班；空弦下班"),
        ("跑单", "恢复原班", "但书上班"),
    ] {
        let mut value = alpha_task(Local::now(), "", kind, 2);
        value["display_name"] = json!(stage);
        value["presentation"]["title"] = json!(title);
        let task: Task = serde_json::from_value(value).unwrap();
        let info = task_info_from_tasks(vec![task.clone(), task])
            .unwrap()
            .unwrap();
        assert_eq!(
            [
                info.current_name.as_str(),
                info.next_task.as_ref().unwrap().name.as_str()
            ],
            [stage, stage],
            "{kind}: {title}"
        );
    }
}

#[tokio::test]
async fn both_existing_transports_summarize_current_and_next_tasks() {
    for local in [false, true] {
        let now = Local::now();
        let mut first = alpha_task(now - ChronoDuration::minutes(1), "shifit_on", "上班", 2);
        first["presentation"]["title"] = json!("感知信息上班；Mon3tr、焰尾下班");
        let mut next = alpha_task(
            now - ChronoDuration::minutes(2),
            "exhaust_on",
            "用尽下班",
            2,
        );
        next["presentation"]["title"] = json!("能天使、空弦下班");
        let tasks = json!([first, next]);
        let mut config = if local {
            serve(
                vec![(
                    200,
                    json!({
                        "protocol":"mowerhud/1", "id":"summary-instance", "mower_port":58000,
                        "status":"sleeping", "tasks":tasks
                    })
                    .to_string(),
                )],
                None,
            )
        } else {
            serve(
                vec![
                    (200, json!({"status":"sleeping"}).to_string()),
                    (200, tasks.to_string()),
                ],
                Some("test-token"),
            )
        };
        if local {
            config.local_instance_id = Some("summary-instance".into());
            config.local_hud_port = Some(config.port);
        }
        let (task, error, status) = poll_once(config, None).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(status, MowerStatus::Sleeping);
        let task = task.unwrap();
        assert_eq!(task.current_name, "基建换班");
        assert!(task.is_expired);
        assert_eq!(task.next_task.unwrap().name, "用尽下班");
    }
}

#[tokio::test]
async fn connection_test_rejects_unrelated_successful_http_service() {
    let config = serve(vec![(200, "<html>unrelated service</html>".into())], None);
    assert!(test_api_connection(&config).await.is_err());
}

/// 用指定的 Mower 实例手动核对真实接口；不参与默认测试。
#[tokio::test]
#[ignore = "需要设置 MOWER_ALPHA_TEST_ADDRESS 和 MOWER_ALPHA_TEST_TOKEN"]
async fn live_alpha_http_contract() {
    let address = std::env::var("MOWER_ALPHA_TEST_ADDRESS").expect("缺少 Mower 地址");
    let token = std::env::var("MOWER_ALPHA_TEST_TOKEN").expect("缺少 Mower Token");
    let mut config = Config::from_address(&address, false, "", "").unwrap();
    config.token = Some(token);
    let client = build_client();
    let status = fetch_status(&client, &config).await.unwrap();
    assert_ne!(status, MowerStatus::Unknown);
    let task = fetch_earliest_task(&client, &config).await.unwrap();
    if let Ok(expected) = std::env::var("MOWER_ALPHA_TEST_FIRST_TASK") {
        assert_eq!(task.unwrap().current_name, expected);
    }
}

#[tokio::test]
#[ignore = "需要本地 MowerHUD 联调服务"]
async fn live_local_hud_reconnection() {
    let port = std::env::var("MOWER_HUD_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let id = std::env::var("MOWER_HUD_TEST_ID").unwrap();
    let marker = std::env::var("MOWER_HUD_TEST_MARKER").unwrap();
    let config = Config {
        local_instance_id: Some(id),
        local_hud_port: Some(port),
        ..Config::default()
    };
    let task = Arc::new(Mutex::new(None));
    let error = Arc::new(Mutex::new(None));
    let status = Arc::new(Mutex::new(MowerStatus::Unknown));
    let handle = tokio::spawn(fetch_state_loop(
        task.clone(),
        error.clone(),
        status,
        config,
    ));
    async fn wait_for(task: &Arc<Mutex<Option<TaskInfo>>>, name: &str) {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if task
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|task| task.current_name == name)
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("等待本地 Mower 任务超时");
    }
    wait_for(&task, "刷新跑单时间 · 贸易站 B201").await;
    std::fs::write(marker, b"ready").unwrap();
    wait_for(&task, "领取邮件").await;
    assert!(error.lock().unwrap().is_none());
    handle.abort();
}
