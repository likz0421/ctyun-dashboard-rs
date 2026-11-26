use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub version: String,
    pub settings: Settings,
    pub users: Vec<User>,
    pub accounts: Vec<Account>,
    /// 每日任务执行记录: account_id -> 最后执行日期。持久化，避免重启后重复执行。
    #[serde(default)]
    pub last_daily_run: std::collections::HashMap<String, String>,
}

fn default_auth_required() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub web_port: u16,
    pub keep_alive_seconds: u64,
    pub pulse_interval_seconds: u64,
    pub allow_registration: bool,
    pub default_quota: u32,
    /// 是否开启面板登录鉴权。默认 true（公网安全）；纯内网可关闭以便免登录访问。
    #[serde(default = "default_auth_required")]
    pub auth_required: bool,
    pub cron: CronConfig,
    pub notify: NotifyConfig,
    pub system_title: String,
    pub system_subtitle: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            web_port: 8571,
            keep_alive_seconds: 60,
            pulse_interval_seconds: 30,
            allow_registration: false,
            default_quota: 2,
            auth_required: true,
            cron: CronConfig::default(),
            notify: NotifyConfig::default(),
            system_title: "云电脑控制中心".into(),
            system_subtitle: "CtYun Dashboard · Rust Edition".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CronConfig {
    pub execute_time: String,
    pub enable_sub_cron: bool,
    pub sign_cron: String,
    pub ai_chat_cron: String,
    pub cloud_hang_cron: String,
    pub redeem_cron: String,
}

impl Default for CronConfig {
    fn default() -> Self {
        Self {
            execute_time: "01:20".into(),
            enable_sub_cron: false,
            sign_cron: "0 20 1 * * *".into(),
            ai_chat_cron: "0 25 1 * * *".into(),
            cloud_hang_cron: "0 30 1 * * *".into(),
            redeem_cron: "0 35 1 * * *".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NotifyConfig {
    pub enabled: bool,
    pub channel: String,
    pub webhook_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub role: String,
    pub max_quota: u32,
    pub avatar: String,
    pub created_at: String,
    pub notify: UserNotify,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UserNotify {
    pub enabled: bool,
    pub channel: String,
    pub webhook_url: String,
    pub secret: String,
    pub custom_title_template: String,
    pub custom_content_template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Account {
    pub id: String,
    #[serde(default = "default_platform")]
    pub platform: String,
    pub owner_id: String,
    pub name: String,
    pub user: String,
    pub password: String,
    pub device_code: String,
    pub saved_login_info: Option<serde_json::Value>,
    pub display_config: DisplayConfig,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub bound: bool,
    pub session_expired: bool,
    pub features: Features,
    pub redeem_config: RedeemConfig,
    pub stats: AccountStats,
    pub desktops: Vec<Desktop>,
    pub keepalive_interval: u64,
    pub created_at: String,
}

fn default_platform() -> String { "ctyun".into() }
fn default_true() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    pub width: u32,
    pub height: u32,
    pub scale: u32,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self { width: 2560, height: 1440, scale: 150 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Features {
    #[serde(default = "default_true")]
    pub keep_alive: bool,
    pub auto_sign: bool,
    pub ai_chat: bool,
    pub cloud_hang: bool,
    pub auto_redeem: bool,
    #[serde(default = "default_true")]
    pub auto_boot: bool,
}

impl Default for Features {
    fn default() -> Self {
        Self {
            keep_alive: true,
            // 新账号默认开启「零成本、纯领取」的任务：用户添加账号就是为了让这些自动跑。
            // 旧配置已有 features 字段，#[serde(default)] 不会覆盖，历史账号不受影响。
            // 自动兑换默认关闭（会消耗积分/需先配置商品）。
            auto_sign: true,
            ai_chat: true,
            cloud_hang: true,
            auto_redeem: false,
            auto_boot: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RedeemConfig {
    pub enabled: bool,
    pub target_type: String,
    pub desktop_id: String,
    pub desktop_name: String,
    pub prod_id: u64,
    pub prod_name: String,
    pub prod_type: String,
    pub cost_points: u64,
    pub max_redeem_times: u32,
    pub schedule_type: String,
    pub monthly_days: Vec<i32>,
    pub interval_days: u32,
    pub last_redeem_date: String,
}

impl Default for RedeemConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_type: "desktop".into(),
            desktop_id: String::new(),
            desktop_name: String::new(),
            prod_id: 17023101,
            prod_name: String::new(),
            prod_type: "pointstplupgrade".into(),
            cost_points: 500,
            max_redeem_times: 1,
            schedule_type: "monthly_days".into(),
            monthly_days: vec![-1],
            interval_days: 30,
            last_redeem_date: String::new(),
        }
    }
}

/// 积分余额变动历史的一条记录。老配置缺字段时由 `#[serde(default)]` 兜底为空 Vec。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PointsEntry {
    pub ts: String,       // 北京时间 "YYYY-MM-DD HH:MM:SS"
    pub points: u64,      // 变动后的余额
    pub delta: i64,       // 相对上一条的增减（正=增加，负=消耗）
    pub reason: String,   // 变动原因/标注
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AccountStats {
    pub last_sign_time: String,
    pub last_ai_chat_time: String,
    pub last_hang_time: String,
    pub hang_minutes_today: u64,
    pub points: u64,
    pub keep_alive_status: String,
    pub last_error: String,
    pub today_success_count: u64,
    pub last_success_date: String,
    pub pulse_interval_seconds: u64,
    /// 积分余额变动历史（最多保留最近 200 条）。
    pub points_history: Vec<PointsEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Desktop {
    pub desktop_id: String,
    pub obj_id: String,
    pub desktop_name: String,
    pub desktop_code: String,
    pub use_status_text: String,
    pub use_status: i32,
    pub image_name: String,
    pub flavor_name: String,
    pub cpu: String,
    pub memory: String,
    pub os: String,
    pub spec_str: String,
    pub obj_type: i32,
    pub is_pool: bool,
    #[serde(default = "default_true")]
    pub keepalive_enabled: bool,
    #[serde(default = "default_true")]
    pub task_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_boot_enabled: bool,
    pub keepalive_interval: u64,
    pub last_pulse_at: String,
    pub live_metrics: LiveMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LiveMetrics {
    pub status: String,
    pub countdown: u64,
    pub keep_alive_seconds: u64,
    pub last_heartbeat_time: String,
    pub last_heartbeat_result: String,
    pub success_count: u64,
    pub official_tasks: Vec<TaskProgress>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TaskProgress {
    pub name: String,
    pub current: u64,
    pub total: u64,
    pub status: i32,
    pub points: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub user_id: String,
    pub username: String,
    pub role: String,
    pub max_quota: u32,
    pub remember: bool,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LogEntry {
    pub id: String,
    pub timestamp: String,
    pub source: String,
    pub message: String,
    pub level: String,
    pub account_name: String,
    pub platform: String,
    pub repeat_count: u64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: "1.0.0".into(),
            settings: Settings::default(),
            users: Vec::new(),
            accounts: Vec::new(),
            last_daily_run: std::collections::HashMap::new(),
        }
    }
}

pub fn now_beijing() -> String {
    use chrono::{FixedOffset, Utc};
    let beijing = Utc::now().with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap());
    beijing.format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn today_beijing() -> String {
    use chrono::{FixedOffset, Utc};
    Utc::now()
        .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
        .format("%Y-%m-%d")
        .to_string()
}

