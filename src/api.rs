//! 与割草机 API 的网络交互：拉取状态/任务、测试连接、后台轮询。

use crate::config::Config;
use crate::model::{MowerStatus, NextTaskInfo, TaskInfo, calculate_countdown_with_status};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::interval;

/// 任何线程安全的错误。`Send + Sync` 让它能跨 `async`/`spawn` 边界传递。
type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// API 返回的原始任务结构 —— 只保留我们关心的字段。
#[derive(Deserialize, Debug, Clone)]
struct Task {
    /// ISO 8601 格式时间，例如 `2025-10-09T20:19:40.257970`
    time: String,
    #[serde(rename = "type")]
    task_type: TaskType,
    /// 泛化任务（“空任务”）的真实名称藏在这里。
    #[serde(default)]
    meta_data: String,
    /// 房间 -> 干员列表；跑单任务用它定位贸易站。值的具体形状我们不关心。
    #[serde(default)]
    plan: Option<BTreeMap<String, serde_json::Value>>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    presentation: Option<TaskPresentation>,
}

#[derive(Deserialize, Debug, Clone, Default)]
struct TaskPresentation {
    #[serde(default)]
    title: String,
    #[serde(default)]
    rooms: Vec<PresentationRoom>,
    #[serde(default)]
    targets: Vec<PresentationRoom>,
}

#[derive(Deserialize, Debug, Clone)]
struct PresentationRoom {
    id: String,
    label: String,
}

#[derive(Deserialize, Debug, Clone)]
struct TaskType {
    display_value: String,
    /// 优先级：跑单为 1，其余为 2。
    #[serde(default)]
    priority: i64,
}

/// `/status` 返回体 —— 只取 `status` 字段。
#[derive(Deserialize, Debug)]
struct StatusResponse {
    status: String,
}

#[derive(Deserialize, Debug)]
pub(crate) struct LocalHudSnapshot {
    protocol: String,
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: String,
    pub(crate) mower_port: u16,
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) next_task_time: Option<String>,
    tasks: Vec<Task>,
}

pub(crate) async fn fetch_local_snapshot(
    client: &reqwest::Client,
    port: u16,
) -> Result<LocalHudSnapshot, BoxError> {
    let response = client
        .get(format!("http://127.0.0.1:{port}/mowerhud"))
        .send()
        .await?
        .error_for_status()?;
    let snapshot: LocalHudSnapshot = response.json().await?;
    if snapshot.protocol != "mowerhud/1"
        || snapshot.id.is_empty()
        || snapshot.id.len() > 128
        || snapshot.mower_port == 0
        || snapshot.status.is_empty()
    {
        return Err("本地 MowerHUD 响应格式无效".into());
    }
    Ok(snapshot)
}

/// 构造带超时的客户端，避免挂死连接卡住整个轮询。
fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn build_local_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// 构造 GET 请求，带上 Mower token（HTTP 头 `token`）与可选的 Basic 认证。
///
/// token 是 Mower 自带的鉴权（`config.conf.webview.token`）；Basic 认证是给反向代理用的。
/// 两者相互独立，可同时存在。alpha 的 `/status` 和 `/task` 均需要实例 token。
fn authed_get(client: &reqwest::Client, config: &Config, path: &str) -> reqwest::RequestBuilder {
    let url = format!("{}{}", config.base_url(), path);
    let mut request = client.get(&url);
    if let Some(token) = &config.token
        && !token.is_empty()
    {
        request = request.header("token", token);
    }
    if let (Some(username), Some(password)) = (&config.username, &config.password) {
        request = request.basic_auth(username, Some(password));
    }
    request
}

