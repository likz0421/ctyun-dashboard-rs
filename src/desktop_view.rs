//! 免密远程桌面：反向代理天翼云官方 Web 控制台并注入认证凭据。
//! 对齐上游 muyicn/ctyun-dashboard 的 desktop-view 方案：
//!   1. 拉取官方 pc.ctyun.cn 首页 HTML（同源加载，才能写它的 localStorage）
//!   2. 注入完整 authDataObj + web_device_code + 路由锁定
//!   3. 劫持 fetch/XHR，把天翼云 API 请求改写回本地代理（规避跨域/防盗链）
//!   4. 劫持 Worker/WASM 解码器到代理路径
//!   5. 静态资源路径改写为 /ctyun-static/*

use crate::ctyun::{jstr, CtYunClient};
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static INDEX_CACHE: OnceLock<std::sync::RwLock<Option<(String, Instant)>>> = OnceLock::new();

const FALLBACK_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>天翼云电脑</title></head><body><div id=\"app\"></div></body></html>";

/// 拉取官方首页 HTML，缓存 1 小时。
async fn fetch_official_index(client: &reqwest::Client) -> String {
    let cache = INDEX_CACHE.get_or_init(|| std::sync::RwLock::new(None));
    if let Ok(guard) = cache.read() {
        if let Some((html, at)) = guard.as_ref() {
            if at.elapsed() < Duration::from_secs(3600) {
                return html.clone();
            }
        }
    }
    let fetched: Option<String> = match client
        .get("https://pc.ctyun.cn/")
        .header("User-Agent", crate::ctyun::UA)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            match resp.text().await {
                Ok(t) if t.contains("app") || t.len() > 500 => Some(t),
                _ => None,
            }
        }
        _ => None,
    };

    if let Some(html) = fetched {
        if let Ok(mut guard) = cache.write() {
            *guard = Some((html.clone(), Instant::now()));
        }
        return html;
    }
    // 网络失败时退回上次缓存（即使过期）
    if let Ok(guard) = cache.read() {
        if let Some((html, _)) = guard.as_ref() {
            return html.clone();
        }
    }
    FALLBACK_HTML.to_string()
}

