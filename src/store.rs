use crate::crypto::{decrypt_secret, encrypt_secret};
use crate::models::{AppConfig, LogEntry, User};
use rand::RngCore;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use tokio::sync::broadcast;

pub const MAX_RECENT_LOGS: usize = 800;

#[derive(Clone)]
pub struct Store {
    pub config: std::sync::Arc<std::sync::RwLock<AppConfig>>,
    pub master_key: Vec<u8>,
    pub data_dir: PathBuf,
    /// 串行化配置写盘，避免并发 save 共用临时文件交错导致 JSON 损坏。
    save_lock: std::sync::Arc<std::sync::Mutex<()>>,
}

impl Store {
    pub fn load() -> Self {
        let data_dir = std::env::var("CTYUN_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data"));
        std::fs::create_dir_all(&data_dir).ok();
        std::fs::create_dir_all(data_dir.join("logs")).ok();

        let key_path = data_dir.join(".master.key");
        // 主密钥必须是合法 32 字节：损坏/长度错误的 hex 若被接受，会导致
        // encrypt_secret 失败→密码明文落盘，或每次重启换钥匙→旧凭据全失效。
        // 此时优先用 .bak 恢复，否则生成新密钥并立即持久化。
        let valid_key = |hex: &str| -> Option<Vec<u8>> {
            hex::decode(hex.trim()).ok().filter(|k| k.len() == 32)
        };
        let master_key = if let Some(k) = std::fs::read_to_string(&key_path).ok().and_then(|h| valid_key(&h)) {
            k
        } else if let Some(k) = std::fs::read_to_string(data_dir.join(".master.key.bak")).ok().and_then(|h| valid_key(&h)) {
            let _ = std::fs::write(&key_path, hex::encode(&k));
            k
        } else {
            let mut key = vec![0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut key);
            let _ = std::fs::write(&key_path, hex::encode(&key));
            let _ = std::fs::write(data_dir.join(".master.key.bak"), hex::encode(&key));
            key
        };

        let config_path = data_dir.join("app_config.json");
        let mut config: AppConfig = std::fs::read_to_string(&config_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        if !config.users.iter().any(|u| u.role == "admin") {
            let admin = User {
                id: "u_admin".into(),
                username: "admin".into(),
                password_hash: crate::crypto::password_hash("admin123"),
                role: "admin".into(),
                max_quota: 999,
                avatar: String::new(),
                created_at: crate::models::now_beijing(),
                notify: Default::default(),
            };
            config.users.push(admin);
        }
        if config.version.is_empty() {
            config.version = "1.0.0".into();
        }
        // 一次性迁移：旧版本创建账号时漏设 enabled=true，导致账号被误标"已停用"
        // 且保活不执行。1.1.0 之前的历史账号统一恢复启用；用户此后主动停用不再受影响。
        if config.version.as_str() < "1.1.0" {
            for acc in config.accounts.iter_mut() {
                if !acc.enabled { acc.enabled = true; }
            }
            config.version = "1.1.0".into();
        }

        let store = Self {
            config: std::sync::Arc::new(std::sync::RwLock::new(config)),
            master_key,
            data_dir,
            save_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
        };
        store.save();
        store
    }

    pub fn config_path(&self) -> PathBuf {
        self.data_dir.join("app_config.json")
    }

    pub fn save(&self) {
        let guard = match self.config.read() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let json = serde_json::to_string_pretty(&*guard).unwrap_or_default();
        drop(guard);
        self.write_config_file(&json);
    }

    /// Synchronous save for use outside an async runtime.
    pub fn save_blocking(&self, config: &AppConfig) {
        let json = serde_json::to_string_pretty(config).unwrap_or_default();
        self.write_config_file(&json);
    }

    fn write_config_file(&self, json: &str) {
        // 串行化：并发 save（如多个任务同时落盘）不会交错写同一 tmp 文件。
        let _guard = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.config_path();
        let backup = self.data_dir.join("app_config.json.bak");
        if let Ok(old) = std::fs::read_to_string(&path) {
            let _ = std::fs::write(&backup, old);
        }
        // 进程内唯一临时文件名，避免多进程/多线程同时写冲突。
        let tmp = self.data_dir.join(format!("app_config.json.tmp.{}", std::process::id()));
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    pub fn encrypt_password(&self, plain: &str) -> String {
        encrypt_secret(&self.master_key, plain).unwrap_or_else(|_| plain.to_string())
    }

    pub fn decrypt_password(&self, stored: &str) -> String {
        decrypt_secret(&self.master_key, stored).unwrap_or_else(|_| stored.to_string())
    }

    pub fn master_key_hex(&self) -> String {
        hex::encode(&self.master_key)
    }

    pub fn log_dir(&self) -> &Path {
        &self.data_dir
    }
}

#[derive(Clone)]
pub struct LogHub {
    pub recent: std::sync::Arc<tokio::sync::RwLock<VecDeque<LogEntry>>>,
    pub tx: broadcast::Sender<LogEntry>,
    pub data_dir: PathBuf,
}

impl LogHub {
    pub fn new(data_dir: PathBuf) -> Self {
        let (tx, _) = broadcast::channel(512);
        let hub = Self {
            recent: std::sync::Arc::new(tokio::sync::RwLock::new(VecDeque::with_capacity(MAX_RECENT_LOGS))),
            tx,
            data_dir,
        };
        hub.replay_from_disk();
        hub
    }

