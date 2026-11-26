use crate::ctyun::CtYunClient;
use crate::keeper::{run_keepalive, SessionMode};
use crate::models::Account;
use crate::store::{LogHub, Store};
use chrono::{Datelike, FixedOffset, Timelike, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct Scheduler {
    pub store: Store,
    pub log: LogHub,
    pub active: Arc<RwLock<HashSet<String>>>,
    /// 账号级任务会话占用（签到认领/挂机补挂）：对齐上游 isTaskHanging。
    /// 任务会话期间该账号所有桌面的脉冲都要避让，避免并发会话互相顶掉、官方计时不累加。
    pub task_active: Arc<RwLock<HashSet<String>>>,
    pub web_active: Arc<RwLock<HashMap<String, i64>>>,
    /// 签到待确认队列: account_id -> 首次认领时间戳(秒)。用于跨分钟复检，期间不重复认领。
    pub pending_sign: Arc<RwLock<HashMap<String, i64>>>,
    /// 签到复检节流: account_id -> 上次发起复检的时间戳(秒)，避免每 10s tick 重复 spawn。
    pub last_sign_check: Arc<RwLock<HashMap<String, i64>>>,
    /// 上次批量刷新官方任务/积分的时间戳(秒)，让看板任务状态实时更新。
    pub last_metrics_refresh: Arc<RwLock<i64>>,
    /// 上次自动开机守护检查的时间戳(秒)。
    pub last_auto_boot: Arc<RwLock<i64>>,
    /// 已就"会话过期"告警过的账号，避免每 10s tick 重复推送。登录恢复后移除。
    pub notified_expired: Arc<RwLock<HashSet<String>>>,
    /// 上次已通知的积分余额，首次记录不推送（避免启动即轰炸），仅变化时才告警。
    pub last_notified_points: Arc<RwLock<HashMap<String, u64>>>,
    /// 独立 cron 模式下各子任务当天是否已触发：key = "account_id:task"，value = 日期 YYYY-MM-DD。
    /// 避免 cron 命中那一分钟内的多次 tick 重复 spawn（官方落账前 last_*_time 可能尚未更新）。
    pub sub_task_run: Arc<RwLock<HashMap<String, String>>>,
}

impl Scheduler {
    pub fn new(store: Store, log: LogHub) -> Self {
        Self {
            store,
            log,
            active: Arc::new(RwLock::new(HashSet::new())),
            task_active: Arc::new(RwLock::new(HashSet::new())),
            web_active: Arc::new(RwLock::new(HashMap::new())),
            pending_sign: Arc::new(RwLock::new(HashMap::new())),
            last_sign_check: Arc::new(RwLock::new(HashMap::new())),
            last_metrics_refresh: Arc::new(RwLock::new(0)),
            last_auto_boot: Arc::new(RwLock::new(0)),
            notified_expired: Arc::new(RwLock::new(HashSet::new())),
            last_notified_points: Arc::new(RwLock::new(HashMap::new())),
            sub_task_run: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn start(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(10));
            let mut last_refresh = 0i64;
            loop {
                ticker.tick().await;
                self.tick(&mut last_refresh).await;
            }
        });
    }

    fn beijing_now(&self) -> chrono::DateTime<FixedOffset> {
        Utc::now().with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
    }

    async fn tick(&self, last_refresh: &mut i64) {
        let accounts: Vec<Account> = {
            let cfg = match self.store.config.read() { Ok(g) => g, Err(e) => e.into_inner() };
            cfg.accounts.clone()
        };
        let now_ts = Utc::now().timestamp();
        let today = self.beijing_now().format("%Y-%m-%d").to_string();

        // 每 90 秒批量刷新官方任务进度与积分，让看板任务状态实时更新。
        // 用户在天翼云官方完成任务后无需等到定点或手动触发即可看到"已完成"。
        // 只刷新已登录（未过期）且启用的账号；90 秒间隔在实时性与接口风控间取平衡。
        let last_metric = *self.last_metrics_refresh.read().await;
        if now_ts - last_metric >= 90 {
            *self.last_metrics_refresh.write().await = now_ts;
            for acc in accounts.iter().filter(|a| a.enabled && a.platform == "ctyun" && !a.session_expired) {
                let store = self.store.clone();
                let log = self.log.clone();
                let acc_clone = acc.clone();
                tokio::spawn(async move {
                    let acc_name = acc_clone.name.clone();
                    let mut client = CtYunClient::new(acc_clone, store, log.clone());
                    match client.refresh_official_metrics().await {
                        (true, _) => log.log("Task", &format!("[{}] 任务与积分已同步", acc_name), "success", &acc_name).await,
                        (false, false) => log.log("Task", &format!("[{}] 任务/积分同步失败（凭据或接口异常）", acc_name), "warning", &acc_name).await,
                        _ => {}
                    }
                });
            }
        }

        // 自动开机守护：每 2 分钟检查一次。账号级 auto_boot 与单机 auto_boot_enabled
        // 都开启、且云电脑未处于运行状态时，下发开机/唤醒（对齐上游 checkExternalPowerOn）。
        let last_boot = *self.last_auto_boot.read().await;
        if now_ts - last_boot >= 120 {
            *self.last_auto_boot.write().await = now_ts;
            for acc in accounts.iter().filter(|a| a.enabled && a.platform == "ctyun"
                && !a.session_expired && a.features.auto_boot) {
                let store = self.store.clone();
                let log = self.log.clone();
                let active = self.active.clone();
                let acc_clone = acc.clone();
                tokio::spawn(async move {
                    let acc_name = acc_clone.name.clone();
                    let mut client = CtYunClient::new(acc_clone, store, log.clone());
                    // 先刷新真实电源状态，避免用过期状态误开机。
                    if client.fetch_desktops().await.is_err() {
                        log.log("Power", &format!("[{}] 自动开机守护跳过：电源状态刷新失败", acc_name), "warning", &acc_name).await;
                        return;
                    }
                    let offlines: Vec<(String, String, bool)> = client.account.desktops.iter()
                        .filter(|d| d.task_enabled || d.keepalive_enabled)
                        .filter(|d| {
                            let running = d.use_status_text == "运行中" || d.use_status == 25;
                            !running && d.auto_boot_enabled
                        })
                        .map(|d| {
                            let dormant = d.use_status_text.contains("休眠") || d.use_status_text.contains("睡眠");
                            (d.desktop_id.clone(), d.desktop_name.clone(), dormant)
                        })
                        .collect();
                    for (did, name, dormant) in offlines {
                        let key = format!("{}:{}", client.account.id, did);
                        if active.read().await.contains(&key) { continue; } // 有会话在跑就不打扰
                        let action = if dormant { "awake" } else { "poweron" };
                        match client.control_power(&did, action).await {
                            Ok(_) => log.log("Power", &format!("[{}] 自动开机守护: {} 已下发{}指令",
                                client.account.name, name, if dormant { "唤醒" } else { "开机" }),
                                "success", &client.account.name).await,
                            Err(e) => log.log("Power", &format!("[{}] 自动开机守护 {} 失败: {}",
                                client.account.name, name, e), "warning", &client.account.name).await,
                        }
                    }
                });
            }
        }

        // Refresh the desktop inventory every 10 minutes.
        if now_ts - *last_refresh > 600 {
            *last_refresh = now_ts;
            for acc in accounts.iter().filter(|a| a.enabled && a.platform == "ctyun") {
                let store = self.store.clone();
                let log = self.log.clone();
                let acc_clone = acc.clone();
                tokio::spawn(async move {
                    let log_clone = log.clone();
                    let mut client = CtYunClient::new(acc_clone, store, log_clone);
                    if let Err(e) = client.fetch_desktops().await {
                        log.log("System", &format!("刷新云电脑列表失败: {}", e), "warning", "").await;
                    }
                });
            }
        }

        for acc in &accounts {
            if !acc.enabled || acc.platform != "ctyun" { continue; }

            // ---- 自动通知：状态翻转检测（同步去重，异步发送，不阻塞调度）----
            {
                // 账号级去重状态用独占锁的短临界区，避免同一次检测重复 spawn。
                // 会话过期：false→true 告警一次，恢复登录后重置以便下次再告警。
                let was_notified = self.notified_expired.read().await.contains(&acc.id);
                if acc.session_expired && !was_notified {
                    self.notified_expired.write().await.insert(acc.id.clone());
                    let store = self.store.clone();
                    let (id, name, owner) = (acc.id.clone(), acc.name.clone(), acc.owner_id.clone());
                    tokio::spawn(async move {
                        crate::store::notify_account(&store, &id, &name, &owner,
                            "账号会话已过期",
                            "天翼云登录会话已失效，自动任务与保活已暂停。请尽快到面板重新验证登录（短信/扫码），否则任务将持续停摆。").await;
                    });
                } else if !acc.session_expired && was_notified {
                    self.notified_expired.write().await.remove(&acc.id);
                }

                // 积分变动：仅当积分接口刷新后才变化。首次仅记录基线不推送（避免启动即轰炸）。
                let points = acc.stats.points;
                let last = self.last_notified_points.read().await.get(&acc.id).copied();
                match last {
                    None => { self.last_notified_points.write().await.insert(acc.id.clone(), points); }
                    Some(prev) if prev != points => {
                        self.last_notified_points.write().await.insert(acc.id.clone(), points);
                        let store = self.store.clone();
                        let (id, name, owner) = (acc.id.clone(), acc.name.clone(), acc.owner_id.clone());
                        tokio::spawn(async move {
                            let delta = if points >= prev { format!("+{}", points - prev) } else { format!("-{}", prev - points) };
                            crate::store::notify_account(&store, &id, &name, &owner,
                                "积分余额变动",
                                &format!("当前积分 {}（{}），上次 {}。", points, delta, prev)).await;
                        });
                    }
                    _ => {}
                }
            }

            // 签到跨分钟复检：官方落账约需 14 分钟，认领窗口内未确认的转入复检，
            // 期间绝不重复认领（与上游 scheduleSignVerify 语义一致）。
            {
                let pending_since = self.pending_sign.read().await.get(&acc.id).copied();
                if let Some(since) = pending_since {
                    let elapsed = now_ts - since;
                    if elapsed > 20 * 60 {
                        // 超时放弃，避免无限复检
                        self.pending_sign.write().await.remove(&acc.id);
                        self.log.log("Sign", &format!("[{}] 签到官方确认超时(>20分钟)，停止复检", acc.name), "warning", &acc.name).await;
                    } else if elapsed >= 60 {
                        // 节流：距上次复检至少 60 秒才再 spawn，避免每 10s tick 重复请求。
                        let last_check = self.last_sign_check.read().await.get(&acc.id).copied().unwrap_or(0);
                        if now_ts - last_check >= 60 {
                            self.last_sign_check.write().await.insert(acc.id.clone(), now_ts);
                            let store = self.store.clone();
                            let log = self.log.clone();
                            let acc_recheck = acc.clone();
                            let pending = self.pending_sign.clone();
                            let acc_id = acc.id.clone();
                            let acc_name = acc.name.clone();
                            tokio::spawn(async move {
                                let mut client = CtYunClient::new(acc_recheck, store, log.clone());
                                match client.refresh_tasks().await {
                                    Ok(tasks) => {
                                        let done = tasks.iter().any(|t|
                                            (t.name.contains("登录AI云电脑") || t.name.contains("登录"))
                                            && (t.status == 2 || (t.total > 0 && t.current >= t.total)));
                                        if done {
                                            let _ = client.get_points().await;
                                            pending.write().await.remove(&acc_id);
                                            log.log("Sign", &format!("[{}] 官方任务中心已确认登录打卡达成(+100积分)", acc_name), "success", &acc_name).await;
                                        }
                                    }
                                    Err(_) => { /* 复检失败静默，下轮继续 */ }
                                }
                            });
                        }
                    }
                }
            }

            // Per-device observer pulse.
            for desktop in &acc.desktops {
                if !desktop.keepalive_enabled { continue; }
                let key = format!("{}:{}", acc.id, desktop.desktop_id);
                if self.active.read().await.contains(&key) { continue; }
                // 任务会话（签到认领/挂机补挂）期间避让，避免并发会话互相顶掉。
                if self.task_active.read().await.contains(&acc.id) { continue; }
                // Skip if the official client or a web user is currently active.
                // web_active 存的是最后一次心跳时间戳；超过 90 秒无心跳视为标签页已关闭
                // （崩溃/强杀不会触发 beforeunload），避免标记残留一小时抑制保活。
                if let Some(last_beat) = self.web_active.read().await.get(&acc.id) {
                    if now_ts - *last_beat < 90 { continue; }
                }
                // 上次保活失败（如"视讯网关暂未分配"）用短间隔快速重试，网关一旦就绪即可连上；
                // 成功才用配置的长间隔（如 500/1500 秒）。否则失败后要干等一个长周期。
                let failed = desktop.live_metrics.status == "error";
                let interval = if failed { 60 } else { desktop.keepalive_interval.max(10) as i64 };
                let due = parse_datetime(&desktop.last_pulse_at)
                    .map(|t| now_ts - t > interval)
                    .unwrap_or(true);
                if due {
                    self.spawn_pulse(acc.clone(), desktop.clone(), SessionMode::Pulse, 30);
                }
            }

            // Daily scheduled tasks (sign-in / AI chat / one-hour hang / auto redeem).
            // 两种模式：
            //   统一模式(enable_sub_cron=false)：到 execute_time 后一次性执行全部任务，每天每账号一次。
            //   独立模式(enable_sub_cron=true)：每个任务按各自的 cron 表达式独立触发，
            //     依靠 last_sign_time/last_ai_chat_time/last_hang_time/last_redeem_date 做「当天已执行」去重。
            let (execute_time, enable_sub, sub_crons) = {
                let cfg = match self.store.config.read() { Ok(g) => g, Err(e) => e.into_inner() };
                let c = &cfg.settings.cron;
                (c.execute_time.clone(), c.enable_sub_cron,
                 (c.sign_cron.clone(), c.ai_chat_cron.clone(), c.cloud_hang_cron.clone(), c.redeem_cron.clone()))
            };
            let bj = self.beijing_now();
            let current_hm = bj.format("%H:%M").to_string();
            // 用户正在远程桌面里时，定时任务（签到/挂机 Hang 会话）会用同一 deviceCode
            // 建连、把远程挤掉线。此时跳过本轮，待远程结束、心跳过期后补跑。
            let web_busy = self.web_active.read().await.get(&acc.id)
                .map(|last_beat| now_ts - *last_beat < 90).unwrap_or(false);
            if web_busy { /* 远程会话中，避让 */ }
            else if enable_sub {
                // 独立 cron 模式：逐任务判断。每个任务当天只触发一次（按 last_*_time 日期）。
                let specs: [(&str, &str, &str, bool); 4] = [
                    ("sign",   &sub_crons.0, &acc.stats.last_sign_time,    acc.features.auto_sign),
                    ("ai",     &sub_crons.1, &acc.stats.last_ai_chat_time, acc.features.ai_chat),
                    ("hang",   &sub_crons.2, &acc.stats.last_hang_time,    acc.features.cloud_hang),
                    ("redeem", &sub_crons.3, &acc.redeem_config.last_redeem_date,
                        acc.features.auto_redeem && acc.redeem_config.enabled),
                ];
                for (key, expr, last_time, enabled) in specs {
                    if !enabled || expr.trim().is_empty() { continue; }
                    // 当天已成功执行过则跳过（时间字符串以今天日期开头）。
                    if last_time.starts_with(&today) { continue; }
                    // 当天已触发过（含官方尚未落账的情况）则跳过，避免 cron 命中分钟内重复 spawn。
                    let mark = format!("{}:{}", acc.id, key);
                    if self.sub_task_run.read().await.get(&mark).map(|d| d == &today).unwrap_or(false) { continue; }
                    if cron_matches(expr, &bj) {
                        self.sub_task_run.write().await.insert(mark, today.clone());
                        self.spawn_daily_tasks(acc.clone(), self.task_active.clone(), Some(key));
                    }
                }
            } else {
                // 统一模式：时间点已过即补跑（容错：服务器在定点那一分钟抖动没 tick 到也不会漏掉整天任务）。
                let not_run_today = {
                    let cfg = match self.store.config.read() { Ok(g) => g, Err(e) => e.into_inner() };
                    cfg.last_daily_run.get(&acc.id).map(|d| d != &today).unwrap_or(true)
                };
                if current_hm.as_str() >= execute_time.as_str() && not_run_today {
                    {
                        let mut cfg = match self.store.config.write() { Ok(g) => g, Err(e) => e.into_inner() };
                        cfg.last_daily_run.insert(acc.id.clone(), today.clone());
                    }
                    self.store.save();
                    self.spawn_daily_tasks(acc.clone(), self.task_active.clone(), None);
                }
            }
        }
    }

    fn spawn_pulse(&self, acc: Account, desktop: crate::models::Desktop, mode: SessionMode, seconds: u64) {
        let key = format!("{}:{}", acc.id, desktop.desktop_id);
        let acc_id = acc.id.clone();
        let active = self.active.clone();
        let store = self.store.clone();
        let log = self.log.clone();
        let name = desktop.desktop_name.clone();
        let acc_name = acc.name.clone();
        let did = desktop.desktop_id.clone();
        tokio::spawn(async move {
            active.write().await.insert(key.clone());
            let client = CtYunClient::new(acc, store.clone(), log.clone());
            let result = run_keepalive(client, &did, &name, mode, seconds).await;
            match result {
                Ok(r) => {
                    let mut cfg = match store.config.write() { Ok(g) => g, Err(e) => e.into_inner() };
                    if let Some(account) = cfg.accounts.iter_mut().find(|a| a.id == acc_id) {
                        if let Some(d) = account.desktops.iter_mut().find(|d| d.desktop_id == did) {
                            d.last_pulse_at = crate::models::now_beijing();
                            d.live_metrics.status = if r.success { "online".into() } else { "error".into() };
                            d.live_metrics.last_heartbeat_result = r.reason.clone();
                            d.live_metrics.last_heartbeat_time = crate::models::now_beijing();
                            // 桌面卡片显示"累计 N 次"，每轮成功脉冲都要递增，否则一直显示 0。
                            if r.success {
                                d.live_metrics.success_count = d.live_metrics.success_count.saturating_add(1);
                            }
                        }
                        // 成功保活后同步账号级状态：看板顶部在线统计与账号角标读的是
                        // stats.keep_alive_status / last_error，不同步会导致"脉冲成功但仍显示错误"。
                        if r.success {
                            account.stats.keep_alive_status = "online".into();
                            account.stats.last_error.clear();
                            let today = crate::models::today_beijing();
                            if account.stats.last_success_date != today {
                                account.stats.today_success_count = 0;
                                account.stats.last_success_date = today.clone();
                            }
                            account.stats.today_success_count += 1;
                        }
                    }
                    drop(cfg);
                    store.save();
                }
                Err(e) => {
                    // 失败也记录脉冲时间与错误状态：避免 due 恒 true 导致每 10 秒重试风暴，
                    // 同时让看板能显示失败原因（而非脉冲 0 却看不出问题）。
                    {
                        let mut cfg = match store.config.write() { Ok(g) => g, Err(g) => g.into_inner() };
                        if let Some(account) = cfg.accounts.iter_mut().find(|a| a.id == acc_id) {
                            if let Some(d) = account.desktops.iter_mut().find(|d| d.desktop_id == did) {
                                d.last_pulse_at = crate::models::now_beijing();
                                d.live_metrics.status = "error".into();
                                d.live_metrics.last_heartbeat_result = e.clone();
                                d.live_metrics.last_heartbeat_time = crate::models::now_beijing();
                            }
                            account.stats.keep_alive_status = "error".into();
                            account.stats.last_error = e.clone();
                        }
                    }
                    store.save();
                    log.log("Heartbeat", &format!("[{}] 保活会话失败: {}", name, e), "warning", &acc_name).await;
                }
            }
            active.write().await.remove(&key);
        });
    }

    /// 手动"立即执行任务"：对指定账号异步执行全部启用的每日任务，绕过时间点/当天去重。
    pub fn trigger_now(&self, acc: Account) {
        self.spawn_daily_tasks(acc, self.task_active.clone(), None);
    }

    /// only=None 执行全部任务（统一时间点模式）；only=Some("sign"/"ai"/"hang"/"redeem") 只执行单个任务（独立 cron 模式）。
    fn spawn_daily_tasks(&self, acc: Account, task_active: Arc<RwLock<HashSet<String>>>, only: Option<&str>) {
        let store = self.store.clone();
        let log = self.log.clone();
        let pending_sign = self.pending_sign.clone();
        let acc_id = acc.id.clone();
        let acc_name = acc.name.clone();
        let only = only.map(|s| s.to_string());
        tokio::spawn(async move {
            let want = |k: &str| only.as_deref().map(|o| o == k).unwrap_or(true);
            log.log("Task", &format!("[{}] 开始执行每日定时任务{}", acc_name,
                only.as_deref().map(|o| format!("（{}）", o)).unwrap_or_default()), "info", &acc_name).await;
            let mut client = CtYunClient::new(acc, store.clone(), log.clone());

            // 签到认领 + 挂机补挂期间占用账号级任务锁，脉冲避让（对齐上游 isTaskHanging）。
            task_active.write().await.insert(acc_id.clone());
            // 上游: 每个任务执行后都调用 refreshOfficialTasks 同步任务进度 + 积分余额。
            if want("sign") && client.account.features.auto_sign {
                if let Some(d) = client.account.desktops.iter().find(|d| d.task_enabled).cloned() {
                    let did = d.desktop_id.clone();
                    let name = d.desktop_name.clone();
                    if let Ok(r) = run_keepalive(client.clone(), &did, &name, SessionMode::Hang, 300).await {
                        // 签到只需在握手完成时下发 112/104 认领，是否挂满 300s 不影响；
                        // 成败最终以官方任务中心确认为准（见下方 pending_sign 跨分钟复检）。
                        log.log("Task",
                            &format!("[{}] 签到认领会话结束（{}，官方确认以任务中心为准）", acc_name, r.reason),
                            "info", &acc_name).await;
                    }
                }
                let (tk, pt) = client.refresh_official_metrics().await;
                log.log("Task", &format!("[{}] 官方任务/积分同步: 任务={} 积分={}", acc_name, tk, pt),
                    if tk || pt { "success" } else { "warning" }, &acc_name).await;
                // 若官方尚未确认登录打卡，加入待复检队列（跨分钟复检，期间不重复认领）
                let sign_done = {
                    let cfg = match store.config.read() { Ok(g) => g, Err(e) => e.into_inner() };
                    cfg.accounts.iter().find(|a| a.id == acc_id)
                        .map(|a| a.stats.last_sign_time.starts_with(&crate::models::today_beijing()))
                        .unwrap_or(false)
                };
                if !sign_done {
                    let started = Utc::now().timestamp();
                    pending_sign.write().await.insert(acc_id.clone(), started);
                    log.log("Sign", &format!("[{}] 认领已下发，官方暂未确认，已安排跨分钟复检", acc_name), "info", &acc_name).await;
                } else {
                    pending_sign.write().await.remove(&acc_id);
                }
            }

            if want("hang") && client.account.features.cloud_hang {
                if let Some(d) = client.account.desktops.iter().find(|d| d.task_enabled).cloned() {
                    let did = d.desktop_id.clone();
                    let name = d.desktop_name.clone();
                    // 对齐上游 runHangTask: 1 轮 3600s 主挂机 + 最多 3 轮 600s 尾差补挂，
                    // 每轮结束都核对官方"使用1小时"是否 60/60，达成即止。网关提前关闭
                    // （goal_achieved=false）时不再当成"完成"，而是继续补挂，直到官方确认。
                    const MAX_ROUNDS: u32 = 4;
                    let mut hang_done = false;
                    for round in 1..=MAX_ROUNDS {
                        let round_sec: u64 = if round == 1 { 3600 } else { 600 };
                        match run_keepalive(client.clone(), &did, &name, SessionMode::Hang, round_sec).await {
                            Ok(r) => {
                                let label = if r.goal_achieved { "挂满" } else { "中断" };
                                log.log("Task",
                                    &format!("[{}] 挂机第{}/{}轮结束({}s, {}): {}", acc_name, round, MAX_ROUNDS, round_sec, label, r.reason),
                                    if r.goal_achieved { "success" } else { "info" }, &acc_name).await;
                            }
                            Err(e) => log.log("Task",
                                &format!("[{}] 挂机第{}/{}轮异常: {}", acc_name, round, MAX_ROUNDS, e), "warning", &acc_name).await,
                        }
                        // 每轮都重新核对官方任务中心进度，避免只看本地会话状态。
                        match client.refresh_tasks().await {
                            Ok(tasks) => {
                                hang_done = tasks.iter().any(|t|
                                    t.name.contains("使用1小时")
                                        && (t.status == 2 || (t.total > 0 && t.current >= t.total)));
                                if hang_done {
                                    log.log("Task",
                                        &format!("[{}] 官方任务中心确认『使用1小时』已达成(+100积分)", acc_name),
                                        "success", &acc_name).await;
                                    break;
                                }
                                if round < MAX_ROUNDS {
                                    log.log("Task",
                                        &format!("[{}] 『使用1小时』尚未满60分钟，准备尾差补挂第{}轮", acc_name, round + 1),
                                        "info", &acc_name).await;
                                }
                            }
                            Err(_) => { /* 官方状态暂不可读，下轮继续 */ }
                        }
                        // 轮次间短暂冷却，避免连续快速抢占同一桌面。
                        if round < MAX_ROUNDS {
                            tokio::time::sleep(Duration::from_secs(5)).await;
                        }
                    }
                    if !hang_done {
                        log.log("Task",
                            &format!("[{}] 挂机补挂已达上限，『使用1小时』最终以官方任务中心为准", acc_name),
                            "warning", &acc_name).await;
                    }
                }
                // 挂机完成后同步积分与时长
                let _ = client.refresh_official_metrics().await;
            }

            // 签到认领 + 挂机补挂会话已结束，释放任务锁，恢复脉冲保活。
            task_active.write().await.remove(&acc_id);

            if want("ai") && client.account.features.ai_chat {
                match crate::tasks::run_ai_chat(&mut client).await {
                    Ok(msg) => {
                        log.log("Task", &format!("[{}] AI 对话完成: {}", acc_name, msg), "success", &acc_name).await;
                        let _ = client.refresh_official_metrics().await; // 上游: AI 对话后立即刷新积分
                    }
                    Err(e) => log.log("Task", &format!("[{}] AI 对话失败: {}", acc_name, e), "warning", &acc_name).await,
                }
            }

            if want("redeem") && client.account.features.auto_redeem && client.account.redeem_config.enabled {
                let rc = client.account.redeem_config.clone();
                let today = crate::models::today_beijing();
                let due = match rc.schedule_type.as_str() {
                    "daily" => true,
                    "interval_days" => {
                        if rc.last_redeem_date.is_empty() { true }
                        else {
                            let t = chrono::NaiveDate::parse_from_str(&today, "%Y-%m-%d");
                            let l = chrono::NaiveDate::parse_from_str(&rc.last_redeem_date, "%Y-%m-%d");
                            match (t, l) {
                                (Ok(t), Ok(l)) => (t - l).num_days() >= rc.interval_days as i64,
                                _ => true,
                            }
                        }
                    }
                    "monthly_days" | _ => {
                        let cst_day = today.split('-').nth(2).and_then(|s| s.parse::<i32>().ok()).unwrap_or(1);
                        let last_day = last_day_of_month(&today);
                        rc.monthly_days.contains(&cst_day) || (rc.monthly_days.contains(&-1) && cst_day == last_day)
                    }
                };
                if due && rc.last_redeem_date != today {
                    // 上游: 原子多笔（points = costPoints * maxRedeemTimes），且需绑定桌面的商品必须有目标设备。
                    let times = rc.max_redeem_times.max(1) as u64;
                    let needs_desktop = crate::ctyun::CtYunClient::reward_needs_desktop(rc.prod_id, &rc.prod_type);
                    if needs_desktop && rc.desktop_id.is_empty() {
                        log.log("Redeem", &format!("[{}] 自动兑换跳过: 商品 {} 必须绑定云电脑但未配置", acc_name, rc.prod_name), "warning", &acc_name).await;
                    } else {
                        match client.place_order(rc.prod_id, &rc.prod_name, &rc.prod_type, rc.cost_points, times, &rc.desktop_id, 1).await {
                            Ok(msg) => {
                                log.log("Redeem", &format!("[{}] {}", acc_name, msg), "success", &acc_name).await;
                                {
                                    let mut cfg = match store.config.write() { Ok(g) => g, Err(e) => e.into_inner() };
                                    if let Some(a) = cfg.accounts.iter_mut().find(|a| a.id == acc_id) {
                                        a.redeem_config.last_redeem_date = today.clone();
                                    }
                                }
                                store.save();
                                let _ = client.refresh_official_metrics().await; // 上游: 兑换成功后刷新积分
                            }
                            Err(e) => log.log("Redeem", &format!("[{}] 自动兑换失败: {}", acc_name, e), "warning", &acc_name).await,
                        }
                    }
                }
            }
        });
    }
}

