use crate::auth::{extract_token, AuthManager};
use crate::ctyun::{CtYunClient, jstr, UA};
use crate::models::{Account, Session, User};
use crate::scheduler::Scheduler;
use crate::store::{send_webhook, LogHub, Store};
use axum::body::Bytes;
use axum::extract::{FromRequestParts, OriginalUri, Path, Query, State};
use axum::http::{header, request::Parts, HeaderMap, Method, StatusCode};
use axum::response::{sse::Event, IntoResponse, Response, Sse};
use axum::routing::{any, delete, get, post, put};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub log: LogHub,
    pub auth: AuthManager,
    pub scheduler: Scheduler,
}

pub struct AuthUser(pub Session);
pub struct AdminUser(pub Session);

/// 内网免登录模式下使用的虚拟管理员会话
fn anon_admin_session() -> Session {
    Session {
        user_id: "u_admin".into(),
        username: "admin".into(),
        role: "admin".into(),
        max_quota: 999,
        remember: false,
        created_at: 0,
        expires_at: i64::MAX,
    }
}

/// 是否处于免登录模式（鉴权关闭）
fn auth_is_open(state: &AppState) -> bool {
    !state.store.config.read().unwrap_or_else(|e| e.into_inner()).settings.auth_required
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        // 内网免登录：鉴权关闭时以虚拟管理员放行所有 API
        if auth_is_open(state) {
            return Ok(AuthUser(anon_admin_session()));
        }
        let query: Query<HashMap<String, String>> = Query::try_from_uri(&parts.uri)
            .unwrap_or(Query(HashMap::new()));
        let headers = &parts.headers;
        let token = extract_token(
            headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()),
            query.0.get("token").cloned(),
            headers.get(header::COOKIE).and_then(|v| v.to_str().ok()),
        ).ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(json!({"error": "未登录"}))).into_response())?;
        let users = state.store.config.read().unwrap_or_else(|e| e.into_inner()).users.clone();
        let session = state.auth.verify(&token, &users).await
            .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(json!({"error": "会话已失效"}))).into_response())?;
        Ok(AuthUser(session))
    }
}

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let AuthUser(session) = AuthUser::from_request_parts(parts, state).await?;
        if session.role != "admin" {
            return Err((StatusCode::FORBIDDEN, Json(json!({"error": "需要管理员权限"}))).into_response());
        }
        Ok(AdminUser(session))
    }
}

fn can_access(session: &Session, account: &Account) -> bool {
    session.role == "admin" || account.owner_id == session.user_id
}

fn error_json(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn client_for(account: &Account, state: &AppState) -> CtYunClient {
    CtYunClient::new(account.clone(), state.store.clone(), state.log.clone())
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/static/style.css", get(|| async { static_css() }))
        .route("/static/app.js", get(|| async { static_js() }))
        .route("/health", get(|| async { Json(json!({"status":"ok","service":"ctyun-dashboard-rs"})) }))
        .route("/api/auth/config", get(auth_config))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/logout", post(auth_logout))
        .route("/api/auth/register", post(auth_register))
        .route("/api/auth/me", get(auth_me))
        .route("/api/auth/change-password", post(auth_change_password))
        .route("/api/auth/change-username", post(auth_change_username))
        .route("/api/accounts", get(list_accounts).post(create_account))
        .route("/api/accounts/reorder", post(reorder_accounts))
        .route("/api/accounts/{id}", put(update_account).delete(delete_account))
        .route("/api/accounts/{id}/vm-feature", put(update_vm_feature))
        .route("/api/accounts/{id}/desktops", get(list_desktops))
        .route("/api/accounts/{id}/web-launch", get(web_launch))
        .route("/api/accounts/{id}/run/{task}", post(run_task))
        .route("/api/accounts/{id}/order", post(order_reward))
        .route("/api/accounts/{id}/power/{action}", post(power_action))
        .route("/api/accounts/{id}/web-active", post(web_active))
        .route("/api/accounts/{id}/web-close", post(web_close))
        .route("/api/accounts/{id}/sms-captcha", get(sms_captcha))
        .route("/api/accounts/{id}/send-sms", post(send_sms))
        .route("/api/accounts/{id}/bind-sms", post(bind_sms))
        .route("/api/captcha/{phone}", get(get_captcha))
        .route("/api/account/qrcode/generate", post(qrcode_generate))
        .route("/api/account/qrcode/status", get(qrcode_status))
        .route("/api/device/generate", post(generate_device))
        .route("/api/status", get(dashboard_status))
        .route("/api/rewards", get(rewards))
        .route("/api/logs", get(get_logs))
        .route("/api/logs/clear", post(clear_logs))
        .route("/api/logs/stream", get(log_stream))
        .route("/api/settings", get(get_settings).put(update_settings))
        .route("/api/user/notify", get(get_notify).put(update_notify))
        .route("/api/user/notify/test", post(test_notify))
        .route("/api/admin/users", get(admin_users))
        .route("/api/admin/users/{id}/quota", put(admin_quota))
        .route("/api/admin/users/{id}/password", put(admin_password))
        .route("/api/admin/users/{id}", delete(admin_delete_user))
        .route("/api/scheduler/trigger", post(scheduler_trigger))
        .route("/api/config/export", get(config_export))
        .route("/api/config/import", post(config_import))
        .route("/api/keeper/restart", post(keeper_restart))
        .route("/api/upgrade/check", get(upgrade_check))
        .route("/api/upgrade/do", post(upgrade_do))
        .route("/desktop-view", get(desktop_view))
        .route("/api/ctyun-proxy", any(proxy_ctyun_full))
        .route("/ctyun-static/{*path}", get(proxy_static))
        .route("/workers/{*path}", get(proxy_static))
        .with_state(state)
}

// 前端资源在编译期嵌入二进制，单文件部署不依赖工作目录或外部 static/ 目录。
// 这对 OpenWrt 等 procd 从 / 启动的场景至关重要。
const EMBED_INDEX: &str = include_str!("../static/index.html");
const EMBED_CSS: &str   = include_str!("../static/style.css");
const EMBED_JS: &str    = include_str!("../static/app.js");

async fn index() -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], EMBED_INDEX).into_response()
}

fn static_css() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], EMBED_CSS).into_response()
}

fn static_js() -> Response {
    ([(header::CONTENT_TYPE, "application/javascript; charset=utf-8")], EMBED_JS).into_response()
}

// ---------- Authentication ----------

/// 公开端点：前端启动时查询是否需要登录（必须在鉴权提取器之外，否则关闭鉴权时前端无法得知）
async fn auth_config(State(state): State<AppState>) -> Response {
    let (auth_required, allow_registration) = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        (config.settings.auth_required, config.settings.allow_registration)
    };
    Json(json!({
        "auth_required": auth_required,
        "allow_registration": allow_registration
    })).into_response()
}

async fn auth_login(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let username = jstr(&body, "username");
    let password = jstr(&body, "password");
    let remember = body.get("remember").and_then(Value::as_bool).unwrap_or(false);
    let user = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        match config.users.iter().find(|u| u.username == username) {
            Some(u) if u.password_hash == crate::crypto::password_hash(&password) => u.clone(),
            _ => return error_json(StatusCode::UNAUTHORIZED, "用户名或密码错误"),
        }
    };
    let token = state.auth.create_session(&user, remember).await;
    state.log.log("Auth", &format!("用户 {} 登录成功", username), "info", "").await;
    Json(json!({
        "success": true,
        "token": token,
        "user": { "id": user.id, "username": user.username, "role": user.role, "maxQuota": user.max_quota }
    })).into_response()
}

async fn auth_logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    AuthUser(session): AuthUser,
) -> Response {
    let token = extract_token(
        headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()),
        None,
        headers.get(header::COOKIE).and_then(|v| v.to_str().ok()),
    );
    if let Some(t) = token { state.auth.delete(&t).await; }
    let _ = session;
    Json(json!({"success": true})).into_response()
}