    fn replay_from_disk(&self) {
        let today = crate::models::today_beijing();
        let path = self.data_dir.join("logs").join(format!("dashboard-{}.jsonl", today));
        if let Ok(content) = std::fs::read_to_string(path) {
            let mut count = 0;
            for line in content.lines().rev().take(400) {
                if let Ok(entry) = serde_json::from_str::<LogEntry>(line) {
                    if let Ok(mut recent) = self.recent.try_write() {
                        recent.push_front(entry);
                    }
                    count += 1;
                    if count >= 400 { break; }
                }
            }
        }
    }

    pub async fn log(&self, source: &str, message: &str, level: &str, account_name: &str) {
        let entry = LogEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: crate::models::now_beijing(),
            source: source.into(),
            message: message.into(),
            level: level.into(),
            account_name: account_name.into(),
            platform: "ctyun".into(),
            repeat_count: 1,
        };

        {
            let mut recent = self.recent.write().await;
            recent.push_front(entry.clone());
            while recent.len() > MAX_RECENT_LOGS {
                recent.pop_back();
            }
        }
        let _ = self.tx.send(entry.clone());

        let today = crate::models::today_beijing();
        let dir = self.data_dir.join("logs");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("dashboard-{}.jsonl", today));
        if let Ok(line) = serde_json::to_string(&entry) {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(file, "{}", line);
            }
        }
    }
}

/* ============================================================
   通知：Webhook 推送（钉钉/企微/飞书/Telegram/Server酱/Bark/自定义）。
   由 api 的测试按钮与 scheduler 的自动告警共用。
   ============================================================ */

/// 向指定 Webhook 推送一条消息。带超时，失败返回错误但不 panic——
/// 通知是"副作用"，绝不能因为网络不通拖垮任务或调度主流程。
pub async fn send_webhook(url: &str, channel: &str, title: &str, content: &str) -> Result<(), String> {
    if url.is_empty() { return Err("未配置 Webhook 地址".into()); }
    // 解析 URL 后按 IP 判定私网/回环/链路本地/元数据地址（防 SSRF）。
    let parsed = url::Url::parse(url).map_err(|_| "Webhook 地址格式错误".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("Webhook 仅支持 http/https".into());
    }
    if let Some(host) = parsed.host_str() {
        let blocked = host == "localhost"
            || host.parse::<std::net::IpAddr>().map(|ip| match ip {
                std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback()
                    || v4.is_link_local() || v4.is_unspecified(),
                std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
            }).unwrap_or(false);
        if blocked {
            return Err("出于安全考虑，禁止向内网地址推送".into());
        }
    }
    let payload = match channel {
        "dingtalk" => serde_json::json!({"msgtype": "markdown", "markdown": {"title": title, "text": format!("### {}\n\n{}", title, content)}}),
        "wecom" => serde_json::json!({"msgtype": "markdown", "markdown": {"content": format!("**{}**\n{}", title, content)}}),
        "feishu" => serde_json::json!({"msg_type": "text", "content": {"text": format!("{}\n{}", title, content)}}),
        "telegram" => serde_json::json!({"text": format!("{}\n{}", title, content)}),
        _ => serde_json::json!({"title": title, "content": content}),
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    client.post(url).json(&payload).send().await.map_err(|e| e.to_string())?;
    Ok(())
}

/// 按账号归属定向通知。先发给 owner 的用户通知配置；若 owner 未启用通知，
/// 回退给所有启用了通知的 admin（自用单用户场景下 admin 即 owner）。
/// 先克隆配置再发请求，绝不持锁做网络 IO。
pub async fn notify_account(store: &Store, _account_id: &str, account_name: &str,
                            owner_id: &str, title: &str, content: &str) {
    let (owner_notify, admin_notifies) = {
        let cfg = store.config.read().unwrap_or_else(|e| e.into_inner());
        let owner = cfg.users.iter().find(|u| u.id == owner_id).map(|u| u.notify.clone());
        let admins: Vec<_> = cfg.users.iter().filter(|u| u.role == "admin")
            .map(|u| u.notify.clone()).collect();
        (owner, admins)
    };

    let mut sent = false;
    if let Some(n) = owner_notify {
        if n.enabled && !n.webhook_url.is_empty() {
            let text = format!("[{}] {}", account_name, content);
            if send_webhook(&n.webhook_url, &n.channel, title, &text).await.is_ok() {
                sent = true;
            }
        }
    }
    if sent { return; }
    // 兜底：owner 没配通知时发给所有启用的 admin。
    for n in admin_notifies {
        if n.enabled && !n.webhook_url.is_empty() {
            let text = format!("[{}] {}", account_name, content);
            let _ = send_webhook(&n.webhook_url, &n.channel, title, &text).await;
        }
    }
}
