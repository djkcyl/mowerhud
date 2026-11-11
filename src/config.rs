//! API 连接配置：结构定义、文件读写、以及从输入文本解析配置。

use serde::{Deserialize, Serialize};
use std::fs::{read_to_string, write};

const CONFIG_FILE: &str = "mower_config.json";

/// HUD 展示模式。
///
/// 注意：序列化用的是**变体名**（serde 默认），故枚举顺序可自由调整、不影响旧配置读取；
/// `as u8`（0/1/2/3）仅用于托盘内部传递当前模式，与持久化无关。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayMode {
    /// 普通：当前 + 下一个任务（双列）。
    #[default]
    Normal,
    /// 单列：同普通，但只留当前任务这一列，不展示下一个。
    NormalSingle,
    /// 隐身：隐藏窗口，只留托盘图标。
    Stealth,
}

/// 容错地反序列化展示模式：未知值（含已删除的旧 "Simple"）一律回退普通，
/// 避免旧配置因为某个字段不认识就整份读取失败、连带丢掉地址/端口等设置。
fn deserialize_display_mode<'de, D>(d: D) -> Result<DisplayMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    Ok(match s.as_str() {
        "NormalSingle" => DisplayMode::NormalSingle,
        "Stealth" => DisplayMode::Stealth,
        _ => DisplayMode::Normal,
    })
}

/// API 连接配置。
///
/// 注意：`password` 以明文形式存盘到 [`CONFIG_FILE`]，仅适合本地可信环境使用。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    #[serde(default)]
    pub leisure: crate::leisure::LeisureConfig,
    pub host: String,
    pub port: u16,
    pub use_https: bool,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Mower 自带的 token 鉴权（作为 HTTP 头 `token` 发送）。缺省即不发送。
    #[serde(default)]
    pub token: Option<String>,
    /// 本地只读服务的稳定实例标识；缺省时使用普通 HTTP 连接。
    #[serde(default)]
    pub local_instance_id: Option<String>,
    /// 最近一次发现的本地服务端口，失效时会重新扫描。
    #[serde(default)]
    pub local_hud_port: Option<u16>,
    /// 是否发送桌面通知。`#[serde(default)]` 保证旧配置文件缺字段时回退到 true。
    #[serde(default = "default_true")]
    pub notify_enabled: bool,
    /// 是否播放提示音。
    #[serde(default = "default_true")]
    pub sound_enabled: bool,
    /// 上次窗口左上角位置（逻辑像素），用于跨重启保持位置。缺省即不指定。
    #[serde(default)]
    pub window_x: Option<f32>,
    #[serde(default)]
    pub window_y: Option<f32>,
    /// HUD 展示模式（普通/单列/隐身）。容错读取：未知值回退普通。
    #[serde(default, deserialize_with = "deserialize_display_mode")]
    pub display_mode: DisplayMode,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            leisure: crate::leisure::LeisureConfig::default(),
            host: "127.0.0.1".to_string(),
            port: 3289,
            use_https: false,
            username: None,
            password: None,
            token: None,
            local_instance_id: None,
            local_hud_port: None,
            notify_enabled: true,
            sound_enabled: true,
            window_x: None,
            window_y: None,
            display_mode: DisplayMode::Normal,
        }
    }
}

impl Config {
    pub fn auto_connect_to_local(&self, first_start: bool) -> bool {
        self.local_instance_id.is_some()
            || first_start
            || (matches!(self.host.as_str(), "127.0.0.1" | "localhost" | "::1")
                && !self.use_https
                && self.username.is_none()
                && self.password.is_none())
    }

    /// 从 `host` 或 `host:port` 文本解析出配置。地址为空时回退到 `127.0.0.1`。
    pub fn from_address(
        address: &str,
        use_https: bool,
        username: &str,
        password: &str,
    ) -> Result<Config, String> {
        let address = address.trim();
        let address = if address.is_empty() {
            "127.0.0.1"
        } else {
            address
        };

        let parts: Vec<&str> = address.split(':').collect();
        let (host, port) = match parts.len() {
            // 只有主机名，按协议选择默认端口
            1 => {
                let default_port = if use_https { 443 } else { 80 };
                (parts[0].trim().to_string(), default_port)
            }
            // 标准 host:port
            2 => {
                let host = parts[0].trim().to_string();
                let port = parts[1]
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| "端口号格式错误".to_string())?;
                (host, port)
            }
            _ => return Err("地址格式错误，应为 host 或 host:port".to_string()),
        };