/// 每秒拉取一次状态/任务数据，更新共享状态。复用单个 [`reqwest::Client`] 以享受连接池。
///
/// 状态机：
/// - `/status` 不可达 → 离线，写入 `task_error`，但**保留**上次任务与状态（不闪错误屏）。
/// - `status == stopped` → mower 已停止，清空任务与错误。
/// - 其余 → 拉取 `/task`；成功则更新任务并清错，失败则记错但保留上次任务。
pub async fn fetch_state_loop(
    task_info: Arc<Mutex<Option<TaskInfo>>>,
    task_error: Arc<Mutex<Option<String>>>,
    mower_status: Arc<Mutex<MowerStatus>>,
    config: Config,
) {
    if let Some(id) = config.local_instance_id.clone() {
        fetch_local_state_loop(
            task_info,
            task_error,
            mower_status,
            id,
            config.local_hud_port,
        )
        .await;
        return;
    }
    let client = build_client();
    let mut interval = interval(Duration::from_secs(1));

    loop {
        interval.tick().await;

        match fetch_status(&client, &config).await {
            Ok(status) => {
                if let Ok(mut guard) = mower_status.lock() {
                    *guard = status;
                }
                if status == MowerStatus::Stopped {
                    // 已停止：没有任务，清空残留任务与错误，让 UI 展示“已停止”。
                    if let Ok(mut guard) = task_info.lock() {
                        *guard = None;
                    }
                    if let Ok(mut guard) = task_error.lock() {
                        *guard = None;
                    }
                } else {
                    match fetch_earliest_task(&client, &config).await {
                        Ok(task) => {
                            if let Ok(mut guard) = task_info.lock() {
                                *guard = task;
                            }
                            if let Ok(mut guard) = task_error.lock() {
                                *guard = None;
                            }
                        }
                        Err(e) => {
                            if let Ok(mut guard) = task_error.lock() {
                                *guard = Some(e.to_string());
                            }
                        }
                    }
                }
            }
            Err(e) => {
                // 整个后端不可达：保留上次任务与状态，仅标记离线。
                if let Ok(mut guard) = task_error.lock() {
                    *guard = Some(e.to_string());
                }
            }
        }
    }
}

async fn fetch_local_state_loop(
    task_info: Arc<Mutex<Option<TaskInfo>>>,
    task_error: Arc<Mutex<Option<String>>>,
    mower_status: Arc<Mutex<MowerStatus>>,
    id: String,
    mut port: Option<u16>,
) {
    let client = build_local_client();
    let mut interval = interval(Duration::from_secs(1));
    let mut next_scan = tokio::time::Instant::now();
    loop {
        interval.tick().await;
        if port.is_none() && tokio::time::Instant::now() >= next_scan {
            port = crate::discovery::scan_local_mowers()
                .await
                .into_iter()
                .find(|instance| instance.local_id.as_deref() == Some(&id))
                .and_then(|instance| instance.local_port);
            next_scan = tokio::time::Instant::now() + Duration::from_secs(5);
        }
        let result = match port {
            Some(current_port) => match fetch_local_snapshot(&client, current_port).await {
                Ok(snapshot) if snapshot.id == id => {
                    let status = MowerStatus::from_api(&snapshot.status);
                    let task = if status == MowerStatus::Stopped {
                        Ok(None)
                    } else {
                        task_info_from_tasks(snapshot.tasks)
                    };
                    task.map(|task| (status, task))
                }
                Ok(_) => Err("本地实例已改变，正在重新查找".into()),
                Err(error) => Err(error),
            },
            None => Err("等待本地 Mower 实例".into()),
        };
        match result {
            Ok((status, task)) => {
                if let Ok(mut guard) = mower_status.lock() {
                    *guard = status;
                }
                if let Ok(mut guard) = task_info.lock() {
                    *guard = task;
                }
                if let Ok(mut guard) = task_error.lock() {
                    *guard = None;
                }
            }
            Err(error) => {
                if port.take().is_some() {
                    next_scan = tokio::time::Instant::now();
                }
                if let Ok(mut guard) = task_error.lock() {
                    *guard = Some(error.to_string());
                }
            }
        }
    }
}