async fn auth_register(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        if !config.settings.allow_registration {
            return error_json(StatusCode::FORBIDDEN, "系统未开放注册");
        }
    }
    let username = jstr(&body, "username");
    let password = jstr(&body, "password");
    if username.is_empty() || password.len() < 6 {
        return error_json(StatusCode::BAD_REQUEST, "用户名不能为空且密码至少 6 位");
    }
    let user = {
        let mut config = state.store.config.write().unwrap_or_else(|e| e.into_inner());
        if config.users.iter().any(|u| u.username == username) {
            return error_json(StatusCode::CONFLICT, "用户名已存在");
        }
        let quota = config.settings.default_quota;
        let user = User {
            id: format!("u_{}", uuid::Uuid::new_v4().simple()),
            username: username.clone(),
            password_hash: crate::crypto::password_hash(&password),
            role: "user".into(),
            max_quota: quota,
            avatar: String::new(),
            created_at: crate::models::now_beijing(),
            notify: Default::default(),
        };
        config.users.push(user.clone());
        user
    };
    state.store.save();
    let token = state.auth.create_session(&user, false).await;
    Json(json!({"success": true, "token": token})).into_response()
}

async fn auth_me(AuthUser(session): AuthUser) -> Response {
    Json(json!({
        "id": session.user_id,
        "username": session.username,
        "role": session.role,
        "maxQuota": session.max_quota
    })).into_response()
}

async fn auth_change_password(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Json(body): Json<Value>,
) -> Response {
    let old = jstr(&body, "oldPassword");
    let new = jstr(&body, "newPassword");
    if new.len() < 6 { return error_json(StatusCode::BAD_REQUEST, "新密码至少 6 位"); }
    let mut config = state.store.config.write().unwrap();
    let user = match config.users.iter_mut().find(|u| u.id == session.user_id) {
        Some(u) => u,
        None => return error_json(StatusCode::NOT_FOUND, "用户不存在"),
    };
    if user.password_hash != crate::crypto::password_hash(&old) {
        return error_json(StatusCode::BAD_REQUEST, "原密码错误");
    }
    user.password_hash = crate::crypto::password_hash(&new);
    drop(config);
    state.store.save();
    Json(json!({"success": true})).into_response()
}

async fn auth_change_username(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Json(body): Json<Value>,
) -> Response {
    let new = jstr(&body, "username");
    if new.is_empty() { return error_json(StatusCode::BAD_REQUEST, "用户名不能为空"); }
    let mut config = state.store.config.write().unwrap();
    if let Some(user) = config.users.iter_mut().find(|u| u.id == session.user_id) {
        user.username = new;
    }
    drop(config);
    state.store.save();
    Json(json!({"success": true})).into_response()
}

// ---------- Accounts ----------

fn sanitize_account(acc: &Account, store: &Store, session: &Session) -> Value {
    let mut v = serde_json::to_value(acc).unwrap_or(Value::Null);
    if let Some(obj) = v.as_object_mut() {
        obj.insert("password".into(), json!(store.decrypt_password(&acc.password)));
        obj.insert("pulseIntervalSeconds".into(), json!(acc.keepalive_interval.max(10)));
        // 展示兜底：旧数据里写死成"扫码账号"的名字，用手机号替代，
        // 否则多个扫码账号在目标账号下拉里全部同名、无法区分。
        let name = acc.name.trim();
        if name.is_empty() || name == "扫码账号" {
            let fallback = if acc.user.is_empty() { "云电脑账号".to_string() } else { acc.user.clone() };
            obj.insert("name".into(), json!(fallback));
        }
        let _ = session;
    }
    v
}

async fn list_accounts(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
) -> Response {
    let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
    let accounts: Vec<Value> = config.accounts.iter()
        .filter(|a| can_access(&session, a))
        .map(|a| sanitize_account(a, &state.store, &session))
        .collect();
    Json(accounts).into_response()
}

async fn create_account(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Json(body): Json<Value>,
) -> Response {
    {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        let owned = config.accounts.iter().filter(|a| a.owner_id == session.user_id).count();
        if session.role != "admin" && owned as u32 >= session.max_quota {
            return error_json(StatusCode::FORBIDDEN, "账号配额已用尽");
        }
    }

    let mut account = Account::default();
    account.id = format!("ct_{}", uuid::Uuid::new_v4().simple());
    account.platform = "ctyun".into();
    account.owner_id = session.user_id.clone();
    account.name = {
        let n = jstr(&body, "name");
        if n.is_empty() { jstr(&body, "user") } else { n }
    };
    account.user = jstr(&body, "user");
    account.password = state.store.encrypt_password(&jstr(&body, "password"));
    account.device_code = {
        let dc = jstr(&body, "deviceCode");
        if dc.is_empty() { generate_device_code() } else { dc }
    };
    account.created_at = crate::models::now_beijing();
    account.keepalive_interval = 30;
    // 新增账号默认即启用：与上游 server.js 一致，否则密码/扫码登录的账号
    // 会一直显示"已停用"且保活任务不执行。短信设备绑定是独立的可选授权步骤。
    account.enabled = true;

    if account.user.is_empty() {
        return error_json(StatusCode::BAD_REQUEST, "手机号不能为空");
    }

    let captcha = jstr(&body, "captchaCode");
    let challenge_id = jstr(&body, "challengeId");
    let challenge_code = jstr(&body, "challengeCode");
    if !captcha.is_empty() && !challenge_id.is_empty() {
        let mut client = client_for(&account, &state);
        match client.login_with_captcha(&captcha, &challenge_id, &challenge_code).await {
            Ok(info) => account.saved_login_info = Some(info),
            Err(e) => return error_json(StatusCode::BAD_REQUEST, format!("天翼云登录失败: {}", e)),
        }
    }

    let result = sanitize_account(&account, &state.store, &session);
    {
        let mut config = state.store.config.write().unwrap_or_else(|e| e.into_inner());
        config.accounts.push(account);
    }
    state.store.save();
    let name = jstr(&result, "name");
    state.log.log("System", &format!("新增天翼云账号: {}", name), "success", "").await;
    Json(result).into_response()
}

async fn update_account(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let mut config = state.store.config.write().unwrap();
    let acc = match config.accounts.iter_mut().find(|a| a.id == id && can_access(&session, a)) {
        Some(a) => a,
        None => return error_json(StatusCode::NOT_FOUND, "账号不存在"),
    };
    for key in ["name", "user", "deviceCode", "accountType"] {
        let value = jstr(&body, key);
        if !value.is_empty() {
            match key {
                "name" => acc.name = value,
                "user" => acc.user = value,
                "deviceCode" => acc.device_code = value,
                _ => {}
            }
        }
    }
    let password = jstr(&body, "password");
    if !password.is_empty() {
        acc.password = state.store.encrypt_password(&password);
    }
    if let Some(v) = body.get("enabled").and_then(Value::as_bool) { acc.enabled = v; }
    if let Some(v) = body.get("bound").and_then(Value::as_bool) { acc.bound = v; }
    if let Some(v) = body.get("keepaliveInterval").and_then(Value::as_u64) { acc.keepalive_interval = v; }
    if let Some(v) = body.get("features") {
        if let Ok(f) = serde_json::from_value(v.clone()) { acc.features = f; }
    }
    if let Some(v) = body.get("redeemConfig") {
        if let Ok(r) = serde_json::from_value(v.clone()) { acc.redeem_config = r; }
    }
    if let Some(v) = body.get("displayConfig") {
        if let Ok(d) = serde_json::from_value(v.clone()) { acc.display_config = d; }
    }
    let result = sanitize_account(acc, &state.store, &session);
    drop(config);
    state.store.save();
    Json(result).into_response()
}

async fn delete_account(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path(id): Path<String>,
) -> Response {
    let mut config = state.store.config.write().unwrap();
    let before = config.accounts.len();
    config.accounts.retain(|a| !(a.id == id && can_access(&session, a)));
    if config.accounts.len() == before {
        return error_json(StatusCode::NOT_FOUND, "账号不存在");
    }
    drop(config);
    state.store.save();
    Json(json!({"success": true})).into_response()
}