        if host.is_empty() {
            return Err("主机地址不能为空".to_string());
        }

        Ok(Config {
            host,
            port,
            use_https,
            username: (!username.is_empty()).then(|| username.to_string()),
            password: (!password.is_empty()).then(|| password.to_string()),
            // 提醒开关由调用方在解析后单独设置，这里取默认值
            ..Config::default()
        })
    }

    /// 返回不含路径的基础 URL，例如 `http://127.0.0.1:3289`。
    pub fn base_url(&self) -> String {
        let protocol = if self.use_https { "https" } else { "http" };
        format!("{}://{}:{}", protocol, self.host, self.port)
    }
}

pub fn save_config(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string_pretty(config)?;
    write(CONFIG_FILE, json)?;
    Ok(())
}

pub fn load_config() -> Result<Config, Box<dyn std::error::Error>> {
    let json = read_to_string(CONFIG_FILE)?;
    let config: Config = serde_json::from_str(&json)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_only_with_default_http_port() {
        let c = Config::from_address("192.168.1.10", false, "", "").unwrap();
        assert_eq!(c.host, "192.168.1.10");
        assert_eq!(c.port, 80);
        assert!(c.username.is_none());
    }

    #[test]
    fn parses_host_and_port() {
        let c = Config::from_address("127.0.0.1:3289", false, "u", "p").unwrap();
        assert_eq!(c.port, 3289);
        assert_eq!(c.username.as_deref(), Some("u"));
        assert_eq!(c.password.as_deref(), Some("p"));
    }

    #[test]
    fn empty_address_falls_back_to_localhost() {
        let c = Config::from_address("  ", true, "", "").unwrap();
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.port, 443); // https 默认端口
    }

    #[test]
    fn rejects_bad_port() {
        assert!(Config::from_address("127.0.0.1:abc", false, "", "").is_err());
    }

    #[test]
    fn base_url_uses_protocol() {
        let c = Config::from_address("h:1", true, "", "").unwrap();
        assert_eq!(c.base_url(), "https://h:1");
    }

    #[test]
    fn from_address_defaults_alerts_on() {
        let c = Config::from_address("h:1", false, "", "").unwrap();
        assert!(c.notify_enabled);
        assert!(c.sound_enabled);
    }

    #[test]
    fn old_config_without_alert_fields_defaults_to_true() {
        // 模拟旧版本写出的、不含 notify_enabled/sound_enabled 的配置文件
        let json =
            r#"{"host":"127.0.0.1","port":3289,"use_https":false,"username":null,"password":null}"#;
        let c: Config = serde_json::from_str(json).unwrap();
        assert!(c.notify_enabled);
        assert!(c.sound_enabled);
    }

    #[test]
    fn local_autoconnect_preserves_explicit_remote_and_proxy_settings() {
        let mut c = Config::default();
        assert!(c.auto_connect_to_local(false));
        c.host = "remote.example".into();
        assert!(!c.auto_connect_to_local(false));
        c.host = "127.0.0.1".into();
        c.username = Some("proxy-user".into());
        assert!(!c.auto_connect_to_local(false));
        c.username = None;
        c.use_https = true;
        assert!(!c.auto_connect_to_local(false));
    }

    #[test]
    fn leisure_settings_default_off_for_old_configs_and_persist_custom_values() {
        let old =
            r#"{"host":"127.0.0.1","port":3289,"use_https":false,"username":null,"password":null}"#;
        let mut config: Config = serde_json::from_str(old).unwrap();
        assert!(!config.leisure.enabled);
        config.leisure.enabled = true;
        config.leisure.min_idle_minutes = 40;
        config.leisure.warning_minutes = 7;
        let saved = serde_json::to_string(&config).unwrap();
        let restored: Config = serde_json::from_str(&saved).unwrap();
        assert!(restored.leisure.enabled);
        assert_eq!(restored.leisure.min_idle_minutes, 40);
        assert_eq!(restored.leisure.warning_minutes, 7);
    }
}
