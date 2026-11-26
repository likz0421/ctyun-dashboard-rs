use crate::crypto::redq_response;
use crate::ctyun::{jstr, CtYunClient};
use crate::store::LogHub;
use futures_util::{SinkExt, StreamExt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug)]
struct NoCertVerify;

impl ServerCertVerifier for NoCertVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SessionMode {
    /// Observer pulse: never claims the desktop, safe alongside the official client.
    Pulse,
    /// Hang mode: sends 112/104 to accumulate the official one-hour task.
    Hang,
}

#[derive(Debug, Default)]
pub struct SessionResult {
    pub success: bool,
    pub reason: String,
    pub goal_achieved: bool,
}

fn frame(msg_type: u16, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(6 + payload.len());
    buf.extend_from_slice(&msg_type.to_le_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

fn frame118(login_info: &Value) -> Vec<u8> {
    let user_name = jstr(login_info, "userName");
    let user_id = jstr(login_info, "userId");
    let payload = json!({
        "type": 1,
        "userName": user_name,
        "userInfo": "",
        "userId": user_id
    }).to_string();
    let mut buf = Vec::with_capacity(14 + payload.len());
    buf.extend_from_slice(&118u16.to_le_bytes());
    buf.extend_from_slice(&((8 + payload.len()) as i32).to_le_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(&8u32.to_le_bytes());
    buf.extend_from_slice(payload.as_bytes());
    buf
}

fn frame112(desktop_id: &str, desktop_info: &Value, client: &CtYunClient) -> Vec<u8> {
    let strings = [
        jstr(desktop_info, "token"),
        crate::ctyun::DEVICE_TYPE.to_string(),
        client.account.device_code.clone(),
        jstr(client.login_info.as_ref().unwrap_or(&Value::Null), "userName"),
    ];
    let lens: Vec<usize> = strings.iter().map(|s| s.as_bytes().len() + 1).collect();
    let data_size = 36 + lens.iter().sum::<usize>();
    let mut data = vec![0u8; data_size];
    let numeric_id = desktop_id.parse::<u64>().unwrap_or(0) as u32;
    data[0..4].copy_from_slice(&numeric_id.to_le_bytes());

    let mut offset = 4usize;
    let mut str_offset = 36usize;
    for len in &lens {
        data[offset..offset + 4].copy_from_slice(&(*len as u32).to_le_bytes());
        offset += 4;
        data[offset..offset + 4].copy_from_slice(&(str_offset as u32).to_le_bytes());
        offset += 4;
        str_offset += *len;
    }
    let mut cursor = 36usize;
    for s in &strings {
        data[cursor..cursor + s.len()].copy_from_slice(s.as_bytes());
        cursor += s.len() + 1;
    }
    frame(112, &data)
}

/// Run one WebSocket keepalive session against a desktop.
pub async fn run_keepalive(
    mut client: CtYunClient,
    desktop_id: &str,
    desktop_name: &str,
    mode: SessionMode,
    duration_secs: u64,
) -> Result<SessionResult, String> {
    client.ensure_login().await?;
    // 等待视讯网关分配（对齐上游 runDesktopKeepAliveSession 的重试窗口）：
    // 脉冲模式 4 次 × 4 秒（~16s），挂机模式 12 次 × 10 秒（~2min）。
    // 刚开机/网关尚未就绪时，一次失败就放弃会导致保活一直起不来。
    let (attempts, wait_secs) = match mode {
        SessionMode::Hang => (12u32, 10u64),
        SessionMode::Pulse => (4, 4),
    };
    let mut info: Option<serde_json::Value> = None;
    let mut last_err = String::new();
    for i in 0..attempts {
        match client.get_desktop_info(desktop_id).await {
            Ok(v) if !jstr(&v, "clinkLvsOutHost").is_empty() => { info = Some(v); break; }
            Ok(_) => last_err = "视讯网关暂未分配完毕".into(),
            Err(e) => last_err = e,
        }
        if i + 1 < attempts {
            tokio::time::sleep(Duration::from_secs(wait_secs)).await;
        }
    }
    let info = match info {
        Some(v) => v,
        None => return Err(if last_err.is_empty() { "视讯网关暂未分配完毕".into() } else { last_err }),
    };
    let host = jstr(&info, "clinkLvsOutHost");

    let ws_url = format!("wss://{}/clinkProxy/{}/MAIN", host, desktop_id);
    let tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoCertVerify))
        .with_no_client_auth();
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(tls_config));
    // 上游以 Origin: https://pc.ctyun.cn 建连，网关侧会校验来源。
    let mut req = ws_url.into_client_request()
        .map_err(|e| format!("WebSocket 请求构造失败: {}", e))?;
    req.headers_mut().insert(
        "Origin",
        tokio_tungstenite::tungstenite::http::HeaderValue::from_static("https://pc.ctyun.cn"),
    );
    let (ws, _) = tokio_tungstenite::connect_async_tls_with_config(
        req, None, false, Some(connector),
    ).await.map_err(|e| format!("WebSocket 连接失败: {}", e))?;
    let (mut sink, mut stream) = ws.split();

    let host_parts: Vec<&str> = host.split(':').collect();
    let connect_msg = json!({
        "type": 1,
        "ssl": 1,
        "host": host_parts.first().copied().unwrap_or(""),
        "port": host_parts.get(1).copied().unwrap_or("443"),
        "ca": jstr(&info, "caCert"),
        "cert": jstr(&info, "clientCert"),
        "key": jstr(&info, "clientKey"),
        "servername": format!("{}:{}", jstr(&info, "host"), jstr(&info, "port")),
        "oqs": 0
    });
    sink.send(Message::Text(connect_msg.to_string().into())).await
        .map_err(|e| e.to_string())?;

    let init_bytes = base64::decode("UkVEUQIAAAACAAAAGgAAAAAAAAABAAEAAAABAAAAEgAAAAkAAAAECAAA")
        .unwrap_or_default();
    let deadline = tokio::time::sleep(Duration::from_secs(duration_secs));
    tokio::pin!(deadline);
    let mut init_timer = tokio::time::sleep(Duration::from_millis(500));
    tokio::pin!(init_timer);
    let mut init_sent = false;
    let mut handshake_done = false;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await;

    let log = client.log.clone();
    let acc_name = client.account.name.clone();
    log.log("Heartbeat", &format!("[{}] 保活长连接就绪 ({})", desktop_name, host), "success", &acc_name).await;

    loop {
        tokio::select! {
            _ = &mut deadline => {
                // 只有计时器真正走满 duration_secs 才是"周期完成"，此时 goal_achieved=true，
                // 上层据此确认挂机时长已达标（官方"使用1小时"累计）。
                let reason = match mode {
                    SessionMode::Pulse => "脉冲周期完成",
                    SessionMode::Hang => "挂机周期完成",
                };
                return Ok(SessionResult { success: true, reason: reason.into(), goal_achieved: true });
            }
            _ = &mut init_timer, if !init_sent => {
                let _ = sink.send(Message::Binary(init_bytes.clone().into())).await;
                init_sent = true;
            }
            _ = heartbeat.tick(), if handshake_done => {
                let _ = sink.send(Message::Binary(frame(7, &[]).into())).await;
            }
            message = stream.next() => {
                match message {
                    Some(Ok(Message::Binary(data))) => {
                        if data.starts_with(b"REDQ") {
                            if let Some(response) = redq_response(&data) {
                                let _ = sink.send(Message::Binary(response.into())).await;
                                log.log("Heartbeat", &format!("[{}] REDQ 校验通过，已回传加密应答", desktop_name), "success", &acc_name).await;
                            }
                            continue;
                        }
                        if data.len() < 6 { continue; }
                        let msg_type = u16::from_le_bytes([data[0], data[1]]);
                        let size = u32::from_le_bytes([data[2], data[3], data[4], data[5]]) as usize;

                        if msg_type == 4 {
                            let payload = &data[6..(6 + size.min(12)).min(data.len())];
                            let _ = sink.send(Message::Binary(frame(3, payload).into())).await;
                        } else if msg_type == 3 && data.len() >= 10 {
                            let gen = &data[6..10];
                            let mut payload = vec![0u8; 4];
                            payload.copy_from_slice(gen);
                            let mut ack = vec![0u8; 10];
                            ack[0..2].copy_from_slice(&1u16.to_le_bytes());
                            ack[2..6].copy_from_slice(&4u32.to_le_bytes());
                            ack[6..10].copy_from_slice(gen);
                            let _ = sink.send(Message::Binary(ack.into())).await;
                        } else if msg_type == 103 {
                            handshake_done = true;
                            // 118 身份帧用 login_info（userName/userId）；
                            // 112 登录帧的 token 必须来自桌面凭证 info（get_desktop_info 返回），
                            // 不能用 login_info 覆盖——否则 token 为空，网关认领失败，
                            // 官方"登录AI云电脑/使用1小时"永远不会达成（对齐上游 desktopInfo.token）。
                            let login_info = client.login_info.clone().unwrap_or(Value::Null);
                            let _ = sink.send(Message::Binary(frame118(&login_info).into())).await;
                            if mode == SessionMode::Hang {
                                let _ = sink.send(Message::Binary(frame112(desktop_id, &info, &client).into())).await;
                                let _ = sink.send(Message::Binary(frame(104, &[]).into())).await;
                                log.log("Heartbeat", &format!("[{}] 桌面认领已下发(Type 118/112/104, token={}字节)",
                                    desktop_name, jstr(&info, "token").len()), "success", &acc_name).await;
                            }
                            log.log("Heartbeat", &format!("[{}] 身份上报完成，在线状态已激活", desktop_name), "success", &acc_name).await;
                        } else if matches!(msg_type, 119 | 120 | 137) {
                            log.log("Heartbeat", &format!("[{}] 收到客户端在席信令 ({})", desktop_name, msg_type), "info", &acc_name).await;
                        }
                    }
                    Some(Ok(Message::Text(_))) => {}
                    Some(Ok(Message::Close(_))) | None => {
                        // 区分两种模式（对齐上游 runDesktopKeepAliveSession 的 close 判定）：
                        // - Pulse 脉冲：只是旁观保活，网关在 deadline 前关闭属正常轮转，
                        //   握手完成后按成功处理（等价 endSession('Pulse Finished')），
                        //   避免成功会话被误判失败、把看板状态覆盖成 error。
                        // - Hang 挂机：必须挂满 duration_secs（默认 3600s）官方才累计
                        //   "使用1小时"进度。远端提前关闭绝不能算"挂机周期完成"，否则
                        //   会话几秒就退出、官方时长永远停在 0/3600。这里返回
                        //   goal_achieved=false，让上层安排尾差补挂（对齐上游 MAX_ROUNDS）。
                        if mode == SessionMode::Pulse && handshake_done {
                            return Ok(SessionResult { success: true, reason: "脉冲周期完成".into(), goal_achieved: false });
                        }
                        let reason = if mode == SessionMode::Hang {
                            "挂机会话被网关提前关闭(未挂满时长)"
                        } else {
                            "连接被远端关闭(握手未完成)"
                        };
                        return Ok(SessionResult { success: false, reason: reason.into(), goal_achieved: false });
                    }
                    _ => {}
                }
            }
        }
    }
}