async fn reorder_accounts(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Json(body): Json<Value>,
) -> Response {
    let ids: Vec<String> = serde_json::from_value(body.get("ids").cloned().unwrap_or(Value::Null))
        .unwrap_or_default();
    let mut config = state.store.config.write().unwrap();
    let mut rest: Vec<Account> = config.accounts.iter().filter(|a| !ids.contains(&a.id)).cloned().collect();
    let mut reordered = Vec::new();
    for id in &ids {
        if let Some(pos) = config.accounts.iter().position(|a| a.id == *id && can_access(&session, a)) {
            reordered.push(config.accounts[pos].clone());
        }
    }
    reordered.append(&mut rest);
    config.accounts = reordered;
    drop(config);
    state.store.save();
    Json(json!({"success": true})).into_response()
}

async fn update_vm_feature(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let feature = jstr(&body, "feature");
    let value = body.get("value").and_then(Value::as_bool).unwrap_or(false);
    let desktop_id = {
        let d = jstr(&body, "vmId");
        if d.is_empty() { jstr(&body, "desktopId") } else { d }
    };
    let mut config = state.store.config.write().unwrap();
    let acc = match config.accounts.iter_mut().find(|a| a.id == id && can_access(&session, a)) {
        Some(a) => a,
        None => return error_json(StatusCode::NOT_FOUND, "账号不存在"),
    };
    for d in &mut acc.desktops {
        if d.desktop_id == desktop_id || d.obj_id == desktop_id {
            match feature.as_str() {
                "keepaliveEnabled" => d.keepalive_enabled = value,
                "taskEnabled" => d.task_enabled = value,
                "autoBootEnabled" => d.auto_boot_enabled = value,
                "keepaliveInterval" => d.keepalive_interval = body.get("value").and_then(Value::as_u64).unwrap_or(30),
                _ => {}
            }
        }
    }
    let result = sanitize_account(acc, &state.store, &session);
    drop(config);
    state.store.save();
    Json(json!({"success": true, "account": result})).into_response()
}

