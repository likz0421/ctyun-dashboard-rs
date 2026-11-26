use crate::crypto::{md5_hex, sha256_hex};
use crate::models::{Account, Desktop, LiveMetrics, TaskProgress};
use crate::store::{LogHub, Store};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};
use std::time::Duration;

pub const BASE: &str = "https://desk.ctyun.cn:8810";
pub const SELFORDER: &str = "https://desk.ctyun.cn/selforder";
pub const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36";
pub const VERSION: &str = "103020001";
pub const DEVICE_TYPE: &str = "60";

pub fn jstr(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

pub fn jnum(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

pub fn jbool(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// 去除 HTML 标签（对齐上游 replace(/<[^>]+>/g, ' ')）。
fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => { in_tag = false; out.push(' '); }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.shrink_to_fit();
    out
}

fn header_value(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static(""))
}

#[derive(Clone)]
pub struct CtYunClient {
    pub account: Account,
    pub login_info: Option<Value>,
    pub store: Store,
    pub log: LogHub,
    pub http: reqwest::Client,
}

impl CtYunClient {
    pub fn new(account: Account, store: Store, log: LogHub) -> Self {
        let login_info = account.saved_login_info.clone();
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { account, login_info, store, log, http }
    }

    async fn info(&self, msg: &str, level: &str) {
        self.log.log("CtYun", msg, level, &self.account.name).await;
    }

    pub fn base_headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        let _ = h.insert("User-Agent", HeaderValue::from_static(UA));
        let _ = h.insert("ctg-devicetype", HeaderValue::from_static(DEVICE_TYPE));
        let _ = h.insert("ctg-version", HeaderValue::from_static(VERSION));
        let _ = h.insert("ctg-devicecode", header_value(&self.account.device_code));
        h
    }

    /// Signed headers identical to the upstream `getSignedHeaders()`.
    pub fn signed_headers(&self, json_body: bool) -> HeaderMap {
        let mut h = self.base_headers();
        if let Some(info) = &self.login_info {
            let ts = chrono::Utc::now().timestamp_millis().to_string();
            let tenant = jstr(info, "tenantId");
            let user = jstr(info, "userId");
            let secret = jstr(info, "secretKey");
            let raw = format!("{}{}{}{}{}{}{}", DEVICE_TYPE, ts, tenant, ts, user, VERSION, secret);
            let sig = md5_hex(raw.as_bytes());
            let _ = h.insert("ctg-userid", header_value(&user));
            let _ = h.insert("ctg-tenantid", header_value(&tenant));
            let _ = h.insert("ctg-timestamp", header_value(&ts));
            let _ = h.insert("ctg-requestid", header_value(&ts));
            let _ = h.insert("ctg-signaturestr", header_value(&sig));
            let token = jstr(info, "token");
            if !token.is_empty() {
                let _ = h.insert("cookie", header_value(&format!("token={}", token)));
            }
        }
        let _ = h.insert("referer", HeaderValue::from_static("https://pc.ctyun.cn/"));
        if json_body {
            let _ = h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        h
    }

    fn persist_login_info(&self, info: Value) {
        let mut config = self.store.config.write().unwrap();
        if let Some(acc) = config.accounts.iter_mut().find(|a| a.id == self.account.id) {
            acc.saved_login_info = Some(info.clone());
            acc.session_expired = false;
            acc.bound = true;
        }
        drop(config);
        self.store.save();
    }

    fn mark_session_expired(&self, reason: &str) {
        let mut config = self.store.config.write().unwrap();
        if let Some(acc) = config.accounts.iter_mut().find(|a| a.id == self.account.id) {
            acc.session_expired = true;
            acc.stats.keep_alive_status = "offline".into();
            acc.stats.last_error = reason.into();
        }
        drop(config);
        self.store.save();
    }

    /// Password + captcha login against the official client endpoint.
    pub async fn login_with_captcha(
        &mut self,
        captcha_code: &str,
        challenge_id: &str,
        challenge_code: &str,
    ) -> Result<Value, String> {
        let password = self.store.decrypt_password(&self.account.password);
        let mut headers = self.base_headers();
        let _ = headers.insert("referer", HeaderValue::from_static("https://pc.ctyun.cn/"));
        let _ = headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/x-www-form-urlencoded"));

        let form = [
            ("userAccount", self.account.user.clone()),
            ("password", sha256_hex(format!("{}{}", password, challenge_code))),
            ("sha256Password", sha256_hex(format!("{}{}", sha256_hex(&password), challenge_code))),
            ("challengeId", challenge_id.to_string()),
            ("captchaCode", captcha_code.to_string()),
            ("deviceCode", self.account.device_code.clone()),
            ("deviceName", "Chrome浏览器".into()),
            ("deviceType", DEVICE_TYPE.into()),
            ("deviceModel", "Windows NT 10.0; Win64; x64".into()),
            ("appVersion", "3.2.0".into()),
            ("sysVersion", "Windows NT 10.0; Win64; x64".into()),
            ("clientVersion", VERSION.into()),
        ];

        let resp = self.http
            .post(format!("{}/api/auth/client/login", BASE))
            .headers(headers)
            .form(&form)
            .send().await.map_err(|e| format!("登录请求失败: {}", e))?;
        let json: Value = resp.json().await.map_err(|e| format!("解析登录响应失败: {}", e))?;
        if jnum(&json, "code") == 0 && json.get("data").is_some() {
            let info = json.get("data").cloned().unwrap_or(Value::Null);
            self.login_info = Some(info.clone());
            self.persist_login_info(info.clone());
            self.info("登录成功，凭据已加密落盘", "success").await;
            Ok(info)
        } else {
            Err(jstr(&json, "msg").is_empty().then(|| "登录失败，请检查验证码或密码".to_string()).unwrap_or_else(|| jstr(&json, "msg")))
        }
    }

    /// Redeem an access token (QR login or silent renewal) for a full session.
    pub async fn login_by_token(&mut self, access_token: &str) -> Result<Value, String> {
        let device_code = self.login_info.as_ref()
            .map(|i| jstr(i, "deviceCode"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.account.device_code.clone());
        let body = json!({
            "accessToken": access_token,
            "osType": "Windows",
            "deviceModel": "Windows NT 10.0; Win64; x64",
            "appVersion": "3.2.0",
            "deviceCode": device_code,
            "deviceName": "Chrome浏览器",
            "deviceType": DEVICE_TYPE,
            "sysVersion": "Windows NT 10.0; Win64; x64",
            "clientVersion": VERSION
        });
        let resp = self.http
            .post(format!("{}/api/auth/client/tokenLogin", BASE))
            .headers(self.signed_headers(true))
            .json(&body)
            .send().await.map_err(|e| format!("凭据登录失败: {}", e))?;
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        let code = jnum(&json, "code");
        if code == 0 || code == 200 {
            let info = json.get("data").cloned().ok_or("凭据登录未返回会话信息")?;
            self.login_info = Some(info.clone());
            self.persist_login_info(info.clone());
            Ok(info)
        } else {
            Err(format!("{} (Code: {})", jstr(&json, "msg"), code))
        }
    }

    pub async fn gen_login_token(&self, seconds: u64) -> Result<String, String> {
        let mut last_err = String::new();
        for _ in 0..2 {
            let resp = self.http
                .post(format!("{}/api/auth/client/genLoginToken", BASE))
                .headers(self.signed_headers(true))
                .json(&json!({ "authAppModel": 34, "effectiveSeconds": seconds }))
                .send().await;
            match resp {
                Ok(r) => {
                    if let Ok(json) = r.json::<Value>().await {
                        if jnum(&json, "code") == 0 {
                            let token = jstr(json.get("data").unwrap_or(&Value::Null), "token");
                            if !token.is_empty() { return Ok(token); }
                        }
                        last_err = format!("{} (Code: {})", jstr(&json, "msg"), jnum(&json, "code"));
                    }
                }
                Err(e) => last_err = e.to_string(),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(last_err)
    }

    pub async fn renew_session(&mut self) -> Result<Value, String> {
        let token = self.gen_login_token(300).await?;
        self.login_by_token(&token).await
    }

    /// Ensure a valid session, silently renewing when possible.
    pub async fn ensure_login(&mut self) -> Result<Value, String> {
        if self.login_info.is_some() && !self.account.session_expired {
            return Ok(self.login_info.clone().unwrap());
        }
        if self.login_info.is_some() {
            if let Ok(info) = self.renew_session().await {
                self.info("会话已自动无感续期", "success").await;
                return Ok(info);
            }
        }
        self.mark_session_expired("会话已过期，需要重新验证登录");
        Err("会话已过期，请在控制台重新验证登录".into())
    }

    /// Generate an official QR code for app-based login.
    pub async fn gen_qr_code(&self, device_code: &str) -> Result<Value, String> {
        let mut headers = self.base_headers();
        let _ = headers.insert("ctg-devicecode", header_value(device_code));
        let resp = self.http
            .post(format!("{}/api/auth/client/qrCode/genData", BASE))
            .headers(headers)
            .form(&[("deviceCode", device_code)])
            .send().await.map_err(|e| e.to_string())?;
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        if jnum(&json, "code") != 0 {
            return Err(if jstr(&json, "msg").is_empty() { "获取二维码失败".into() } else { jstr(&json, "msg") });
        }
        let data = json.get("data").cloned().unwrap_or(Value::Null);
        let qr_id = jstr(&data, "qrCodeId");
        let endpoint = jstr(&data, "qrCodeEndpoint");
        let qr_url = if !endpoint.is_empty() {
            format!("{}{}qrCodeId={}&loginMode=1", endpoint, if endpoint.contains('?') { '&' } else { '?' }, urlencoding::encode(&qr_id))
        } else {
            format!("https://desk.ctyun.cn/selforder/#/login-confirm?qrCodeId={}&loginMode=1", urlencoding::encode(&qr_id))
        };
        Ok(json!({ "qrCodeId": qr_id, "qrUrl": qr_url }))
    }

    pub async fn qr_code_status(&self, qr_code_id: &str) -> Result<Value, String> {
        let resp = self.http
            .get(format!("{}/api/auth/client/qrCode/getStatus", BASE))
            .headers(self.base_headers())
            .query(&[("qrCodeId", qr_code_id)])
            .send().await.map_err(|e| e.to_string())?;
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        if jnum(&json, "code") != 0 {
            return Err(jstr(&json, "msg"));
        }
        let data = json.get("data").cloned().unwrap_or(Value::Null);
        Ok(json!({
            "codeStatus": jstr(&data, "codeStatus"),
            "loginToken": jstr(&data, "loginToken")
        }))
    }

    pub async fn get_captcha(&self, phone: &str, device_code: &str) -> Result<Value, String> {
        let mut headers = self.base_headers();
        let _ = headers.insert("ctg-devicecode", header_value(device_code));
        let chal: Value = self.http
            .post(format!("{}/api/auth/client/genChallengeData", BASE))
            .headers(headers.clone())
            .json(&json!({}))
            .send().await.map_err(|e| e.to_string())?
            .json().await.map_err(|e| e.to_string())?;
        if jnum(&chal, "code") != 0 {
            return Err(jstr(&chal, "msg"));
        }
        let data = chal.get("data").cloned().unwrap_or(Value::Null);
        let ts = chrono::Utc::now().timestamp_millis().to_string();
        let img = self.http
            .get(format!("{}/api/auth/client/captcha", BASE))
            .headers(headers)
            .query(&[
                ("height", "36"), ("width", "85"),
                ("userInfo", phone), ("mode", "auto"), ("_t", ts.as_str()),
            ])
            .send().await.map_err(|e| e.to_string())?
            .bytes().await.map_err(|e| e.to_string())?;
        Ok(json!({
            "challengeCode": jstr(&data, "challengeCode"),
            "challengeId": jstr(&data, "challengeId"),
            "deviceCode": device_code,
            "captchaImage": format!("data:image/jpeg;base64,{}", base64::encode(img))
        }))
    }

    /// Fetch all desktop variants, merging local per-VM preferences.
    pub async fn fetch_desktops(&mut self) -> Result<Vec<Desktop>, String> {
        self.ensure_login().await?;
        let mut merged: Vec<Desktop> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        let page = self.http
            .post(format!("{}/api/desktop/client/pageDesktop", BASE))
            .headers(self.signed_headers(true))
            .json(&json!({
                "getCnt": 50,
                "desktopTypes": ["1", "2001", "2002", "2003"],
                "sortType": "createTimeV1"
            }))
            .send().await.ok();
        let page = match page { Some(r) => r.json::<Value>().await.ok(), None => None };
        if let Some(page) = page {
            if jnum(&page, "code") == 0 {
                for d in extract_desktops(page.get("data").unwrap_or(&Value::Null)) {
                    let key = if d.desktop_id.is_empty() { d.obj_id.clone() } else { d.desktop_id.clone() };
                    if seen.insert(key) { merged.push(d); }
                }
            }
        }

        let list = self.http
            .get(format!("{}/api/desktop/client/list", BASE))
            .headers(self.signed_headers(false))
            .send().await.ok();
        let list = match list { Some(r) => r.json::<Value>().await.ok(), None => None };
        if let Some(list) = list {
            if jnum(&list, "code") == 0 {
                for d in extract_desktops(list.get("data").unwrap_or(&Value::Null)) {
                    let key = if d.desktop_id.is_empty() { d.obj_id.clone() } else { d.desktop_id.clone() };
                    if seen.insert(key) { merged.push(d); }
                }
            }
        }

        if merged.is_empty() {
            return Err("未获取到云电脑列表，请检查会话状态".into());
        }

        let old_map: std::collections::HashMap<String, Desktop> = self.account.desktops.iter()
            .map(|d| {
                let k = if d.desktop_id.is_empty() { d.obj_id.clone() } else { d.desktop_id.clone() };
                (k, d.clone())
            }).collect();
        for d in &mut merged {
            let key = if d.desktop_id.is_empty() { d.obj_id.clone() } else { d.desktop_id.clone() };
            if let Some(old) = old_map.get(&key) {
                d.keepalive_enabled = old.keepalive_enabled;
                d.task_enabled = old.task_enabled;
                d.auto_boot_enabled = old.auto_boot_enabled;
                d.keepalive_interval = old.keepalive_interval;
                d.last_pulse_at = old.last_pulse_at.clone();
                d.live_metrics = old.live_metrics.clone();
            } else {
                d.keepalive_interval = self.account.keepalive_interval;
            }
        }

        let mut config = self.store.config.write().unwrap();
        if let Some(acc) = config.accounts.iter_mut().find(|a| a.id == self.account.id) {
            acc.desktops = merged.clone();
        }
        drop(config);
        self.store.save();
        self.account.desktops = merged.clone();
        Ok(merged)
    }

    /// Retrieve the connection descriptor required for the WebSocket session.
    /// Mirrors upstream `connect()` exactly: try the official status endpoint
    /// first, then fall back to the `connect` endpoint (needed for 政企桌面池 /
    /// 启动中的虚拟机). Also performs one silent session renewal when the
    /// official API reports an expired credential (40010 / 登录信息已过期).
    pub async fn get_desktop_info(&mut self, desktop_id: &str) -> Result<Value, String> {
        let session_expired = |json: &Value| -> bool {
            jnum(json, "code") == 40010
                || jstr(json, "msg").contains("登录信息已过期")
                || jstr(json, "msg").contains("会话已过期")
        };
        let mut renewed = false;
        let mut last_msg = String::new();

        for _ in 0..2 {
            // Tier 1 — official status endpoint (公众版单机优先).
            if let Ok(resp) = self.http
                .get(format!("{}/api/desktop/client/status", BASE))
                .headers(self.signed_headers(false))
                .query(&[("desktopId", desktop_id), ("specifiedCertCategory", "1")])
                .send().await
            {
                if let Ok(json) = resp.json::<Value>().await {
                    if jnum(&json, "code") == 0 {
                        let data = json.get("data").cloned().unwrap_or(Value::Null);
                        let info = data.get("desktopInfo").cloned().unwrap_or(Value::Null);
                        if !jstr(&info, "clinkLvsOutHost").is_empty() {
                            return Ok(info);
                        }
                    } else {
                        last_msg = jstr(&json, "msg");
                        if session_expired(&json) && !renewed {
                            let _ = self.renew_session().await;
                            renewed = true;
                            continue;
                        }
                    }
                }
            }

            // Tier 2 — official connect endpoint (政企桌面池 / 启动中虚拟机).
            // Payload is byte-for-byte aligned with upstream `connBody`.
            let form = [
                ("objId", desktop_id.to_string()),
                ("objType", "0".to_string()),
                ("osType", "15".to_string()),
                ("deviceId", DEVICE_TYPE.to_string()),
                ("vdCommand", String::new()),
                ("ipAddress", String::new()),
                ("macAddress", String::new()),
                ("deviceCode", self.account.device_code.clone()),
                ("deviceName", "Chrome浏览器".to_string()),
                ("deviceType", DEVICE_TYPE.to_string()),
                ("deviceModel", "Windows NT 10.0; Win64; x64".to_string()),
                ("desktopId", desktop_id.to_string()),
                ("appVersion", "3.2.0".to_string()),
                ("sysVersion", "Windows NT 10.0; Win64; x64".to_string()),
                ("clientVersion", VERSION.to_string()),
                ("specifiedCertCategory", "1".to_string()),
            ];
            if let Ok(resp) = self.http
                .post(format!("{}/api/desktop/client/connect", BASE))
                .headers(self.signed_headers(false))
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .form(&form)
                .send().await
            {
                if let Ok(json) = resp.json::<Value>().await {
                    if jnum(&json, "code") == 0 {
                        let data = json.get("data").cloned().unwrap_or(Value::Null);
                        let info = data.get("desktopInfo").cloned().unwrap_or(Value::Null);
                        if !jstr(&info, "clinkLvsOutHost").is_empty() {
                            return Ok(info);
                        }
                    } else {
                        last_msg = jstr(&json, "msg");
                        if session_expired(&json) && !renewed {
                            let _ = self.renew_session().await;
                            renewed = true;
                            continue;
                        }
                    }
                }
            }
            break;
        }

        Err(if last_msg.is_empty() {
            "视讯网关暂未分配完毕".into()
        } else {
            format!("视讯网关暂未分配完毕 ({})", last_msg)
        })
    }

    /// Power control: 1=on, 18=wake, 2=off, 3=reboot.
    pub async fn control_power(&mut self, desktop_id: &str, action: &str) -> Result<String, String> {
        let op = match action {
            "poweron" => 1,
            "awake" => 18,
            "shutdown" => 2,
            "reboot" => 3,
            _ => return Err("不支持的电源操作".into()),
        };
        self.ensure_login().await?;

        let mut last_err = String::new();
        if op == 1 || op == 18 {
            for attempt in [op, if op == 1 { 18 } else { 1 }] {
                let form = [("desktopId", desktop_id.to_string()), ("operationType", attempt.to_string())];
                let resp = self.http
                    .post(format!("{}/api/desktop/client/operate", BASE))
                    .headers(self.signed_headers(false))
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .form(&form)
                    .send().await;
                let send_result = match resp {
                    Ok(r) => r.json::<Value>().await.map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                };
                match send_result {
                    Ok(json) if jnum(&json, "code") == 0 => {
                        return Ok("电源指令已成功下发".into());
                    }
                    Ok(json) => last_err = jstr(&json, "msg"),
                    Err(e) => last_err = e,
                }
            }
            // Fallback: the official connect endpoint can trigger VM startup.
            let form = [
                ("objId", desktop_id.to_string()), ("objType", "0".to_string()), ("osType", "15".to_string()),
                ("deviceId", DEVICE_TYPE.into()), ("vdCommand", String::new()),
                ("ipAddress", String::new()), ("macAddress", String::new()),
                ("deviceCode", self.account.device_code.clone()), ("deviceName", "Chrome浏览器".into()),
                ("deviceType", DEVICE_TYPE.into()), ("deviceModel", "Windows NT 10.0; Win64; x64".into()),
                ("desktopId", desktop_id.to_string()),
                ("appVersion", "3.2.0".into()), ("sysVersion", "Windows NT 10.0; Win64; x64".into()),
                ("clientVersion", VERSION.into()), ("specifiedCertCategory", "1".into()),
            ];
            if let Ok(resp) = self.http
                .post(format!("{}/api/desktop/client/connect", BASE))
                .headers(self.signed_headers(false))
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .form(&form).send().await
            {
                if let Ok(json) = resp.json::<Value>().await {
                    if jnum(&json, "code") == 0 && jbool(json.get("data").unwrap_or(&Value::Null), "goingRetry") {
                        return Ok("开机信令已触发，云端正在启动".into());
                    }
                }
            }
            if last_err.contains("运行中") || last_err.contains("已经开机") || last_err.contains("正在进行") {
                return Ok("云电脑已处于运行状态".into());
            }
            return Err(if last_err.is_empty() { "官方接口未确认开机".into() } else { last_err });
        }

        let form = [("desktopId", desktop_id.to_string()), ("operationType", op.to_string())];
        let resp = self.http
            .post(format!("{}/api/desktop/client/operate", BASE))
            .headers(self.signed_headers(false))
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .form(&form)
            .send().await;
        match resp {
            Ok(r) => match r.json::<Value>().await {
                Ok(json) if jnum(&json, "code") == 0 => Ok("电源指令已成功下发".into()),
                Ok(json) => Err(jstr(&json, "msg")),
                Err(e) => Err(e.to_string()),
            },
            Err(e) => Err(e.to_string()),
        }
    }

    /// Read official task center progress and update local statistics.
    pub async fn refresh_tasks(&mut self) -> Result<Vec<TaskProgress>, String> {
        self.ensure_login().await?;
        let resp = self.http
            .get(format!("{}/api/marketing/userPoints/getTaskList", SELFORDER))
            .headers(self.signed_headers(false))
            .send().await.map_err(|e| e.to_string())?;
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        // 上游: 40010 / 登录信息已过期 / 会话已过期 → renewToken 后续签
        if jnum(&json, "code") == 40010
            || jstr(&json, "msg").contains("登录信息已过期")
            || jstr(&json, "msg").contains("会话已过期")
        {
            let _ = self.renew_session().await;
            return Err(format!("凭据过期 (code={})", jnum(&json, "code")));
        }
        if jnum(&json, "code") != 0 {
            return Err(jstr(&json, "msg"));
        }
        let data = json.get("data").cloned().unwrap_or(Value::Null);
        let items = data.as_array().cloned().unwrap_or_default();
        let tasks: Vec<TaskProgress> = items.iter().map(|t| TaskProgress {
            name: jstr(t, "taskDefName"),
            current: jnum(t, "currentProgress").max(0) as u64,
            total: jnum(t, "totalProgress").max(1) as u64,
            status: jnum(t, "status") as i32,
            // 上游 t.pointsList?.[0]?.value || 100（JS || 对 0 会回退到 100）
            points: t.get("pointsList")
                .and_then(|p| p.get(0))
                .map(|p| jnum(p, "value") as u64)
                .filter(|v| *v != 0)
                .unwrap_or(100),
        }).collect();

        let today = crate::models::today_beijing();
        let now = crate::models::now_beijing();
        let mut config = self.store.config.write().unwrap();
        if let Some(acc) = config.accounts.iter_mut().find(|a| a.id == self.account.id) {
            for t in &tasks {
                // 上游: 使用1小时任务的 hangMinutesToday 无论完成与否都按进度实时更新
                if t.name.contains("使用1小时") || t.name.contains("使用AI云电脑") {
                    acc.stats.hang_minutes_today = t.current / 60;
                }
                let done = t.status == 2 || (t.total > 0 && t.current >= t.total);
                if !done { continue; }
                if t.name.contains("登录AI云电脑") && !acc.stats.last_sign_time.starts_with(&today) {
                    acc.stats.last_sign_time = now.clone();
                }
                if t.name.contains("AI对话") && !acc.stats.last_ai_chat_time.starts_with(&today) {
                    acc.stats.last_ai_chat_time = now.clone();
                }
                if (t.name.contains("使用AI云电脑") || t.name.contains("使用1小时"))
                    && !acc.stats.last_hang_time.starts_with(&today) {
                    acc.stats.last_hang_time = now.clone();
                }
            }
            for d in &mut acc.desktops { d.live_metrics.official_tasks = tasks.clone(); }
        }
        drop(config);
        self.store.save();
        Ok(tasks)
    }

    /// 上游 refreshOfficialTasks 等价物：一次刷新官方任务进度 + 积分余额。
    /// 返回 (任务是否成功, 积分是否成功)，单项失败不影响另一项。
    pub async fn refresh_official_metrics(&mut self) -> (bool, bool) {
        let tasks_ok = self.refresh_tasks().await.is_ok();
        let points_ok = self.get_points().await.is_ok();
        (tasks_ok, points_ok)
    }

    pub async fn get_points(&mut self) -> Result<u64, String> {
        self.ensure_login().await?;
        let resp = self.http
            .get(format!("{}/api/marketing/userPoints/getUserPoints", SELFORDER))
            .headers(self.signed_headers(false))
            .send().await.map_err(|e| e.to_string())?;
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        // 仅当 code==0 时才信任 data 并覆盖积分；异常响应（如 code=-1 且 data=[]）
        // 若继续解析会得到 0 并落盘，把真实余额清零——必须拒绝。
        if jnum(&json, "code") != 0 {
            return Err(format!("积分接口返回异常 code={}", jnum(&json, "code")));
        }
        // 上游: data 是数组。可能同时含 willOutDate:true 的临期子积分(如100)与真实总积分(如900)。
        // 优先取 !willOutDate && pointType===1 的主账户可用积分；否则取 points 最大项。
        let items = json.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
        let valid = items.iter()
            .find(|p| !jbool(p, "willOutDate") && jnum(p, "pointType") == 1)
            .cloned()
            .or_else(|| items.iter().max_by_key(|p| jnum(p, "points")).cloned());
        let points = valid.map(|p| jnum(&p, "points").max(0) as u64).unwrap_or(0);
        // 与上游一致：积分余额写入 account.stats.points 落盘。
        // 同时记录余额变动历史：仅当余额真正变化（或首次有历史）时才追加一条，
        // 避免每 90 秒一次的刷新产生大量重复点。历史保存在配置里，重启不丢。
        {
            let mut config = self.store.config.write().unwrap();
            if let Some(acc) = config.accounts.iter_mut().find(|a| a.id == self.account.id) {
                let prev = acc.stats.points;
                let hist = &mut acc.stats.points_history;
                let need_record = hist.last().map(|e| e.points) != Some(points);
                if need_record {
                    let delta = points as i64 - prev as i64;
                    // 首次记录（无历史）用"初始余额"，不虚构成某次增减；
                    // 之后按正负标注增加/消耗。
                    let reason = if hist.is_empty() {
                        "初始余额".to_string()
                    } else if delta > 0 {
                        "积分增加".to_string()
                    } else {
                        "积分消耗".to_string()
                    };
                    hist.push(crate::models::PointsEntry {
                        ts: crate::models::now_beijing(),
                        points,
                        delta,
                        reason,
                    });
                    // 滚动保留最近 200 条，防止配置无限膨胀。
                    if hist.len() > 200 {
                        let excess = hist.len() - 200;
                        hist.drain(..excess);
                    }
                }
                acc.stats.points = points;
            }
        }
        self.store.save();
        Ok(points)
    }

    /// 上游兜底商品：官方商城接口取不到数据时的静态回退（字段与上游完全一致）。
    pub fn fallback_rewards() -> Vec<Value> {
        vec![
            json!({"prodId":17023101,"prodName":"8C16G升配包1天","costPoints":500,"prodType":"pointstplupgrade","description":"可将AI云电脑升配至8C16G，最多支持兑换365天；月末兑换可维持长期8C16G"}),
            json!({"prodId":17023111,"prodName":"16C32G升配包1天","costPoints":1000,"prodType":"pointstplupgrade","description":"可将AI云电脑（政企版）升配至16C32G，最多支持兑换365天"}),
            json!({"prodId":17020101,"prodName":"AI应用中心高级版 (1个月)","costPoints":1000,"prodType":"cpcai","description":"权益：支持DeepSeek满血版、专属智库等，有效期1个月"}),
            json!({"prodId":17010101,"prodName":"专属智库1G存储空间","costPoints":1000,"prodType":"cpcai","description":"基于当前AI应用中心存储空间，叠加1G存储空间，每月限兑5次"}),
            json!({"prodId":17024101,"prodName":"1G数据盘永久扩容","costPoints":1200,"prodType":"pointsdiskupgrade","description":"兑换后，将自动创建1个新数据盘，最大不超过500GB"}),
        ]
    }

    /// 商品是否需要绑定目标云电脑（与上游 rewardNeedsDesktop 白名单一致）。
    pub fn reward_needs_desktop(prod_id: u64, prod_type: &str) -> bool {
        const NEED: [u64; 5] = [17023101, 17023111, 17024101, 17026101, 17026111];
        let pt = prod_type.trim().to_lowercase();
        NEED.contains(&prod_id) || pt == "pointstplupgrade" || pt == "pointsdiskupgrade"
    }

    pub async fn get_rewards(&mut self) -> Result<Vec<Value>, String> {
        self.ensure_login().await?;
        let resp = self.http
            .get(format!("{}/api/selforder/prod/get", SELFORDER))
            .headers(self.signed_headers(false))
            .query(&[("prodId", "17000000"), ("prodCode", "POINTS")])
            .send().await.map_err(|e| e.to_string())?;
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        // 上游: data[] -> series[] -> sku[]，字段 costPoints。
        let malls = json.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut rewards = Vec::new();
        for mall in &malls {
            let series_list = mall.get("series").and_then(Value::as_array).cloned().unwrap_or_default();
            for series in &series_list {
                let skus = series.get("sku").and_then(Value::as_array).cloned().unwrap_or_default();
                for sku in &skus {
                    let mut desc = if jstr(sku, "description").is_empty() {
                        jstr(series, "description")
                    } else {
                        jstr(sku, "description")
                    };
                    // 去 HTML 标签（上游 replace(/<[^>]+>/g, ' ')）
                    desc = strip_html_tags(&desc);
                    rewards.push(json!({
                        "prodId": jnum(sku, "prodId"),
                        "prodName": jstr(sku, "prodName"),
                        "costPoints": jnum(sku, "costPoints").max(0),
                        "prodType": jstr(sku, "prodType"),
                        "description": desc
                    }));
                }
            }
        }
        if rewards.is_empty() {
            rewards = Self::fallback_rewards();
        }
        Ok(rewards)
    }

    /// Redeem rewards with the official ordering endpoint.
    pub async fn place_order(
        &mut self,
        prod_id: u64, prod_name: &str, prod_type: &str,
        cost_points: u64, times: u64, desktop_id: &str, point_type: i32,
    ) -> Result<String, String> {
        self.ensure_login().await?;
        // 上游: 升配/数据盘类商品需绑定桌面(attrVal 为数字)；其余商品 attr 仅 attrKey, 无 attrVal。
        let needs_desktop = Self::reward_needs_desktop(prod_id, prod_type);
        let attr = if needs_desktop && !desktop_id.is_empty() {
            let num = desktop_id.trim().parse::<u64>().unwrap_or(0);
            json!({ "attrKey": "bindDesktopId", "attrVal": num })
        } else {
            json!({ "attrKey": "mobilephone" })
        };
        let body = json!({
            "busiChannel": "010",
            "orderType": 1,
            "pointType": point_type,
            "points": cost_points * times,
            "sku": [{
                "execSort": 1,
                "prodId": prod_id,
                "prodType": prod_type,
                "attrs": [attr]
            }]
        });
        let mut last_err = String::new();
        for _ in 0..3 {
            let resp = self.http
                .post(format!("{}/api/selforder/paas/placeOrder", SELFORDER))
                .headers(self.signed_headers(true))
                .json(&body)
                .send().await;
            match resp {
                Ok(r) => {
                    if let Ok(json) = r.json::<Value>().await {
                        if jnum(&json, "code") == 0 {
                            return Ok(format!("兑换成功：{}", prod_name));
                        }
                        last_err = if jstr(&json, "msg").is_empty() { jstr(&json, "reason") } else { jstr(&json, "msg") };
                        // 上游立即终止关键词：积分不足/点数不足/余额不足/风控/频繁/异常操作/超过最大/上限
                        let fatal = ["积分不足", "点数不足", "余额不足", "风控", "频繁", "异常操作", "超过最大", "上限"];
                        if fatal.iter().any(|k| last_err.contains(k)) {
                            break;
                        }
                    }
                }
                Err(e) => last_err = e.to_string(),
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        Err(last_err)
    }

    /// Build the password-free web desktop launch descriptor.
    pub async fn web_launch(&mut self, desktop_id: &str) -> Result<Value, String> {
        self.ensure_login().await?;
        let info = self.get_desktop_info(desktop_id).await?;
        let obj_id = jstr(&info, "objId").is_empty().then(|| desktop_id.to_string()).unwrap_or_else(|| jstr(&info, "objId"));
        let b64 = base64::encode(obj_id.as_bytes());
        let password = self.store.decrypt_password(&self.account.password);
        let token = self.gen_login_token(300).await.unwrap_or_default();
        Ok(json!({
            "success": true,
            "user": self.account.user,
            "password": password,
            "deviceCode": self.account.device_code,
            "authData": self.login_info,
            "authExpiredAt": chrono::Utc::now().timestamp() + 72 * 3600,
            "targetUrl": format!("https://pc.ctyun.cn/#/desktop?id={}", urlencoding::encode(&b64)),
            "directViewUrl": format!("/desktop-view?id={}&desktopId={}", urlencoding::encode(&b64), urlencoding::encode(desktop_id)),
            "desktopUrl": "https://pc.ctyun.cn/#/desktop-list",
            "desktopName": jstr(&info, "desktopName"),
            "displayConfig": self.account.display_config,
            "loginToken": token
        }))
    }
}

/// Extract desktop objects from the several shapes returned by the official APIs.
pub fn extract_desktops(data: &Value) -> Vec<Desktop> {
    let mut list = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (key, obj_type, is_pool, default_flavor) in [
        ("desktopList", 0i32, false, "公众版"),
        ("desktopPoolList", 1, true, "政企版"),
        ("preemptionDesktopList", 2, false, "抢占式"),
    ] {
        if let Some(items) = data.get(key).and_then(Value::as_array) {
            for item in items {
                let id = {
                    let raw = if jstr(item, "desktopId").is_empty() { jstr(item, "objId") } else { jstr(item, "desktopId") };
                    if raw.is_empty() { jstr(item, "poolId") } else { raw }
                };
                if id.is_empty() || !seen.insert(id.clone()) { continue; }
                let spec = extract_specs(item, default_flavor);
                let name = if jstr(item, "desktopName").is_empty() {
                    if jstr(item, "poolName").is_empty() { "天翼云电脑".into() } else { jstr(item, "poolName") }
                } else { jstr(item, "desktopName") };
                let status = jstr(item, "useStatusText");
                list.push(Desktop {
                    desktop_id: id.clone(),
                    obj_id: if jstr(item, "objId").is_empty() { id.clone() } else { jstr(item, "objId") },
                    desktop_name: name,
                    desktop_code: if jstr(item, "desktopCode").is_empty() { id.clone() } else { jstr(item, "desktopCode") },
                    use_status_text: if status.is_empty() { "运行中".into() } else { status },
                    use_status: jnum(item, "useStatus") as i32,
                    image_name: jstr(item, "imageName"),
                    flavor_name: spec.2,
                    spec_str: format!("{}/{}", spec.0, spec.1),
                    cpu: spec.0,
                    memory: spec.1,
                    os: "Windows".into(),
                    obj_type,
                    is_pool,
                    keepalive_enabled: true,
                    task_enabled: true,
                    auto_boot_enabled: true,
                    keepalive_interval: 0,
                    last_pulse_at: String::new(),
                    live_metrics: LiveMetrics::default(),
                });
            }
        }
    }
    list
}

fn extract_specs(item: &Value, default_flavor: &str) -> (String, String, String) {
    let flavor = if jstr(item, "flavorName").is_empty() { jstr(item, "prodGroupName") } else { jstr(item, "flavorName") };
    let name = if jstr(item, "desktopName").is_empty() { jstr(item, "poolName") } else { jstr(item, "desktopName") };
    let combined = format!("{} {}", flavor, name).to_uppercase();
    let explicit = combined.split_whitespace().find_map(|token| {
        let bytes = token.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i].is_ascii_digit() && bytes[i + 1] == b'C' {
                let mut j = i + 2;
                while j < bytes.len() && bytes[j].is_ascii_digit() { j += 1; }
                if j < bytes.len() && bytes[j] == b'G' && j > i + 2 {
                    return Some((format!("{}核", &token[i..i + 1]), format!("{}G", &token[i + 2..j])));
                }
            }
            i += 1;
        }
        None
    });
    if let Some((cpu, memory)) = explicit {
        return (cpu, memory, if flavor.is_empty() { default_flavor.into() } else { flavor });
    }
    let spec = if combined.contains("旗舰版") { (16u32, 32u32) }
        else if combined.contains("标准版") { (4, 8) }
        else { (8, 16) };
    (format!("{}核", spec.0), format!("{}G", spec.1), if flavor.is_empty() { default_flavor.into() } else { flavor })
}