/// 返回 YYYY-MM-DD 所在月份的最后一天。
/// 解析 cron 字段（分/时/日/月/周/秒中的一个），判断给定值是否匹配。
/// 支持：* 全匹配、纯数字、a-b 范围、a,b,c 列表、*/n 步长、a-b/n 范围步长。
fn cron_field_matches(field: &str, value: i32) -> bool {
    let field = field.trim();
    if field == "*" || field == "?" { return true; }
    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() { continue; }
        // 拆出步长：expr/n
        let (base, step) = match part.split_once('/') {
            Some((b, s)) => (b, s.parse::<i32>().unwrap_or(1).max(1)),
            None => (part, 1),
        };
        let (lo, hi) = if base == "*" || base == "?" {
            (0, 59)
        } else if let Some((a, b)) = base.split_once('-') {
            (a.parse::<i32>().unwrap_or(0), b.parse::<i32>().unwrap_or(59))
        } else if let Ok(n) = base.parse::<i32>() {
            (n, n)
        } else {
            continue;
        };
        if value >= lo && value <= hi && (value - lo) % step == 0 { return true; }
    }
    false
}

/// 标准 cron 表达式是否在当前北京时间命中（按分钟粒度，10s tick 内只命中一次由调用方去重）。
/// 支持 5 字段 `分 时 日 月 周` 与 6 字段 `秒 分 时 日 月 周`。周字段 0/7 均表示周日。
fn cron_matches(expr: &str, now: &chrono::DateTime<FixedOffset>) -> bool {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() < 5 { return false; }
    // 统一成 6 字段：sec min hour dom mon dow
    let (sec, min, hour, dom, mon, dow) = if fields.len() >= 6 {
        (fields[0], fields[1], fields[2], fields[3], fields[4], fields[5])
    } else {
        // 5 字段 cron 不含秒：秒内任意时刻都算命中（10s tick 落在整分任意秒均可触发），
        // 若写成 sec="0" 则 tick 很难精确落在第 0 秒而漏触发。
        ("*", fields[0], fields[1], fields[2], fields[3], fields[4])
    };
    let weekday = now.weekday().num_days_from_sunday() as i32;
    cron_field_matches(min, now.minute() as i32)
        && cron_field_matches(hour, now.hour() as i32)
        && cron_field_matches(dom, now.day() as i32)
        && cron_field_matches(mon, now.month() as i32)
        && (cron_field_matches(dow, weekday) || cron_field_matches(dow, if weekday == 0 { 7 } else { weekday }))
        && cron_field_matches(sec, now.second() as i32)
}