async fn list_desktops(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path(id): Path<String>,
) -> Response {
    let account = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        match config.accounts.iter().find(|a| a.id == id && can_access(&session, a)).cloned() {
            Some(a) => a,
            None => return error_json(StatusCode::NOT_FOUND, "账号不存在"),
        }
    };
    let mut client = client_for(&account, &state);
    match client.fetch_desktops().await {
        Ok(desktops) => Json(desktops).into_response(),
        Err(e) => error_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn web_launch(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let account = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        match config.accounts.iter().find(|a| a.id == id && can_access(&session, a)).cloned() {
            Some(a) => a,
            None => return error_json(StatusCode::NOT_FOUND, "账号不存在"),
        }
    };
    let desktop_id = q.get("desktopId").cloned().unwrap_or_default();
    let mut client = client_for(&account, &state);
    match client.web_launch(&desktop_id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => error_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn run_task(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path((id, task)): Path<(String, String)>,
) -> Response {
    let account = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        match config.accounts.iter().find(|a| a.id == id && can_access(&session, a)).cloned() {
            Some(a) => a,
            None => return error_json(StatusCode::NOT_FOUND, "账号不存在"),
        }
    };
    let mut client = client_for(&account, &state);
    match task.as_str() {
        "sign" => {
            let acc_name = account.name.clone();
            if let Some(d) = account.desktops.first().cloned() {
                let mut c = client.clone();
                let log = state.log.clone();
                let did = d.desktop_id.clone();
                let name = d.desktop_name.clone();
                let task_active = state.scheduler.task_active.clone();
                let acc_id = account.id.clone();
                tokio::spawn(async move {
                    task_active.write().await.insert(acc_id.clone()); // 任务会话期间脉冲避让
                    if let Ok(r) = crate::keeper::run_keepalive(
                        c.clone(), &did, &name, crate::keeper::SessionMode::Hang, 300,
                    ).await {
                        log.log("Sign", &format!("[{}] 手动签到认领会话结束: {}", acc_name, r.reason), "info", &acc_name).await;
                    }
                    let _ = c.refresh_official_metrics().await;
                    task_active.write().await.remove(&acc_id);
                });
            }
            Json(json!({"message": "签到任务已下发"})).into_response()
        }
        "hang" => {
            let acc_name = account.name.clone();
            if let Some(d) = account.desktops.first().cloned() {
                let mut c = client.clone();
                let log = state.log.clone();
                let did = d.desktop_id.clone();
                let name = d.desktop_name.clone();
                let task_active = state.scheduler.task_active.clone();
                let acc_id = account.id.clone();
                tokio::spawn(async move {
                    task_active.write().await.insert(acc_id.clone()); // 任务会话期间脉冲避让
                    // 手动挂机同样走"主会话 + 尾差补挂"循环，直到官方"使用1小时"达成。
                    const MAX_ROUNDS: u32 = 4;
                    let mut done = false;
                    for round in 1..=MAX_ROUNDS {
                        let sec: u64 = if round == 1 { 3600 } else { 600 };
                        match crate::keeper::run_keepalive(
                            c.clone(), &did, &name, crate::keeper::SessionMode::Hang, sec,
                        ).await {
                            Ok(r) => log.log("Task",
                                &format!("[{}] 手动挂机第{}/{}轮({}s, {}): {}", acc_name, round, MAX_ROUNDS, sec,
                                    if r.goal_achieved { "挂满" } else { "中断" }, r.reason),
                                if r.goal_achieved { "success" } else { "info" }, &acc_name).await,
                            Err(e) => log.log("Task",
                                &format!("[{}] 手动挂机第{}轮异常: {}", acc_name, round, e), "warning", &acc_name).await,
                        }
                        if let Ok(tasks) = c.refresh_tasks().await {
                            done = tasks.iter().any(|t| t.name.contains("使用1小时")
                                && (t.status == 2 || (t.total > 0 && t.current >= t.total)));
                            if done {
                                log.log("Task", &format!("[{}] 官方确认『使用1小时』已达成", acc_name), "success", &acc_name).await;
                                break;
                            }
                        }
                        if round < MAX_ROUNDS {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                    let _ = c.refresh_official_metrics().await;
                    task_active.write().await.remove(&acc_id);
                });
            }
            Json(json!({"message": "挂机任务已下发"})).into_response()
        }
        "aiChat" => {
            let acc_name = account.name.clone();
            tokio::spawn(async move {
                match crate::tasks::run_ai_chat(&mut client).await {
                    Ok(msg) => {
                        state.log.log("Task", &msg, "success", &acc_name).await;
                        let _ = client.refresh_official_metrics().await; // 上游: AI 对话后刷新积分
                    }
                    Err(e) => state.log.log("Task", &format!("AI 对话失败: {}", e), "warning", &acc_name).await,
                }
            });
            Json(json!({"message": "AI 对话任务已下发"})).into_response()
        }
        "redeem" => {
            let rc = client.account.redeem_config.clone();
            let times = rc.max_redeem_times.max(1) as u64;
            let result = client.place_order(rc.prod_id, &rc.prod_name, &rc.prod_type, rc.cost_points, times, &rc.desktop_id, 1).await;
            match result {
                Ok(msg) => {
                    let _ = client.refresh_official_metrics().await; // 上游: 兑换后刷新积分
                    Json(json!({"success": true, "message": msg})).into_response()
                }
                Err(e) => error_json(StatusCode::BAD_GATEWAY, e),
            }
        }
        _ => error_json(StatusCode::BAD_REQUEST, "不支持的任务"),
    }
}

async fn order_reward(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let account = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        match config.accounts.iter().find(|a| a.id == id && can_access(&session, a)).cloned() {
            Some(a) => a,
            None => return error_json(StatusCode::NOT_FOUND, "账号不存在"),
        }
    };
    let prod_id = body.get("prodId").and_then(Value::as_u64).unwrap_or(17023101);
    let prod_type = jstr(&body, "prodType");
    let cost_points = body.get("costPoints").and_then(Value::as_u64).unwrap_or(500);
    let times = body.get("times").and_then(Value::as_u64).unwrap_or(1).max(1);
    let desktop_id = jstr(&body, "desktopId");
    let point_type = body.get("pointType").and_then(Value::as_i64).unwrap_or(1) as i32;

    // 上游零预热校验：用缓存积分（stats.points），不在下单前一刻查询以防风控指纹
    let cached_points = account.stats.points;
    let total_cost = cost_points * times;
    if cached_points > 0 && cached_points < total_cost {
        return Json(json!({
            "success": false,
            "error": format!("积分不足：当前拥有 {} 积分，本次兑换需要 {} 积分！", cached_points, total_cost)
        })).into_response();
    }
    // 绑定桌面校验（与上游 rewardNeedsDesktop 一致）
    if crate::ctyun::CtYunClient::reward_needs_desktop(prod_id, &prod_type) && desktop_id.is_empty() {
        return Json(json!({
            "success": false,
            "error": "该商品（升配/数据盘扩容）必须选择绑定的目标云电脑！"
        })).into_response();
    }

    let mut client = client_for(&account, &state);
    let result = client.place_order(
        prod_id, &jstr(&body, "prodName"), &prod_type, cost_points,
        times, &desktop_id, point_type,
    ).await;
    match result {
        Ok(msg) => {
            let _ = client.refresh_official_metrics().await; // 上游: 兑换成功后刷新积分
            Json(json!({"success": true, "message": msg})).into_response()
        }
        Err(e) => Json(json!({"success": false, "error": e})).into_response(),
    }
}

async fn power_action(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path((id, action)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    let desktop_id = jstr(&body, "desktopId");
    let account = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        match config.accounts.iter().find(|a| a.id == id && can_access(&session, a)).cloned() {
            Some(a) => a,
            None => return error_json(StatusCode::NOT_FOUND, "账号不存在"),
        }
    };
    let mut client = client_for(&account, &state);
    match client.control_power(&desktop_id, &action).await {
        Ok(msg) => {
            state.log.log("System", &format!("电源操作 {} 成功: {}", action, msg), "success", &account.name).await;
            Json(json!({"success": true, "message": msg})).into_response()
        }
        Err(e) => Json(json!({"success": false, "error": e})).into_response(),
    }
}

async fn web_active(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path(id): Path<String>,
) -> Response {
    // 校验账号归属，防止普通用户抑制他人账号保活（跨租户 DoS）。
    {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        if !config.accounts.iter().any(|a| a.id == id && can_access(&session, a)) {
            return error_json(StatusCode::NOT_FOUND, "账号不存在");
        }
    }
    // 记录最后一次心跳时间戳（而非固定到期时刻）。scheduler 以 90 秒 TTL 判断
    // 活跃：浏览器崩溃/强杀标签不再发心跳时，标记会自动过期，不长期抑制保活。
    let now = chrono::Utc::now().timestamp();
    state.scheduler.web_active.write().await.insert(id, now);
    Json(json!({"success": true})).into_response()
}

async fn web_close(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path(id): Path<String>,
) -> Response {
    {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        if !config.accounts.iter().any(|a| a.id == id && can_access(&session, a)) {
            return error_json(StatusCode::NOT_FOUND, "账号不存在");
        }
    }
    state.scheduler.web_active.write().await.remove(&id);
    Json(json!({"success": true})).into_response()
}

// ---------- Captcha / QR / SMS / Device ----------

async fn get_captcha(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path(phone): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let _ = session;
    let device_code = q.get("deviceCode").cloned().unwrap_or_else(generate_device_code);
    let account = Account { device_code: device_code.clone(), ..Default::default() };
    let client = client_for(&account, &state);
    match client.get_captcha(&phone, &device_code).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => error_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qrcode_generate(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Json(body): Json<Value>,
) -> Response {
    let _ = session;
    let device_code = {
        let d = jstr(&body, "deviceCode");
        if d.is_empty() { generate_device_code() } else { d }
    };
    let account = Account { device_code: device_code.clone(), ..Default::default() };
    let client = client_for(&account, &state);
    match client.gen_qr_code(&device_code).await {
        Ok(v) => {
            let qr_url = jstr(&v, "qrUrl");
            // Render a local QR code as inline SVG data URL (no external service).
            let qr_image = render_qr_svg(&qr_url);
            Json(json!({
                "success": true,
                "deviceCode": device_code,
                "qrCodeId": jstr(&v, "qrCodeId"),
                "qrUrl": qr_url,
                "qrImage": qr_image
            })).into_response()
        }
        Err(e) => Json(json!({"success": false, "error": e})).into_response(),
    }
}

/// Render text as a self-contained SVG data URL using the qrcode crate.
fn render_qr_svg(text: &str) -> String {
    use qrcode::{Color, QrCode};
    let code = match QrCode::new(text.as_bytes()) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let count = code.width();
    let colors = code.to_colors();
    let cells: Vec<String> = colors.iter().enumerate().filter_map(|(i, color)| {
        if *color == Color::Dark {
            let x = i % count;
            let y = i / count;
            Some(format!("M{} {}h1v1h-1z", x, y))
        } else { None }
    }).collect();
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {0} {0}' shape-rendering='crispEdges'><rect width='100%' height='100%' fill='#fff'/><path d='{1}' fill='#000'/></svg>",
        count, cells.join("")
    );
    format!("data:image/svg+xml;base64,{}", base64::encode(svg.as_bytes()))
}

async fn qrcode_status(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let qr_code_id = q.get("qrCodeId").cloned().unwrap_or_default();
    let device_code = q.get("deviceCode").cloned().unwrap_or_default();
    let account_name = q.get("accountName").cloned().unwrap_or_default();
    let account = Account { device_code, ..Default::default() };
    let mut client = client_for(&account, &state);
    match client.qr_code_status(&qr_code_id).await {
        Ok(status) => {
            let code_status = jstr(&status, "codeStatus");
            let token = jstr(&status, "loginToken");
            if (code_status == "authorize" || code_status == "success") && !token.is_empty() {
                match client.login_by_token(&token).await {
                    Ok(info) => {
                        let phone = jstr(&info, "userName");
                        let dev_code = jstr(&info, "deviceCode");
                        // 占位名（含旧前端写死的"扫码账号"）视为未填，用手机号作为可辨识名称。
                        let base_name = if account_name.trim().is_empty() || account_name.trim() == "扫码账号" {
                            phone.clone()
                        } else {
                            account_name.trim().to_string()
                        };
                        let mut config = state.store.config.write().unwrap_or_else(|e| e.into_inner());

                        // 去重：同一手机号已绑定过则更新凭证，不再重复建号。
                        // 否则轮询期间多次命中或重复扫码会在目标账号下拉里堆出一堆同名账号。
                        if let Some(idx) = config.accounts.iter().position(|a| {
                            a.owner_id == session.user_id && !phone.is_empty() && a.user == phone
                        }) {
                            let existing = &mut config.accounts[idx];
                            if existing.name.trim().is_empty() || existing.name.trim() == "扫码账号" {
                                existing.name = base_name.clone();
                            }
                            if !dev_code.is_empty() { existing.device_code = dev_code; }
                            existing.saved_login_info = Some(info);
                            existing.bound = true;
                            existing.enabled = true;
                            let ret = existing.clone();
                            drop(config);
                            state.store.save();
                            return Json(json!({
                                "success": true,
                                "codeStatus": code_status,
                                "reused": true,
                                "account": sanitize_account(&ret, &state.store, &session)
                            })).into_response();
                        }

                        // 新建：校验配额，且默认启用（与密码登录路径一致）。
                        let owned = config.accounts.iter().filter(|a| a.owner_id == session.user_id).count();
                        if session.role != "admin" && owned as u32 >= session.max_quota {
                            return error_json(StatusCode::FORBIDDEN, "账号配额已用尽");
                        }
                        // 名称去重：若同名账号已存在，附加手机号区分，避免下拉里出现两条一样的。
                        let mut final_name = base_name;
                        if config.accounts.iter().any(|a| a.owner_id == session.user_id && a.name == final_name) {
                            final_name = if phone.is_empty() {
                                format!("{} {}", final_name, &uuid::Uuid::new_v4().simple().to_string()[..6])
                            } else {
                                format!("{} ({})", final_name, phone)
                            };
                        }
                        let mut acc = Account::default();
                        acc.id = format!("ct_{}", uuid::Uuid::new_v4().simple());
                        acc.platform = "ctyun".into();
                        acc.owner_id = session.user_id.clone();
                        acc.name = final_name;
                        acc.user = phone;
                        acc.device_code = dev_code;
                        acc.saved_login_info = Some(info);
                        acc.bound = true;
                        acc.enabled = true;
                        acc.created_at = crate::models::now_beijing();
                        config.accounts.push(acc.clone());
                        drop(config);
                        state.store.save();
                        return Json(json!({
                            "success": true,
                            "codeStatus": code_status,
                            "account": sanitize_account(&acc, &state.store, &session)
                        })).into_response();
                    }
                    Err(e) => return Json(json!({"success": false, "error": e})).into_response(),
                }
            }
            Json(json!({"success": true, "codeStatus": code_status})).into_response()
        }
        Err(e) => Json(json!({"success": false, "error": e})).into_response(),
    }
}

async fn sms_captcha(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path(id): Path<String>,
) -> Response {
    let account = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        match config.accounts.iter().find(|a| a.id == id && can_access(&session, a)).cloned() {
            Some(a) => a,
            None => return error_json(StatusCode::NOT_FOUND, "账号不存在"),
        }
    };
    let client = client_for(&account, &state);
    let ts = chrono::Utc::now().timestamp_millis().to_string();
    let resp = client.http
        .get(format!("{}/api/auth/client/validateCode/captcha", crate::ctyun::BASE))
        .headers(client.base_headers())
        .query(&[("width", "120"), ("height", "40"), ("_t", ts.as_str())])
        .send().await;
    match resp {
        Ok(r) => {
            let bytes = r.bytes().await.unwrap_or_default();
            Json(json!({
                "success": true,
                "captchaImage": format!("data:image/jpeg;base64,{}", base64::encode(bytes)),
                "captchaKey": uuid::Uuid::new_v4().to_string()
            })).into_response()
        }
        Err(e) => error_json(StatusCode::BAD_GATEWAY, e.to_string()),
    }
}

async fn send_sms(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let account = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        match config.accounts.iter().find(|a| a.id == id && can_access(&session, a)).cloned() {
            Some(a) => a,
            None => return error_json(StatusCode::NOT_FOUND, "账号不存在"),
        }
    };
    let client = client_for(&account, &state);
    let url = format!("{}/api/cdserv/client/device/getSmsCode", crate::ctyun::BASE);
    let resp = client.http.get(url)
        .headers(client.base_headers())
        .query(&[
            ("mobilePhone", account.user.as_str()),
            ("captchaCode", jstr(&body, "captchaCode").as_str()),
        ])
        .send().await;
    match resp {
        Ok(_) => Json(json!({"success": true, "message": "短信验证码已发送"})).into_response(),
        Err(e) => Json(json!({"success": false, "message": e.to_string()})).into_response(),
    }
}