/// 拉取 mower 运行状态。
async fn fetch_status(client: &reqwest::Client, config: &Config) -> Result<MowerStatus, BoxError> {
    let response = checked_response(authed_get(client, config, "/status").send().await?)?;
    let status: StatusResponse = response.json().await?;
    if status.status.is_empty() {
        return Err("Mower 状态响应缺少状态".into());
    }
    Ok(MowerStatus::from_api(&status.status))
}

fn checked_response(response: reqwest::Response) -> Result<reqwest::Response, BoxError> {
    if response.status() == reqwest::StatusCode::FORBIDDEN {
        return Err("Mower 拒绝访问，请检查实例 Token（HTTP 403）".into());
    }
    Ok(response.error_for_status()?)
}

/// 解析后端时间字符串为本地时间，尽量宽容以免静默丢任务。
fn parse_task_time(s: &str) -> Option<DateTime<Local>> {
    // 优先按 RFC3339（带时区/Z）解析。
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Local));
    }
    // 否则按朴素本地时间，允许有或没有小数秒。
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            // DST 歧义时取较早的一个，而非像 .single() 那样直接丢弃。
            return Local.from_local_datetime(&naive).earliest();
        }
    }
    None
}

/// 优先使用 alpha 提供的展示快照，同时兼容旧版本的类型和元数据。
fn task_label(task: &Task) -> String {
    let presentation = task.presentation.as_ref();
    let mut title = presentation
        .map(|p| p.title.as_str())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            task.display_name
                .as_deref()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or(&task.task_type.display_value)
        .to_string();
    if task.task_type.display_value == "技能专精" || title == "技能专精" {
        title = "自动专精".to_string();
    } else if task.task_type.display_value == "换协助位" || title == "换协助位" {
        title = "自动专精换协助位".to_string();
    } else if title == "趴体" {
        title = "线索交流".to_string();
    } else if title == "肥鸭" || title.starts_with("肥鸭 充 ") {
        return "菲亚梅塔充能".to_string();
    }

    let targets = presentation
        .map(|p| {
            p.targets
                .iter()
                .map(|r| r.label.as_str())
                .collect::<Vec<_>>()
                .join("、")
        })
        .filter(|s| !s.is_empty());
    let target = targets.unwrap_or_else(|| {
        if task.meta_data.starts_with("room_") || task.meta_data.starts_with("dormitory_") {
            translate_room(&task.meta_data)
        } else {
            task.meta_data.clone()
        }
    });
    if title == "空任务" && !target.is_empty() {
        return target;
    }
    if task.plan.as_ref().is_none_or(|plan| plan.is_empty())
        && !target.is_empty()
        && title != target
        && !title.ends_with(&format!(" {target}"))
        && !title.ends_with(&format!("· {target}"))
    {
        title.push_str(" · ");
        title.push_str(&target);
    }
    title
}

/// 先保留特殊阶段，再将普通上下班合并为 HUD 短名称。
fn task_summary(task: &Task) -> String {
    let title = task_label(task);
    for label in [
        task.display_name.as_deref().unwrap_or_default(),
        title.as_str(),
    ] {
        if matches!(label, "恢复原班" | "替班交接" | "轮休协调") {
            return label.to_string();
        }
    }
    if task.task_type.display_value == "用尽下班" {
        return "用尽下班".into();
    }
    // 普通上班类型不能证明已经回满，只保留接口明确给出的描述。
    for label in [
        title.as_str(),
        task.display_name.as_deref().unwrap_or_default(),
        task.task_type.display_value.as_str(),
    ] {
        if label.contains("用尽") || label.contains("回满") {
            return label.to_string();
        }
    }
    if matches!(task.task_type.display_value.as_str(), "上班" | "下班")
        || title
            .split(['；', ';'])
            .map(str::trim)
            .any(|part| part.ends_with("上班") || part.ends_with("下班"))
    {
        return "基建换班".into();
    }
    title
}

