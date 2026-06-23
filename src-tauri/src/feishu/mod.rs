pub mod http_server;

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FeishuConfig {
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    pub verification_token: Option<String>,
    pub encrypt_key: Option<String>,
    pub oauth_redirect_uri: Option<String>,
    pub default_chat_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuBot {
    pub config: FeishuConfig,
    pub tenant_access_token: Option<String>,
    pub last_error: Option<String>,
}

impl Default for FeishuBot {
    fn default() -> Self {
        Self {
            config: FeishuConfig::default(),
            tenant_access_token: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WebhookEvent {
    pub event_type: String,
    pub chat_id: String,
    pub sender_open_id: String,
    pub sender_user_id: Option<String>,
    pub message_id: String,
    pub root_message_id: Option<String>,
    pub text: String,
    pub mentions: Vec<String>,
    pub raw: serde_json::Value,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplyResult {
    pub message_id: String,
    pub chat_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub configured: bool,
    pub token_ready: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SharedFeishuState {
    pub bot: Arc<RwLock<FeishuBot>>,
}

impl SharedFeishuState {
    pub fn new(bot: FeishuBot) -> Self {
        Self {
            bot: Arc::new(RwLock::new(bot)),
        }
    }
}

pub use http_server::build_feishu_router;
pub use http_server::start_http_server;