async fn bind_sms(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let account = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        match config.accounts.iter().find(|a| a.id == id && can_access(&session, a)).cloned() {
            Some(a) => a,
            None => return error_json(StatusCode::NOT_FOUND, "账号不存在"),
        }
    };
    let client = client_for(&account, &state);
    let form = [
        ("mobilePhone", account.user.clone()),
        ("verificationCode", jstr(&body, "verificationCode")),
        ("hostName", "pc.ctyun.cn".into()),
    ];
    let resp = client.http
        .post(format!("{}/api/cdserv/client/device/binding", crate::ctyun::BASE))
        .headers(client.signed_headers(false))
        .form(&form)
        .send().await;
    match resp {
        Ok(_) => {
            let mut config = state.store.config.write().unwrap();
            if let Some(a) = config.accounts.iter_mut().find(|a| a.id == id) { a.bound = true; }
            drop(config);
            state.store.save();
            Json(json!({"success": true, "message": "设备绑定成功"})).into_response()
        }
        Err(e) => Json(json!({"success": false, "message": e.to_string()})).into_response(),
    }
}

async fn generate_device() -> Response {
    Json(json!({"deviceCode": generate_device_code()})).into_response()
}

fn generate_device_code() -> String {
    let chars = "0123456789abcdef";
    let mut rng = rand::rngs::OsRng;
    use rand::Rng;
    (0..32).map(|_| chars.as_bytes()[rng.gen_range(0..chars.len())] as char).collect()
}

// ---------- Dashboard / Status ----------

async fn dashboard_status(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
) -> Response {
    let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
    let accounts: Vec<&Account> = config.accounts.iter()
        .filter(|a| can_access(&session, a)).collect();
    let online = accounts.iter()
        .filter(|a| a.stats.keep_alive_status == "online")
        .count();
    let today = crate::models::today_beijing();
    let signed_today = accounts.iter().filter(|a| a.stats.last_sign_time.starts_with(&today)).count();

    // 与上游 server.js:4137-4185 对齐：
    // totalEarnedPoints = 各账号「今日已获积分」之和（按官方任务完成状态动态计算）
    // totalPoints = 账户积分余额（stats.points，来自 getUserPoints）
    // 今日已获积分按官方任务进度实时计算，任务名/积分值来自 metrics.officialTasks。
    let mut total_today_earned: u64 = 0;
    let mut details: Vec<Value> = Vec::new();
    for a in &accounts {
        // 上游严格跳过移动云账号（本版本已全删，仅保留 ctyun，此处防御性保留）
        if a.platform != "ctyun" { continue; }

        let tasks = a.desktops.first()
            .map(|d| d.live_metrics.official_tasks.clone())
            .unwrap_or_default();
        let mut acc_today: u64 = 0;
        let mut task_rows: Vec<Value> = Vec::new();

        if tasks.is_empty() {
            // 未拉取到官方任务时，回退到本地落盘的完成标记（积分默认 100，与上游 points||100 一致）
            let fallbacks = [
                ("每日签到", a.stats.last_sign_time.as_str()),
                ("AI 对话", a.stats.last_ai_chat_time.as_str()),
                ("挂机 1 小时", a.stats.last_hang_time.as_str()),
            ];
            for (name, done_time) in fallbacks {
                let done = done_time.starts_with(&today);
                if done { acc_today += 100; }
                task_rows.push(json!({
                    "name": name,
                    "points": if done { 100 } else { 0 },
                    "completed": done,
                    "completedAt": if done { done_time.to_string() } else { String::new() },
                    "progress": if done { "" } else { "0/1" }
                }));
            }
        } else {
            for t in &tasks {
                let completed_at = if t.name.contains("登录AI云电脑") {
                    a.stats.last_sign_time.clone()
                } else if t.name.contains("AI对话") {
                    a.stats.last_ai_chat_time.clone()
                } else if t.name.contains("使用1小时") {
                    a.stats.last_hang_time.clone()
                } else { String::new() };

                let is_done = t.status == 2 || (t.total > 0 && t.current >= t.total);
                let val = if t.points == 0 { 100 } else { t.points }; // 上游 t.points || 100
                if is_done {
                    acc_today += val;
                    task_rows.push(json!({
                        "name": t.name,
                        "points": val,
                        "completed": true,
                        "completedAt": if completed_at.is_empty() { format!("{} 08:00:00", today) } else { completed_at },
                        "progress": ""
                    }));
                } else {
                    task_rows.push(json!({
                        "name": t.name,
                        "points": 0,
                        "completed": false,
                        "completedAt": "",
                        "progress": format!("{}/{}", t.current, t.total)
                    }));
                }
            }
        }
        total_today_earned += acc_today;
        let acc_display_name = {
            let n = a.name.trim();
            if n.is_empty() || n == "扫码账号" {
                if a.user.is_empty() { "云电脑账号".to_string() } else { a.user.clone() }
            } else {
                n.to_string()
            }
        };
        details.push(json!({
            "accountId": a.id,
            "accountName": acc_display_name,
            "todayPoints": acc_today,
            "totalPoints": a.stats.points,
            "tasks": task_rows
        }));
    }
    Json(json!({
        "accountsTotal": accounts.len(),
        "onlineKeepAlive": online,
        "signedToday": signed_today,
        "totalEarnedPoints": total_today_earned,
        "pointsDetails": details,
        "currentTime": crate::models::now_beijing(),
        "isGuest": false
    })).into_response()
}

