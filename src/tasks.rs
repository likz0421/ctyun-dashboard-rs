use crate::crypto::{aes_ecb_decrypt, md5_hex, sha256_hex};
use crate::ctyun::{jstr, CtYunClient, BASE, UA};
use base64::Engine as _;
use rand::Rng;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use serde_json::{json, Value};

const PRESET_MESSAGES: &[&str] = &[
    "你好，请简单介绍一下你自己",
    "今天有什么值得关注的新闻吗",
    "请给我讲一个简短的笑话",
    "帮我写一句励志的座右铭",
    "天翼云电脑有哪些实用功能",
];

fn strip_pkcs7(data: &[u8]) -> Vec<u8> {
    if data.is_empty() { return data.to_vec(); }
    let pad = *data.last().unwrap() as usize;
    if pad == 0 || pad > 16 || pad > data.len() { return data.to_vec(); }
    if data[data.len() - pad..].iter().all(|b| *b as usize == pad) {
        data[..data.len() - pad].to_vec()
    } else {
        data.to_vec()
    }
}

fn random_string(len: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::rngs::OsRng;
    (0..len).map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char).collect()
}

/// Full native AI-chat task: gateway config -> CAS ticket -> SSO -> chat completion.
pub async fn run_ai_chat(client: &mut CtYunClient) -> Result<String, String> {
    client.ensure_login().await?;

    // 1. Fetch gateway configuration and decrypt the SSO public key.
    let sys: Value = client.http
        .get("https://gwyilian.ctyun.cn/server/eaiSysInfo")
        .header("User-Agent", UA)
        .send().await.map_err(|e| format!("获取网关配置失败: {}", e))?
        .json().await.map_err(|e| e.to_string())?;
    let raw = jstr(&sys, "data");
    let raw_bytes = decode_base64_loose(&raw)
        .ok_or_else(|| "网关配置 base64 解码失败".to_string())?;
    let decrypted = aes_ecb_decrypt(b"chinatelecom@cnn", &raw_bytes)?;
    let gateway: Value = serde_json::from_slice(&strip_pkcs7(&decrypted))
        .map_err(|e| format!("网关配置解析失败: {}", e))?;
    let sso = gateway.get("sso").unwrap_or(&Value::Null);
    let ssopk = jstr(sso, "ssopk");
    let ssopkid = jstr(sso, "ssopkid");
    if ssopk.is_empty() {
        return Err("未能从网关提取到有效的 SSO RSA 公钥".into());
    }

    // 2. Obtain a CAS service ticket, renewing credentials once on 40010.
    let service = "https://eaichat.ctyun.cn:443/chat/#/aichat";
    let mut ticket = String::new();
    for force_renew in [false, true] {
        if force_renew {
            let _ = client.renew_session().await;
        }
        let resp = client.http
            .get(format!("{}/api/auth/client/getTicket", BASE))
            .headers(client.signed_headers(false))
            .query(&[("service", service)])
            .send().await.map_err(|e| e.to_string())?;
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        ticket = jstr(json.get("data").unwrap_or(&Value::Null), "ticket");
        if !ticket.is_empty() { break; }
    }
    if ticket.is_empty() {
        return Err("获取 CAS Ticket 失败".into());
    }

    // 3. RSA-PKCS1 v1.5 encrypt a random client key and exchange it for a session key.
    let client_key = random_string(16);
    // 网关下发的是单行 base64，rsa 0.9 的 pem-rfc7468 要求每行≤64字符，必须折行。
    let pem = format_public_key_pem(&ssopk);
    let public_key = RsaPublicKey::from_public_key_pem(&pem)
        .map_err(|e| format!("SSO 公钥解析失败: {}", e))?;
    let encrypted_key = public_key
        .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, client_key.as_bytes())
        .map_err(|e| format!("客户端密钥加密失败: {}", e))?;

    let form = [
        ("loginType", "iamTicket"),
        ("clientId", "eaiapp"),
        ("iamTicket", ticket.as_str()),
        ("redirectUri", service),
        ("clientKey", &hex::encode(&encrypted_key)),
        ("clientKeyId", &ssopkid),
    ];
    let auth_resp = client.http
        .post("https://eaichat.ctyun.cn/sso/login/v2/iam/ticketAuthorize")
        .header("User-Agent", UA)
        .header("Referer", "https://eaichat.ctyun.cn/chat/")
        .form(&form)
        .send().await.map_err(|e| e.to_string())?;

    let mut yl_token = String::new();
    let mut yl_ssid = String::new();
    for cookie in auth_resp.headers().get_all("set-cookie").iter() {
        if let Ok(text) = cookie.to_str() {
            for part in text.split(';') {
                let part = part.trim();
                if let Some(v) = part.strip_prefix("YL-Token=") { yl_token = v.to_string(); }
                if let Some(v) = part.strip_prefix("YL-Ssid=") { yl_ssid = v.to_string(); }
            }
        }
    }
    if let Some(v) = auth_resp.headers().get("yl-authorization") {
        if yl_token.is_empty() { yl_token = v.to_str().unwrap_or("").to_string(); }
    }

    let auth_json: Value = auth_resp.json().await.map_err(|e| e.to_string())?;
    if jnum(&auth_json, "resultCode") != 0 {
        return Err(format!("SSO 鉴权失败: {}", jstr(&auth_json, "resultMsg")));
    }
    let session_key_enc = jstr(auth_json.get("data").unwrap_or(&Value::Null), "sessionKey");
    // 上游 Node 的 Buffer.from(x,'base64') 会忽略空格/换行并兼容 URL-safe 字符，
    // Rust 的 base64::decode 严格模式遇到空格报 "Invalid symbol 32"。这里做宽松解码。
    let sk_bytes = decode_base64_loose(&session_key_enc)
        .ok_or_else(|| "sessionKey base64 解码失败".to_string())?;
    let sk_raw = aes_ecb_decrypt(client_key.as_bytes(), &sk_bytes)?;
    let sk = String::from_utf8(strip_pkcs7(&sk_raw)).map_err(|e| e.to_string())?;

    // 4. Send the chat completion request with the signed headers.
    let prompt = PRESET_MESSAGES[rand::thread_rng().gen_range(0..PRESET_MESSAGES.len())];
    let body = json!({
        "key_model": "telechat",
        "messages": [{ "role": "user", "content": prompt }],
        "stream": false,
        "client_retry": true,
        "web_search": false
    });
    let body_str = body.to_string();
    let ts = chrono::Utc::now().timestamp_millis().to_string();
    let rnd = random_string(8);
    let signature = sha256_hex(format!("{}&{}&{}&{}", md5_hex(&body_str), sk, ts, rnd));
    let trace_id = uuid::Uuid::new_v4().to_string();

    let resp = client.http
        .post("https://eaichat.ctyun.cn/ai/portal/v3/openai/chat/completions")
        .header("Content-Type", "application/json")
        .header("Cookie", format!("YL-Token={}; YL-Ssid={}", yl_token, yl_ssid))
        .header("YL-Authorization", &yl_token)
        .header("User-Agent", UA)
        .header("Referer", "https://eaichat.ctyun.cn/chat/")
        .header("Origin", "https://eaichat.ctyun.cn")
        .header("x-client-trace-id", trace_id)
        .header("x-eai-source", "web-eai")
        .header("x-eai-version", "202060305")
        .header("YL-Main-Version", "202060305")
        .header("YL-Product-Id", "5")
        .header("Web-Signature", signature)
        .header("Web-Random", &rnd)
        .header("Web-Timestamp", &ts)
        .body(body_str)
        .send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("AI 对话接口异常 (HTTP {})", resp.status()));
    }
    let text = resp.text().await.map_err(|e| e.to_string())?;
    Ok(format!("AI 对话已完成 ({} 字节响应)", text.len()))
}

