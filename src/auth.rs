use crate::models::{Session, User};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

const SESSION_TTL_SECONDS: i64 = 30 * 24 * 3600;

#[derive(Clone, Default)]
pub struct AuthManager {
    pub sessions: Arc<RwLock<HashMap<String, Session>>>,
}

impl AuthManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn create_session(&self, user: &User, remember: bool) -> String {
        let token = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp();
        let session = Session {
            user_id: user.id.clone(),
            username: user.username.clone(),
            role: user.role.clone(),
            max_quota: user.max_quota,
            remember,
            created_at: now,
            expires_at: now + SESSION_TTL_SECONDS,
        };
        self.sessions.write().await.insert(token.clone(), session);
        token
    }

    pub async fn verify(&self, token: &str, users: &[User]) -> Option<Session> {
        let mut sessions = self.sessions.write().await;
        let mut session = sessions.get(token)?.clone();
        if session.expires_at < chrono::Utc::now().timestamp() {
            sessions.remove(token);
            return None;
        }
        if let Some(user) = users.iter().find(|u| u.id == session.user_id) {
            session.username = user.username.clone();
            session.role = user.role.clone();
            session.max_quota = user.max_quota;
            sessions.insert(token.to_string(), session.clone());
            Some(session)
        } else {
            sessions.remove(token);
            None
        }
    }

    pub async fn delete(&self, token: &str) {
        self.sessions.write().await.remove(token);
    }
}

pub fn extract_token(
    auth_header: Option<&str>,
    query_token: Option<String>,
    cookie_header: Option<&str>,
) -> Option<String> {
    if let Some(header) = auth_header {
        if let Some(value) = header.strip_prefix("Bearer ") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    if let Some(token) = query_token {
        if !token.is_empty() {
            return Some(token);
        }
    }
    if let Some(cookie) = cookie_header {
        for part in cookie.split(';') {
            let part = part.trim();
            if let Some(value) = part.strip_prefix("token=") {
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}