async fn rewards(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
) -> Response {
    let account = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        config.accounts.iter().find(|a| can_access(&session, a)).cloned()
    };
    let account = match account {
        Some(a) => a,
        None => return Json(Vec::<Value>::new()).into_response(),
    };
    let mut client = client_for(&account, &state);
    match client.get_rewards().await {
        Ok(list) => {
            if let Ok(points) = client.get_points().await {
                let mut config = state.store.config.write().unwrap();
                if let Some(a) = config.accounts.iter_mut().find(|a| a.id == account.id) {
                    a.stats.points = points;
                }
                drop(config);
                state.store.save();
            }
            Json(list).into_response()
        }
        Err(e) => Json(json!({"error": e})).into_response(),
    }
}

// ---------- Logs ----------

async fn get_logs(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
) -> Response {
    let logs = state.log.recent.read().await;
    let visible: Vec<Value> = logs.iter()
        .filter(|e| session.role == "admin" || e.account_name.is_empty() || e.account_name == session.username)
        .take(300)
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect();
    Json(visible).into_response()
}

async fn clear_logs(State(state): State<AppState>, _admin: AdminUser) -> Response {
    state.log.recent.write().await.clear();
    let today = crate::models::today_beijing();
    let path = state.log.data_dir.join("logs").join(format!("dashboard-{}.jsonl", today));
    let _ = std::fs::write(path, "");
    Json(json!({"success": true})).into_response()
}

async fn log_stream(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let users = state.store.config.read().unwrap_or_else(|e| e.into_inner()).users.clone();
    let token = q.get("token").cloned();
    let session = match token {
        Some(t) => match state.auth.verify(&t, &users).await {
            Some(s) => s,
            None => return error_json(StatusCode::UNAUTHORIZED, "会话已失效"),
        },
        None => return error_json(StatusCode::UNAUTHORIZED, "缺少 token"),
    };
    let rx = state.log.tx.subscribe();
    let is_admin = session.role == "admin";
    let session_username = session.username.clone();
    // 回放日志同样要按账号名过滤，否则普通用户可读取最近 80 条他人日志。
    let replay: Vec<crate::models::LogEntry> = state.log.recent.read().await
        .iter().rev()
        .filter(|e| is_admin || e.account_name.is_empty() || e.account_name == session_username)
        .take(80).cloned().collect();
    let replay_stream = tokio_stream::iter(replay.into_iter().map(|entry| {
        Ok(Event::default().data(serde_json::to_string(&entry).unwrap_or_default()))
    }));
    let live = BroadcastStream::new(rx).filter_map(move |msg| {
        match msg {
            Ok(entry) => {
                if !is_admin && !entry.account_name.is_empty() && entry.account_name != session_username {
                    return None;
                }
                Some(Ok::<Event, std::convert::Infallible>(Event::default().data(serde_json::to_string(&entry).unwrap_or_default())))
            }
            Err(_) => None,
        }
    });
    let stream = replay_stream.chain(live);
    Sse::new(stream).into_response()
}

// ---------- Settings / Admin / System ----------

async fn get_settings(State(state): State<AppState>, _admin: AdminUser) -> Response {
    let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
    Json(serde_json::to_value(&config.settings).unwrap_or(Value::Null)).into_response()
}

async fn update_settings(
    State(state): State<AppState>,
    _admin: AdminUser,
    Json(body): Json<Value>,
) -> Response {
    let mut config = state.store.config.write().unwrap();
    if let Ok(s) = serde_json::from_value(body) { config.settings = s; }
    let settings = config.settings.clone();
    drop(config);
    state.store.save();
    Json(settings).into_response()
}

async fn get_notify(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
) -> Response {
    let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
    let notify = config.users.iter().find(|u| u.id == session.user_id)
        .map(|u| u.notify.clone()).unwrap_or_default();
    Json(notify).into_response()
}

async fn update_notify(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Json(body): Json<Value>,
) -> Response {
    let mut config = state.store.config.write().unwrap();
    if let Some(user) = config.users.iter_mut().find(|u| u.id == session.user_id) {
        if let Ok(n) = serde_json::from_value(body.clone()) { user.notify = n; }
    }
    drop(config);
    state.store.save();
    Json(json!({"success": true})).into_response()
}

async fn test_notify(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
) -> Response {
    let notify = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        config.users.iter().find(|u| u.id == session.user_id)
            .map(|u| u.notify.clone()).unwrap_or_default()
    };
    match send_webhook(&notify.webhook_url, &notify.channel, "测试通知", "这是一条来自云电脑控制中心的测试消息").await {
        Ok(_) => Json(json!({"success": true, "message": "测试消息已发送"})).into_response(),
        Err(e) => Json(json!({"success": false, "error": e})).into_response(),
    }
}

async fn admin_users(State(state): State<AppState>, _admin: AdminUser) -> Response {
    let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
    let users: Vec<Value> = config.users.iter().map(|u| json!({
        "id": u.id, "username": u.username, "role": u.role,
        "maxQuota": u.max_quota, "createdAt": u.created_at
    })).collect();
    Json(users).into_response()
}

async fn admin_quota(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let quota = body.get("maxQuota").and_then(Value::as_u64).unwrap_or(2) as u32;
    let mut config = state.store.config.write().unwrap();
    if let Some(u) = config.users.iter_mut().find(|u| u.id == id) { u.max_quota = quota; }
    drop(config);
    state.store.save();
    Json(json!({"success": true})).into_response()
}

async fn admin_password(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let password = jstr(&body, "password");
    let mut config = state.store.config.write().unwrap();
    if let Some(u) = config.users.iter_mut().find(|u| u.id == id) {
        u.password_hash = crate::crypto::password_hash(&password);
    }
    drop(config);
    state.store.save();
    Json(json!({"success": true})).into_response()
}

async fn admin_delete_user(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(id): Path<String>,
) -> Response {
    let mut config = state.store.config.write().unwrap();
    config.users.retain(|u| u.id != id);
    config.accounts.retain(|a| a.owner_id != id);
    drop(config);
    state.store.save();
    Json(json!({"success": true})).into_response()
}

async fn scheduler_trigger(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
) -> Response {
    let accounts: Vec<Account> = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        config.accounts.iter()
            .filter(|a| a.enabled && can_access(&session, a))
            .filter(|a| a.features.auto_sign || a.features.ai_chat || a.features.cloud_hang
                || (a.features.auto_redeem && a.redeem_config.enabled))
            .cloned().collect()
    };
    if accounts.is_empty() {
        return error_json(StatusCode::BAD_REQUEST, "没有启用了任务的账号");
    }
    let count = accounts.len();
    for acc in accounts {
        state.scheduler.trigger_now(acc);
    }
    Json(json!({"success": true, "message": format!("已下发 {} 个账号的定时任务，请到实时日志查看进度", count)})).into_response()
}

async fn config_export(State(state): State<AppState>, _admin: AdminUser) -> Response {
    let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
    let json = serde_json::to_string_pretty(&*config).unwrap_or_default();
    Json(json!({"config": json, "masterKey": state.store.master_key_hex()})).into_response()
}