/// 把后端房间标识翻译成中文，镜像后端 `translate_room`。
fn translate_room(room: &str) -> String {
    let parts: Vec<&str> = room.split('_').collect();
    if room.contains("dormitory") {
        return format!("{}层宿舍", parts.get(1).copied().unwrap_or(""));
    }
    if room.contains("central") {
        return "控制中枢".to_string();
    }
    if room.contains("factory") {
        return "加工站".to_string();
    }
    if room.contains("meeting") {
        return "会客室".to_string();
    }
    if room.contains("contact") {
        return "办公室".to_string();
    }
    if room.contains("train") {
        return "训练室".to_string();
    }
    if room.contains("room")
        && let (Some(a), Some(b)) = (parts.get(1), parts.get(2))
    {
        return format!("B{}0{}", a, b);
    }
    room.to_string()
}

/// 跑单任务（priority==1）才返回目标房间字符串，否则 `None`。
fn run_order_rooms(task: &Task) -> Option<String> {
    if task.task_type.priority != 1 {
        return None;
    }
    let plan = task.plan.as_ref()?;
    if plan.is_empty() {
        return None;
    }
    let rooms: Vec<String> = plan
        .keys()
        .map(|room| {
            task.presentation
                .as_ref()
                .and_then(|p| p.rooms.iter().find(|entry| entry.id == *room))
                .map(|entry| {
                    entry
                        .label
                        .strip_prefix("贸易站 ")
                        .unwrap_or(&entry.label)
                        .to_string()
                })
                .unwrap_or_else(|| translate_room(room))
        })
        .collect();
    Some(rooms.join("、"))
}

/// 拉取并计算「当前 + 下一个」任务。
async fn fetch_earliest_task(
    client: &reqwest::Client,
    config: &Config,
) -> Result<Option<TaskInfo>, BoxError> {
    let response = checked_response(authed_get(client, config, "/task").send().await?)?;
    let tasks: Vec<Task> = response.json().await?;
    task_info_from_tasks(tasks)
}

fn task_info_from_tasks(tasks: Vec<Task>) -> Result<Option<TaskInfo>, BoxError> {
    if tasks.is_empty() {
        return Ok(None);
    }

    // 解析时间并保留有效任务
    let parsed_tasks: Vec<(Task, DateTime<Local>)> = tasks
        .into_iter()
        .filter_map(|task| parse_task_time(&task.time).map(|t| (task, t)))
        .collect();

    if parsed_tasks.is_empty() {
        return Err("没有找到有效任务".into());
    }

    // Mower 队列本身反映优先级与保留中的任务，不能按时间重排或跳过逾期任务。
    let (current_task, current_time) = &parsed_tasks[0];
    let (current_countdown, is_expired, remaining_secs) =
        calculate_countdown_with_status(current_time);
    let current_rooms = run_order_rooms(current_task);

    // 下一个任务：只要后面还有就展示
    let next_task = parsed_tasks.get(1).map(|(task, time)| {
        let (countdown, _, _) = calculate_countdown_with_status(time);
        NextTaskInfo {
            name: task_summary(task),
            target_time: *time,
            countdown,
        }
    });

    Ok(Some(TaskInfo {
        current_name: task_summary(current_task),
        current_time: *current_time,
        current_countdown,
        is_expired,
        remaining_seconds: remaining_secs,
        current_rooms,
        next_task,
    }))
}

/// 测试 API 连接是否可用。
pub async fn test_api_connection(config: &Config) -> Result<(), BoxError> {
    if let Some(id) = &config.local_instance_id {
        let port = config.local_hud_port.ok_or("尚未发现本地 Mower 服务")?;
        let snapshot = fetch_local_snapshot(&build_local_client(), port).await?;
        return if &snapshot.id == id {
            Ok(())
        } else {
            Err("本地实例已改变".into())
        };
    }
    let client = build_client();
    fetch_status(&client, config).await.map(|_| ())
}

#[cfg(test)]
mod tests;