fn jnum(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// 宽松 base64 解码：先剔除所有空白字符，标准字母表解码失败后再尝试 URL-safe
/// 字母表。对齐上游 Node Buffer.from(x,'base64') 对空白/URL-safe 的宽容行为。
fn decode_base64_loose(input: &str) -> Option<Vec<u8>> {
    let compact: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() { return Some(Vec::new()); }
    if let Ok(v) = base64::engine::general_purpose::STANDARD.decode(&compact) {
        return Some(v);
    }
    base64::engine::general_purpose::URL_SAFE.decode(&compact).ok()
}

/// 把网关下发的单行 base64 DER 公钥按 64 字符折行，构造标准 PEM。
/// 上游 Node 用 OpenSSL 宽松解析单行 base64 无碍，但 Rust 的 rsa 0.9 底层
/// pem-rfc7468 严格要求每行不超过 64 字符，否则报 "PEM Base64 error:
/// invalid Base64 encoding"。真实 ssopk 为 216 字符单行，必须折行。
fn format_public_key_pem(ssopk: &str) -> String {
    let compact: String = ssopk.chars().filter(|c| !c.is_whitespace()).collect();
    let mut body = String::with_capacity(compact.len() + compact.len() / 64);
    for (i, ch) in compact.chars().enumerate() {
        if i > 0 && i % 64 == 0 {
            body.push('\n');
        }
        body.push(ch);
    }
    format!("-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----", body)
}

#[cfg(test)]
mod pem_tests {
    use super::*;
    #[test]
    fn test_real_gateway_pem_folded() {
        // 真实网关下发的 ssopk（216 字符单行 base64），必须折行后才能被 rsa crate 解析。
        let ssopk = "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQCHaZ5BNp518C/OEOup76d/HXbg6nUGJWiLzMoy4Qg87izLG1dRJ6PIPawt3vGvLuVMG0x8cwPfUfUrfUO1w6e9luGbJLohWwlqBdB0znnmBi5FHWqnNmvigLDVNfOKjQ0NsHhf+6D/aF2RVx+axWPE6auX2iPsYgN8OCOhhfXZwwIDAQAB";
        let pem = format_public_key_pem(ssopk);
        rsa::RsaPublicKey::from_public_key_pem(&pem).expect("折行后公钥必须可解析");
        // 单行未折行时应失败，回归保护
        let bad = format!("-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----", ssopk);
        assert!(rsa::RsaPublicKey::from_public_key_pem(&bad).is_err());
    }
}