async fn config_import(
    State(state): State<AppState>,
    _admin: AdminUser,
    Json(body): Json<Value>,
) -> Response {
    let config_str = jstr(&body, "config");
    match serde_json::from_str::<crate::models::AppConfig>(&config_str) {
        Ok(config) => {
            let mut guard = state.store.config.write().unwrap();
            *guard = config;
            drop(guard);
            state.store.save();
            Json(json!({"success": true, "message": "配置已导入"})).into_response()
        }
        Err(e) => error_json(StatusCode::BAD_REQUEST, format!("配置格式错误: {}", e)),
    }
}

async fn keeper_restart(State(state): State<AppState>, _admin: AdminUser) -> Response {
    state.scheduler.active.write().await.clear();
    state.scheduler.task_active.write().await.clear();
    state.log.log("System", "保活调度器已重启", "success", "").await;
    Json(json!({"success": true})).into_response()
}

async fn desktop_view(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q.get("desktopId").cloned().unwrap_or_default();
    let token = q.get("token").cloned().unwrap_or_default();
    let account = {
        let config = state.store.config.read().unwrap_or_else(|e| e.into_inner());
        config.accounts.iter().find(|a| can_access(&session, a)).cloned()
    };
    let account = match account { Some(a) => a, None => return error_json(StatusCode::NOT_FOUND, "账号不存在") };
    let account_id = account.id.clone();
    let mut client = client_for(&account, &state);
    match crate::desktop_view::desktop_view_html(&mut client, &id, &account_id, &token).await {
        Ok(html) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            html,
        ).into_response(),
        Err(e) => error_json(StatusCode::BAD_GATEWAY, e),
    }
}

/// 代理天翼云静态资源（同源加载官方控制台所需）。
/// 静态资源由 <script>/<img> 直接请求，无法携带 token，故不鉴权；
/// 但路径严格固定转发到 pc.ctyun.cn，不接受任意 URL，无 SSRF 风险。
async fn proxy_static(
    State(state): State<AppState>,
    OriginalUri(original): OriginalUri,
) -> Response {
    let path = original.path().to_string();
    let target = if path.starts_with("/workers/") {
        format!("https://pc.ctyun.cn{}", path)
    } else if path.starts_with("/ctyun-static/") {
        format!("https://pc.ctyun.cn/{}", path.trim_start_matches("/ctyun-static/"))
    } else {
        return error_json(StatusCode::NOT_FOUND, "invalid static path");
    };
    proxy_fetch(&state, &target).await
}

/// 通用天翼云代理转发：支持任意方法/请求体/header，供 fetch/XHR 劫持调用。
/// 不校验 dashboard 会话：这是官方页面自身发起的天翼云 API 请求，鉴权靠
/// 天翼云的 ctg-* 签名 header（由前端用 authData 实时生成），我们只做透传。
/// 安全由 is_allowed_ctyun_target 白名单保证（仅 *.ctyun.cn 的 https）。
async fn proxy_ctyun_full(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let target = q.get("target").cloned().unwrap_or_default();
    if !is_allowed_ctyun_target(&target) {
        return error_json(StatusCode::FORBIDDEN, "目标地址不在白名单内");
    }
    proxy_fetch_with(&state, &target, method, &headers, &body).await
}

fn is_allowed_ctyun_target(target: &str) -> bool {
    url::Url::parse(target).ok()
        .and_then(|u| {
            if u.scheme() != "https" { return None; }
            u.host_str().map(|h| h == "pc.ctyun.cn" || h == "desk.ctyun.cn" || h.ends_with(".ctyun.cn"))
        })
        .unwrap_or(false)
}

async fn proxy_fetch(state: &AppState, target: &str) -> Response {
    proxy_fetch_with(state, target, Method::GET, &HeaderMap::new(), &Bytes::new()).await
}

async fn proxy_fetch_with(_state: &AppState, target: &str, method: Method, headers: &HeaderMap, body: &Bytes) -> Response {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let mut req = client
        .request(method, target)
        .header("User-Agent", UA)
        .header("Origin", "https://pc.ctyun.cn")
        .header("Referer", "https://pc.ctyun.cn/");
    // 透传业务 header（跳过 hop-by-hop 与主机相关字段）
    for (k, v) in headers.iter() {
        let lk = k.as_str().to_lowercase();
        if matches!(lk.as_str(), "host" | "origin" | "referer" | "content-length" | "authorization" | "connection" | "accept-encoding") {
            continue;
        }
        if let Ok(vs) = v.to_str() {
            req = req.header(k.clone(), vs);
        }
    }
    if !body.is_empty() {
        req = req.body(body.clone());
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let content_type = resp.headers().get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
            let bytes = resp.bytes().await.unwrap_or_default();
            (
                status,
                [
                    (header::CONTENT_TYPE, content_type.parse().unwrap_or(header::HeaderValue::from_static("application/octet-stream"))),
                    (header::ACCESS_CONTROL_ALLOW_ORIGIN, header::HeaderValue::from_static("*")),
                    (header::CACHE_CONTROL, header::HeaderValue::from_static("no-store")),
                ],
                bytes,
            ).into_response()
        }
        Err(e) => error_json(StatusCode::BAD_GATEWAY, e.to_string()),
    }
}

/* ============================================================
   在线升级：查 GitHub Release → 下载新二进制 → MD5 校验 → 原子替换 → 自重启
   数据目录（账号/token/配置）全程不碰，仅替换程序本体。
   ============================================================ */

const UPGRADE_REPO: &str = "likz0421/ctyun-dashboard-rs";

/// 把编译目标架构映射到 Release 资产名后缀
fn upgrade_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "arm" => "armv7",
        other => other,
    }
}

/// 当前正在运行的可执行文件绝对路径
fn current_exe_path() -> Result<std::path::PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("无法定位当前程序路径: {}", e))
}

/// 读取 GitHub latest release 的元信息（tag / 发布时间 / 资产列表）
async fn fetch_latest_release() -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("https://api.github.com/repos/{}/releases/latest", UPGRADE_REPO);
    let resp = client
        .get(&url)
        .header("User-Agent", "ctyun-dashboard-upgrade")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("连接 GitHub 失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub API 返回 {}", resp.status()));
    }
    resp.json::<Value>().await.map_err(|e| e.to_string())
}