/// 构造注入脚本。使用占位符 + replace，避免 JS 大括号与 Rust format! 冲突。
fn build_inject_script(auth_json: &str, device_code: &str, account_id: &str, session_token: &str, b64_id: &str) -> String {
    let script = r#"<script>
(function() {
  // 天翼云官方 Web 控制台会校验 navigator.userAgent 的 Chrome 主版本号，
  // 低版本浏览器（旧版 Safari/Edge/内置 WebView）会被拦在“浏览器版本过低”页。
  // 官方页面只用它做兼容性判断，真实渲染能力由浏览器自身决定，这里伪装为受支持的
  // Chrome 版本即可正常进入远程桌面；同时兼容新版 userAgentData/高熵 API。
  var GOOD_UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36';
  var REAL_UA = navigator.userAgent || '';
  try {
    Object.defineProperty(navigator, 'userAgent', { get: function() { return GOOD_UA; }, configurable: true });
    Object.defineProperty(navigator, 'appVersion', { get: function() { return '5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36'; }, configurable: true });
    if (navigator.userAgentData) {
      if (navigator.userAgentData.brands) {
        Object.defineProperty(navigator.userAgentData, 'brands', { get: function() {
          return [{brand:'Chromium',version:'137'},{brand:'Microsoft Edge',version:'137'},{brand:'Not A(Brand',version:'99'}];
        }, configurable: true });
      }
      if (typeof navigator.userAgentData.getHighEntropyValues === 'function') {
        var origHE = navigator.userAgentData.getHighEntropyValues.bind(navigator.userAgentData);
        navigator.userAgentData.getHighEntropyValues = function(hints) {
          return origHE(hints).then(function(v) {
            var out = Object.assign({}, v);
            if (hints.indexOf('majorVersion') >= 0) out.majorVersion = '137';
            if (hints.indexOf('fullVersionList') >= 0) out.fullVersionList = [{brand:'Chromium',version:'137.0.0.0'},{brand:'Microsoft Edge',version:'137.0.0.0'}];
            if (hints.indexOf('platformVersion') >= 0) out.platformVersion = '10.0.0';
            return out;
          });
        };
      }
    }
  } catch (e) { console.warn('UA override failed', e); }

  var authData = __AUTH_DATA__;
  var deviceCode = __DEVICE_CODE__;
  var expiredAt = String(Date.now() + 72 * 3600 * 1000);
  try {
    localStorage.setItem('web_device_code', deviceCode);
    localStorage.setItem('authExpiredAt', expiredAt);
    localStorage.setItem('authData', JSON.stringify(authData));
    localStorage.setItem('judgeUserEId', authData.userEid || '');
    localStorage.setItem('loginAt', Date.now().toString());
    sessionStorage.setItem('authExpiredAt', expiredAt);
    sessionStorage.setItem('authData', JSON.stringify(authData));
    sessionStorage.setItem('user_name', authData.userName || '');
    sessionStorage.setItem('userId', String(authData.userId || ''));
  } catch (e) { console.error('auth inject failed', e); }

  var proxyBase = '/api/ctyun-proxy?target=';
  function rewriteUrl(u) {
    if (!u || typeof u !== 'string') return u;
    if (u.indexOf(proxyBase) === 0) return u;
    if (u.indexOf('.ctyun.cn:8810') >= 0 || u.indexOf('.ctyun.cn:8816') >= 0 || u.indexOf('-deskmgr.ctyun.cn') >= 0) {
      return proxyBase + encodeURIComponent(u);
    }
    return u;
  }
  var origFetch = window.fetch;
  window.fetch = function(resource, init) {
    if (typeof resource === 'string') resource = rewriteUrl(resource);
    else if (resource && resource.url) resource = new Request(rewriteUrl(resource.url), resource);
    return origFetch.call(this, resource, init);
  };
  var origOpen = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function(method, url) {
    var args = Array.prototype.slice.call(arguments);
    args[1] = rewriteUrl(url);
    return origOpen.apply(this, args);
  };

  var targetHash = '#/desktop?id=' + __B64_ID__;
  if (!window.location.hash || window.location.hash.indexOf('/login') >= 0 || window.location.hash.indexOf('/desktop-list') >= 0) {
    window.location.hash = targetHash;
  }

  var OrigWorker = window.Worker;
  window.Worker = function(scriptUrl, options) {
    if (typeof scriptUrl === 'string' && scriptUrl.indexOf('bbenc.worker.js') >= 0) {
      scriptUrl = '/workers/bbenc.worker.js';
    }
    return new OrigWorker(scriptUrl, options);
  };

  var isMobile = /Android|webOS|iPhone|iPad|iPod|BlackBerry|IEMobile|Opera Mini/i.test(REAL_UA) || window.innerWidth <= 768;
  if (isMobile) {
    var s = document.createElement('style');
    s.innerHTML = 'html,body{width:100vw!important;height:100vh!important;margin:0!important;padding:0!important;overflow:hidden!important;background:#000!important;touch-action:none!important;-webkit-user-select:none!important;user-select:none!important}' +
      '#app,.desktop-container,canvas,video{width:100vw!important;height:100vh!important;max-width:100vw!important;max-height:100vh!important;object-fit:contain!important}';
    document.head.appendChild(s);
    if (screen.orientation && screen.orientation.lock) screen.orientation.lock('landscape').catch(function(){});
  }

  var accId = __ACC_ID__;
  var token = __SESSION_TOKEN__;
  var remoteKey = 'ctyun_remote_' + accId;
  function beat() {
    // 刷新同源 localStorage 活跃标记，供面板首页判断该账号是否已有远程标签页在跑。
    try { localStorage.setItem(remoteKey, JSON.stringify({ ts: Date.now() })); } catch (e) {}
    fetch('/api/accounts/' + accId + '/web-active?token=' + encodeURIComponent(token), {method:'POST'}).catch(function(){});
  }
  beat();
  var hb = setInterval(beat, 5000);
  function leave() {
    clearInterval(hb);
    try { localStorage.removeItem(remoteKey); } catch (e) {}
    if (navigator.sendBeacon) navigator.sendBeacon('/api/accounts/' + accId + '/web-close?token=' + encodeURIComponent(token));
    else fetch('/api/accounts/' + accId + '/web-close?token=' + encodeURIComponent(token), {method:'POST',keepalive:true}).catch(function(){});
  }
  window.addEventListener('beforeunload', leave);
  // 标签页被切到后台时 beforeunload 不一定触发，pagehide 更可靠（bfcache/关闭均会触发）。
  window.addEventListener('pagehide', leave);
})();
</script>"#;
    script
        .replace("__AUTH_DATA__", auth_json)
        .replace("__DEVICE_CODE__", &serde_json::to_string(device_code).unwrap_or_default())
        .replace("__ACC_ID__", &serde_json::to_string(account_id).unwrap_or_default())
        .replace("__SESSION_TOKEN__", &serde_json::to_string(session_token).unwrap_or_default())
        .replace("__B64_ID__", &serde_json::to_string(b64_id).unwrap_or_default())
}

