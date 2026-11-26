mod api;
mod auth;
mod crypto;
mod ctyun;
mod desktop_view;
mod keeper;
mod models;
mod scheduler;
mod store;
mod tasks;

use api::AppState;
use auth::AuthManager;
use scheduler::Scheduler;
use std::net::SocketAddr;
use store::{LogHub, Store};
use tower_http::services::ServeDir;

#[tokio::main]
async fn main() {
    // tokio-tungstenite 的 rustls-tls-webpki-roots 会引入 aws-lc-rs，而本项目
    // 显式启用 ring；两个 crypto provider 并存会让 ClientConfig::builder() panic。
    // 这里在进程级固定安装 ring，保证保活 WebSocket 握手可用。
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let store = Store::load();
    let log = LogHub::new(store.data_dir.clone());
    let auth = AuthManager::new();

    // 端口优先级：CTYUN_PORT 环境变量（OpenWrt procd/UCI 传入） > 配置文件 > 默认 8571。
    // 这样 LuCI/UCI 改端口后，重启服务即可生效，不必手改数据目录里的 app_config.json。
    let port = std::env::var("CTYUN_PORT")
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .filter(|p| *p > 0)
        .unwrap_or_else(|| {
            let config = store.config.read().unwrap_or_else(|e| e.into_inner());
            config.settings.web_port
        });

    let scheduler = Scheduler::new(store.clone(), log.clone());
    std::sync::Arc::new(scheduler.clone()).start();

    let state = AppState {
        store,
        log: log.clone(),
        auth,
        scheduler,
    };

    // LuCI 原生视图（路由器 80 端口）需要跨域调用本服务 API（8571）。
    // 放行所有来源/方法/头；API 本身有 Bearer token 鉴权，故不会引入越权风险。
    let cors = tower_http::cors::CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any);

    let app = api::router(state)
        .nest_service("/static", ServeDir::new("static"))
        .layer(cors);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("云电脑控制中心已启动: http://{}", addr);
    log.log("System", &format!("服务已启动，监听端口 {}", port), "success", "").await;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("无法绑定监听端口");
    axum::serve(listener, app).await.expect("服务运行失败");
}