async fn upgrade_check(_admin: AdminUser) -> Response {
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let arch = upgrade_arch();
    let asset_name = format!("ctyun-dashboard-{}-musl", arch);

    // 读取本地二进制内容，用于 MD5 精确比对。
    // 注意：不能用文件 mtime 判断版本——重装/复制会刷新 mtime，导致误判"已是最新"。
    let local_md5 = match current_exe_path().ok().and_then(|p| std::fs::read(&p).ok()) {
        Some(bytes) => crate::crypto::md5_hex(&bytes),
        None => String::new(),
    };

    let latest = match fetch_latest_release().await {
        Ok(v) => v,
        Err(e) => {
            return Json(json!({
                "current_version": current_version,
                "arch": arch,
                "asset": asset_name,
                "reachable": false,
                "error": e,
                "update_available": false
            })).into_response()
        }
    };
    let tag = jstr(&latest, "tag_name");
    let published = jstr(&latest, "published_at");

    // 找到当前架构二进制资产与其 .md5 资产
    let mut remote_updated = String::new();
    let mut remote_size: u64 = 0;
    let mut remote_md5 = String::new();
    let mut md5_urls: Vec<String> = Vec::new();
    if let Some(assets) = latest.get("assets").and_then(|a| a.as_array()) {
        for a in assets {
            let name = jstr(a, "name");
            if name == asset_name {
                remote_updated = jstr(a, "updated_at");
                remote_size = a.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
            } else if name == format!("{}.md5", asset_name) {
                // API 资产端点可达性更好（国内直链常失败），优先；再试直链与常见镜像
                if let Some(id) = a.get("id").and_then(|v| v.as_u64()) {
                    md5_urls.push(format!("https://api.github.com/repos/{}/releases/assets/{}", UPGRADE_REPO, id));
                }
                let direct = jstr(a, "browser_download_url");
                if !direct.is_empty() {
                    md5_urls.push(direct.clone());
                    md5_urls.push(format!("https://ghfast.top/{}", direct));
                    md5_urls.push(format!("https://ghproxy.net/{}", direct));
                }
            }
        }
    }

    // 拉取远端 .md5（少量字节），逐个源尝试；失败则回退到 tag 版本比较
    if !md5_urls.is_empty() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(12))
            .build()
            .ok();
        if let Some(client) = client {
            for url in &md5_urls {
                if let Ok(resp) = client.get(url)
                    .header("User-Agent", "ctyun-dashboard-upgrade")
                    .header("Accept", "application/octet-stream")
                    .send().await
                {
                    if resp.status().is_success() {
                        if let Ok(text) = resp.text().await {
                            let v = text.split_whitespace().next().unwrap_or("").trim().to_ascii_lowercase();
                            // .md5 文件内容形如 "<hash>  filename"，取 32 位十六进制串
                            if v.len() == 32 && v.chars().all(|c| c.is_ascii_hexdigit()) {
                                remote_md5 = v;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    let (update_available, method) = if !remote_md5.is_empty() && !local_md5.is_empty() {
        (local_md5 != remote_md5, "md5")
    } else {
        // 回退：比较语义版本（去掉 tag 前缀 v），本地版本号低于远端即提示更新
        let norm = |s: &str| s.trim().trim_start_matches('v').to_string();
        let smaller = {
            let parse = |s: &str| -> Vec<u64> {
                norm(s).split('.').map(|x| x.parse::<u64>().unwrap_or(0)).collect()
            };
            let a = parse(&current_version);
            let b = parse(&tag);
            a < b
        };
        (smaller, "version")
    };

    Json(json!({
        "current_version": current_version,
        "latest_version": tag,
        "published_at": published,
        "arch": arch,
        "asset": asset_name,
        "asset_size": remote_size,
        "remote_updated_at": remote_updated,
        "local_md5": local_md5,
        "remote_md5": remote_md5,
        "method": method,
        "reachable": true,
        "update_available": update_available
    })).into_response()
}

async fn upgrade_do(State(state): State<AppState>, _admin: AdminUser) -> Response {
    let arch = upgrade_arch();
    let asset_name = format!("ctyun-dashboard-{}-musl", arch);
    let exe = match current_exe_path() {
        Ok(p) => p,
        Err(e) => return error_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let exe_dir = match exe.parent() {
        Some(d) => d.to_path_buf(),
        None => return error_json(StatusCode::INTERNAL_SERVER_ERROR, "无法确定程序目录"),
    };
    let exe_str = exe.to_string_lossy().to_string();

    state.log.log("Upgrade", &format!("开始在线升级：{}", asset_name), "info", "").await;

    // 1) 拿 latest release，解析资产下载地址与 md5 地址
    let latest = match fetch_latest_release().await {
        Ok(v) => v,
        Err(e) => {
            state.log.log("Upgrade", &format!("升级失败：{}", e), "error", "").await;
            return error_json(StatusCode::BAD_GATEWAY, e);
        }
    };
    let (download_urls, md5_url) = {
        let assets = latest.get("assets").and_then(|a| a.as_array()).cloned().unwrap_or_default();
        let bin = assets.iter().find(|a| jstr(a, "name") == asset_name);
        let md5 = assets.iter().find(|a| jstr(a, "name") == format!("{}.md5", asset_name));
        let bin = match bin {
            Some(a) => a,
            None => return error_json(StatusCode::NOT_FOUND, format!("Release 中未找到资产 {}", asset_name)),
        };
        // 优先走 api.github.com 资产端点（可达性好、支持认证与重定向），失败回退直链
        let mut urls = Vec::new();
        if let Some(id) = bin.get("id").and_then(|v| v.as_u64()) {
            urls.push(format!("https://api.github.com/repos/{}/releases/assets/{}", UPGRADE_REPO, id));
        }
        let direct = jstr(bin, "browser_download_url");
        if !direct.is_empty() { urls.push(direct); }
        let m = md5.map(|a| jstr(a, "browser_download_url"));
        (urls, m)
    };

    // 2) 下载到 exe 同目录的临时文件（保证 rename 同文件系统，原子覆盖）
    let tmp_path = exe_dir.join(format!(".ctyun-upgrade-{}-{}", asset_name, std::process::id()));
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build() {
        Ok(c) => c,
        Err(e) => return error_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    // 逐个下载源尝试；用流式 chunk 累积，避免大文件一次性解码失败
    let mut bytes: Option<Vec<u8>> = None;
    let mut last_err = String::new();
    for url in &download_urls {
        match client.get(url)
            .header("User-Agent", "ctyun-dashboard-upgrade")
            .header("Accept", "application/octet-stream")
            .send().await {
            Ok(resp) if resp.status().is_success() => {
                let mut stream = resp.bytes_stream();
                let mut buf = Vec::new();
                let mut ok = true;
                while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
                    match chunk {
                        Ok(c) => buf.extend_from_slice(&c),
                        Err(e) => { last_err = format!("读取新版本失败: {}", e); ok = false; break; }
                    }
                }
                if ok {
                    bytes = Some(buf);
                    break;
                }
            }
            Ok(resp) => last_err = format!("下载失败，HTTP {}", resp.status()),
            Err(e) => last_err = format!("下载失败: {}", e),
        }
    }
    let bytes = match bytes {
        Some(b) => b,
        None => return error_json(StatusCode::BAD_GATEWAY, last_err),
    };
    if bytes.len() < 1024 * 100 {
        let _ = std::fs::remove_file(&tmp_path);
        return error_json(StatusCode::BAD_GATEWAY, "下载文件异常过小，已放弃升级");
    }
    if std::fs::write(&tmp_path, &bytes).is_err() {
        return error_json(StatusCode::INTERNAL_SERVER_ERROR, "写入临时文件失败（程序目录可能不可写）");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755));
    }

    // 3) 可选 MD5 校验：下载 .md5，比对本地实际值
    if let Some(md5_url) = md5_url.filter(|u| !u.is_empty()) {
        if let Ok(resp) = client.get(&md5_url).header("User-Agent", "ctyun-dashboard-upgrade").send().await {
            if resp.status().is_success() {
                if let Ok(text) = resp.text().await {
                    let expect = text.split_whitespace().next().unwrap_or("").trim().to_ascii_lowercase();
                    let actual = crate::crypto::md5_hex(&bytes);
                    if !expect.is_empty() && expect != actual {
                        let _ = std::fs::remove_file(&tmp_path);
                        state.log.log("Upgrade", &format!("升级中止：MD5 不匹配 期望{} 实际{}", expect, actual), "error", "").await;
                        return error_json(StatusCode::BAD_GATEWAY, "MD5 校验失败，已放弃升级");
                    }
                    state.log.log("Upgrade", "MD5 校验通过", "success", "").await;
                }
            }
        }
    }

    // 4) 原子替换当前 exe（Linux 下 rename 覆盖正在运行的文件是安全的）
    if std::fs::rename(&tmp_path, &exe).is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        return error_json(StatusCode::INTERNAL_SERVER_ERROR, "替换程序文件失败（请确认以服务用户身份对程序目录有写权限）");
    }

    let _ = state.log.log("Upgrade", &format!("新版本已写入 {}，即将重启生效", exe_str), "success", "").await;

    // 5) 延迟退出：给响应留出返回时间，随后由 systemd(Restart=always) / procd(respawn) 拉起新版本
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        std::process::exit(0);
    });

    Json(json!({
        "success": true,
        "message": "升级包已安装，服务正在重启，请稍后刷新页面",
        "asset": asset_name,
        "restarting": true
    })).into_response()
}