/// 生成完整的免密远程桌面 HTML（官方页面 + 注入脚本）。
pub async fn desktop_view_html(
    client: &mut CtYunClient,
    desktop_id: &str,
    account_id: &str,
    session_token: &str,
) -> Result<String, String> {
    client.ensure_login().await?;
    let launch = client.web_launch(desktop_id).await?;

    let login_info = client.login_info.clone().unwrap_or(Value::Null);
    let obj_id = {
        let v = jstr(&launch, "objId");
        if v.is_empty() { desktop_id.to_string() } else { v }
    };
    let b64_id = base64::encode(obj_id.as_bytes());

    // 构造完整 authDataObj，对齐上游：先展开 loginInfo 全部字段，再覆盖补充字段。
    // 官方前端会读取 loginInfo 里的诸多私有字段（如 userEid/authority 等），
    // 只挑几个字段会导致登录态校验失败、桌面列表为空。
    let mobilephone = {
        let m = jstr(&login_info, "mobilephone");
        if m.is_empty() { client.account.user.clone() } else { m }
    };
    let mut auth = login_info.clone();
    if !auth.is_object() {
        auth = json!({});
    }
    {
        let obj = auth.as_object_mut().unwrap();
        obj.insert("logined".into(), json!(true));
        obj.insert("userId".into(), json!(jstr(&login_info, "userId")));
        obj.insert("userName".into(), json!(jstr(&login_info, "userName")));
        obj.insert("userEid".into(), json!(jstr(&login_info, "userEid")));
        obj.insert("userAccount".into(), json!(jstr(&login_info, "userAccount")));
        obj.insert("mobilephone".into(), json!(mobilephone));
        obj.insert("tenantId".into(), json!(jstr(&login_info, "tenantId")));
        obj.insert("secretKey".into(), json!(jstr(&login_info, "secretKey")));
        obj.insert("deviceType".into(), json!(crate::ctyun::DEVICE_TYPE));
        obj.insert("bondedDevice".into(), json!(true));
        obj.insert("commonLoginReqHeader".into(), json!(jstr(&login_info, "commonLoginReqHeader")));
        obj.insert("timestamp".into(), json!(chrono::Utc::now().timestamp_millis()));
    }
    let auth_json = serde_json::to_string(&auth).map_err(|e| e.to_string())?;

    let inject = build_inject_script(
        &auth_json,
        &client.account.device_code,
        account_id,
        session_token,
        &b64_id,
    );

    let mut html = fetch_official_index(&client.http).await;
    // 注入到 <head> 开头，确保脚本在官方应用启动前执行
    let viewport = "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no, viewport-fit=cover\">";
    html = html.replacen("<head>", &format!("<head>{}{}", viewport, inject), 1);

    // 静态资源改走本地代理（同源，避免跨域与防盗链）
    html = html.replace("src=\"static/", "src=\"/ctyun-static/static/");
    html = html.replace("src=\"./static/", "src=\"/ctyun-static/static/");
    html = html.replace("href=\"./static/", "href=\"/ctyun-static/static/");
    html = html.replace("href=\"./manifest.json\"", "href=\"/ctyun-static/manifest.json\"");

    Ok(html)
}