fn last_day_of_month(date: &str) -> i32 {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 { return 31; }
    let y = parts[0].parse::<i32>().unwrap_or(2026);
    let m = parts[1].parse::<u32>().unwrap_or(1);
    chrono::NaiveDate::from_ymd_opt(y, m, 1)
        .and_then(|d| d.with_month(m + 1))
        .map(|d| d.pred_opt().map(|p| p.day() as i32).unwrap_or(31))
        .unwrap_or(31)
}

fn parse_datetime(value: &str) -> Option<i64> {
    if value.is_empty() { return None; }
    // last_pulse_at 由 now_beijing() 写入，是北京时间本地字符串（无时区后缀）。
    // 必须按 UTC+8 解释后再转 UTC 时间戳，否则 and_utc() 会把它当 UTC，
    // 导致与 now_ts(真 UTC) 相差 -28800 秒，脉冲判因 while 恒为 false 而永不触发。
    let beijing = chrono::FixedOffset::east_opt(8 * 3600)?;
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .ok()
        .and_then(|dt| dt.and_local_timezone(beijing).single())
        .map(|dt| dt.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_datetime_beijing() {
        // 北京时间 2026-11-24 20:00:00 = UTC 2026-11-24 12:00:00
        let ts = parse_datetime("2026-11-24 20:00:00").unwrap();
        let expected = chrono::DateTime::parse_from_rfc3339("2026-11-24T12:00:00Z").unwrap().timestamp();
        assert_eq!(ts, expected, "北京时间必须按 UTC+8 解析，不能当 UTC");
        assert_eq!(parse_datetime(""), None);
        assert_eq!(parse_datetime("bad"), None);
    }

    #[test]
    fn test_cron_field_matches() {
        assert!(cron_field_matches("*", 37));
        assert!(cron_field_matches("5", 5));
        assert!(!cron_field_matches("5", 6));
        assert!(cron_field_matches("1-5", 3));
        assert!(!cron_field_matches("1-5", 9));
        assert!(cron_field_matches("*/15", 30));
        assert!(!cron_field_matches("*/15", 31));
        assert!(cron_field_matches("1,3,5", 5));
        assert!(!cron_field_matches("1,3,5", 4));
        assert!(cron_field_matches("0-30/10", 20));
        assert!(!cron_field_matches("0-30/10", 25));
    }

    #[test]
    fn test_cron_matches_5_and_6_field() {
        let beijing = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
        // 2026-11-25 01:20:30 周三
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 25).unwrap()
            .and_hms_opt(1, 20, 30).unwrap().and_local_timezone(beijing).single().unwrap();
        assert!(cron_matches("20 1 * * *", &now), "5 字段应命中");
        assert!(cron_matches("30 20 1 * * *", &now), "6 字段(秒 分 时 日 月 周)应命中");
        assert!(!cron_matches("21 1 * * *", &now), "分钟不同不应命中");
        assert!(!cron_matches("0 20 1 * * *", &now), "秒不同不应命中(6字段)");
    }
}
