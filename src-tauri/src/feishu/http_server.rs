use crate::feishu::{HealthResponse, ReplyResult, SharedFeishuState, WebhookEvent};
use axum::body::Body;
use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::RwLock;

const TENANT_ACCESS_TOKEN_URL: &str =
    "https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal";
const SEND_MESSAGE_URL: &str = "https://open.feishu.cn/open-apis/im/v1/messages";
const GATEWAY_HOST: &str = "127.0.0.1";
const GATEWAY_PORT: u16 = 18930;
const GATEWAY_MESSAGE_ROUTE_URL: &str =
    "http://127.0.0.1:18930/api/v1/message/route";

#[derive(Debug, Clone, Deserialize)]
pub struct TenantAccessTokenRequest {
    pub app_id: String,
    pub app_secret: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateBotRequest {
    pub app_id: String,
    pub app_secret: String,
    pub verification_token: Option<String>,
    pub encrypt_key: Option<String>,
    pub oauth_redirect_uri: Option<String>,
    pub default_chat_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OAuthExchangeRequest {
    pub code: String,
    pub app_id: String,
    pub app_secret: String,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SendTextRequest {
    pub receive_id: String,
    pub text: String,
    pub receive_id_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WebhookChallenge {
    pub challenge: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeishuWebhookEvent {
    pub header: FeishuEventHeader,
    pub event: FeishuEventBody,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeishuEventHeader {
    pub event_id: String,
    pub event_type: String,
    pub create_time: String,
    pub token: String,
    pub app_id: String,
    pub tenant_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeishuEventBody {
    pub sender: FeishuSender,
    pub message: FeishuMessage,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeishuSender {
    pub sender_id: FeishuSenderId,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeishuSenderId {
    pub open_id: String,
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeishuMessage {
    pub message_id: String,
    pub root_id: Option<String>,
    pub chat_id: String,
    pub content: String,
}

pub async fn start_http_server(
    app: AppHandle,
    state: SharedFeishuState,
    router_factory: impl FnOnce(AppHandle, SharedFeishuState) -> Router,
) -> Result<(), String> {
    let emit_app = app.clone();
    let router = router_factory(app, state);
    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("无法启动飞书 HTTP 服务: {e}"))?;

    let local_addr = listener
        .local_addr()
        .map_err(|e| format!("无法读取飞书服务监听地址: {e}"))?;

    tauri::async_runtime::spawn(async move {
        if let Err(e) = axum::serve(listener, router.into_make_service()).await {
            eprintln!("[feishu] HTTP 服务停止: {e}");
        }
    });

    if let Err(e) = emit_app.emit("feishu-bridge-ready", local_addr.to_string()) {
        eprintln!("[feishu] 发送 bridge-ready 事件失败: {e}");
    }

    Ok(())
}

pub fn build_feishu_router(app: AppHandle, state: SharedFeishuState) -> Router {
    Router::new()
        .route("/feishu/health", get(health))
        .route("/feishu/bot", post(create_or_update_bot))
        .route("/feishu/oauth/exchange", post(oauth_exchange))
        .route("/feishu/webhook", post(webhook))
        .route("/feishu/message", post(send_text))
        .with_state(FeishuState { app, state })
}

#[derive(Debug, Clone)]
struct FeishuState {
    app: AppHandle,
    state: SharedFeishuState,
}

async fn health(State(state): State<FeishuState>) -> impl IntoResponse {
    let bot = state.state.bot.read().await;
    let configured = bot.config.app_id.is_some() && bot.config.app_secret.is_some();
    let token_ready = bot.tenant_access_token.is_some();

    Json(HealthResponse {
        status: "ok".to_string(),
        configured,
        token_ready,
        last_error: bot.last_error.clone(),
    })
}

async fn create_or_update_bot(
    State(state): State<FeishuState>,
    Json(payload): Json<CreateBotRequest>,
) -> impl IntoResponse {
    let mut bot = state.state.bot.write().await;
    bot.config.app_id = Some(payload.app_id);
    bot.config.app_secret = Some(payload.app_secret);
    bot.config.verification_token = payload.verification_token;
    bot.config.encrypt_key = payload.encrypt_key;
    bot.config.oauth_redirect_uri = payload.oauth_redirect_uri;
    bot.config.default_chat_id = payload.default_chat_id;
    bot.tenant_access_token = None;
    bot.last_error = None;

    Json(serde_json::json!({
        "status": "ok",
    }))
    .into_response()
}

async fn oauth_exchange(
    State(state): State<FeishuState>,
    Json(payload): Json<OAuthExchangeRequest>,
) -> impl IntoResponse {
    let client = match Client::builder()
        .user_agent("ClawBuddy/feishu-bridge")
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("创建 HTTP 客户端失败: {e}"),
            );
        }
    };

    let body = client
        .post(TENANT_ACCESS_TOKEN_URL)
        .json(&serde_json::json!({
            "app_id": payload.app_id,
            "app_secret": payload.app_secret,
        }))
        .send()
        .await;

    let Ok(response) = body else {
        return error_response(
            StatusCode::BAD_GATEWAY,
            "请求 tenant_access_token 失败",
        );
    };

    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if !status.is_success() {
        let mut bot = state.state.bot.write().await;
        bot.last_error = Some(format!("获取 tenant_access_token 失败: {status}, body={body}"));
        return error_response(
            StatusCode::BAD_GATEWAY,
            format!("获取 tenant_access_token 失败: {status}, body={body}"),
        );
    }

    let mut bot = state.state.bot.write().await;
    bot.tenant_access_token = Some(body.clone());
    bot.config.app_id = Some(payload.app_id);
    bot.config.app_secret = Some(payload.app_secret);
    bot.config.oauth_redirect_uri = Some(payload.redirect_uri);

    Json(serde_json::json!({
        "status": "ok",
        "raw": body,
    })).into_response()
}

async fn webhook(State(state): State<FeishuState>) -> impl IntoResponse {
    let body = axum::body::to_bytes(Body::default(), usize::MAX)
        .await
        .unwrap_or_default();
    let raw_body = String::from_utf8(body.to_vec()).unwrap_or_default();

    if let Err(e) = handle_feishu_webhook(state.app.clone(), state.state.clone(), raw_body).await {
        eprintln!("[feishu] webhook 处理失败: {e}");
    }

    Json(serde_json::json!({"status":"accepted"})).into_response()
}

async fn send_text(
    State(state): State<FeishuState>,
    Json(payload): Json<SendTextRequest>,
) -> impl IntoResponse {
    let token = {
        let bot = state.state.bot.read().await;
        bot.tenant_access_token.clone()
    };

    let Some(token) = token else {
        return error_response(StatusCode::BAD_REQUEST, "未配置 tenant_access_token，请先完成 OAuth 或创建 Bot");
    };

    let receive_id_type = payload
        .receive_id_type
        .unwrap_or_else(|| "open_id".to_string());
    let content = serde_json::json!({"text": payload.text});

    let client = match Client::builder()
        .user_agent("ClawBuddy/feishu-bridge")
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("创建 HTTP 客户端失败: {e}"));
        }
    };

    let response = client
        .post(SEND_MESSAGE_URL)
        .query(&[("receive_id_type", receive_id_type)])
        .bearer_auth(token)
        .json(&serde_json::json!({
            "receive_id": payload.receive_id,
            "msg_type": "text",
            "content": content,
        }))
        .send()
        .await;

    let Ok(response) = response else {
        let mut bot = state.state.bot.write().await;
        bot.last_error = Some("发送消息网络失败".to_string());
        return error_response(StatusCode::BAD_GATEWAY, "发送消息网络失败");
    };

    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if !status.is_success() {
        let mut bot = state.state.bot.write().await;
        bot.last_error = Some(format!("发送消息失败: {status}, body={body}"));
        return error_response(
            StatusCode::BAD_GATEWAY,
            format!("发送消息失败: {status}, body={body}"),
        );
    }

    let parsed: serde_json::Value = match serde_json::from_str(&body) {
        Ok(parsed) => parsed,
        Err(_) => {
            return error_response(StatusCode::BAD_GATEWAY, "解析发送消息响应失败");
        }
    };

    let message_id = parsed
        .get("data")
        .and_then(|data| data.get("message_id"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();

    let chat_id = payload.receive_id;

    Json(ReplyResult {
        message_id,
        chat_id,
        status: "sent".to_string(),
    })
    .into_response()
}

async fn handle_feishu_webhook(
    app: AppHandle,
    state: SharedFeishuState,
    raw_body: String,
) -> Result<(), String> {
    let event: FeishuWebhookEvent = serde_json::from_str(&raw_body)
        .map_err(|e| format!("解析飞书事件失败: {e}"))?;

    if event.header.event_type == "url_verification" {
        let challenge = WebhookChallenge {
            challenge: event.header.event_id,
        };
        let _ = app.emit("feishu-webhook-challenge", challenge);
        return Ok(());
    }

    if event.header.event_type != "im.message.receive_v1" {
        return Ok(());
    }

    let message = event.event.message;
    let chat_id = message.chat_id;
    let sender_open_id = event.event.sender.sender_id.open_id;
    let text = extract_text(&message.content)?;
    let mentions = extract_mentions(&message.content);

    let normalized = WebhookEvent {
        event_type: event.header.event_type,
        chat_id,
        sender_open_id,
        sender_user_id: event.event.sender.sender_id.user_id,
        message_id: message.message_id,
        root_message_id: message.root_id,
        text,
        mentions,
        raw: serde_json::from_str(&raw_body).unwrap_or_default(),
        timestamp: event.header.create_time.parse().unwrap_or(0),
    };

    let route_payload = serde_json::json!({
        "chat_id": normalized.chat_id.clone(),
        "sender_open_id": normalized.sender_open_id.clone(),
        "text": normalized.text.clone(),
        "message_type": "text",
    });

    let _ = app.emit("feishu-message-received", normalized);

    if let Err(error) = route_message_to_gateway(&app, route_payload).await {
        eprintln!("[feishu] 路由到 Gateway 失败: {error}");
    }

    Ok(())
}

async fn route_message_to_gateway(
    app: &AppHandle,
    payload: serde_json::Value,
) -> Result<(), String> {
    // 通过 AppHandle.state() 直接访问 GatewayManager，而不是 invoke Tauri command
    let gateway_manager = app.state::<crate::GatewayManager>();
    
    if !gateway_manager.is_ready().await {
        gateway_manager.start().await
            .map_err(|error| format!("启动 Gateway 失败: {error}"))?;
    }

    if !gateway_manager.is_ready().await {
        return Err("Gateway 仍未就绪，跳过路由".into());
    }

    let client = Client::builder()
        .user_agent("ClawBuddy/feishu-bridge")
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| format!("创建 Gateway HTTP 客户端失败: {error}"))?;

    let response = client
        .post(GATEWAY_MESSAGE_ROUTE_URL)
        .json(&payload)
        .send()
        .await
        .map_err(|error| format!("请求 Gateway 路由接口失败: {error}"))?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if status.is_success() {
        Ok(())
    } else {
        Err(format!("Gateway 路由失败: {status}, body={body}"))
    }
}

fn extract_text(content: &str) -> Result<String, String> {
    let parsed: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| format!("解析消息内容失败: {e}"))?;
    parsed
        .get("text")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "消息内容缺少 text 字段".into())
}

fn extract_mentions(content: &str) -> Vec<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(content);
    let Some(text_value) = parsed.ok().and_then(|v| v.get("text").cloned()) else {
        return Vec::new();
    };

    let text = match text_value.as_str() {
        Some(s) => s,
        None => return Vec::new(),
    };

    let mut mentions = Vec::new();
    let mut remaining = text;

    while let Some(start) = remaining.find("<at user_id=\"") {
        if let Some(id_start) = remaining[start + 14..].find('"') {
            let user_id = &remaining[start + 14..start + 14 + id_start];
            let rest = &remaining[start + 14 + id_start..];
            let name = if let Some(gt_start) = rest.find('>') {
                if let Some(lt_end) = rest[gt_start + 1..].find("</at>") {
                    let name_candidate = &rest[gt_start + 1..gt_start + 1 + lt_end];
                    if name_candidate.is_empty() {
                        user_id
                    } else {
                        name_candidate
                    }
                } else {
                    user_id
                }
            } else {
                user_id
            };
            mentions.push(format!("{}:{}", user_id, name));
            remaining = &rest[rest.find("</at>").unwrap_or(rest.len())..];
        } else {
            break;
        }
    }

    mentions
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    let body = Json(serde_json::json!({"error": message.into()}));
    (status, body).into_response()
}
