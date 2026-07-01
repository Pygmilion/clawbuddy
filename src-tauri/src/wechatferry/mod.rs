use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Command as TokioCommand, ChildStdin, ChildStdout};
use uuid::Uuid;

const WECHATFERRY_MAX_RECENT: usize = 200;
const WECHATFERRY_TCP_FALLBACK_PORT: u16 = 18790;
const DEFAULT_WECHATFERRY_START_TIMEOUT: Duration = Duration::from_secs(90);
const DEFAULT_WECHATFERRY_QR_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_WECHATFERRY_LOGIN_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WeChatFerryStatus {
    #[default]
    Idle,
    Starting,
    QrPending,
    Running,
    Stopping,
    Failed,
}

impl WeChatFerryStatus {
    pub fn description(&self) -> &'static str {
        match self {
            WeChatFerryStatus::Idle => "未启动",
            WeChatFerryStatus::Starting => "启动中",
            WeChatFerryStatus::QrPending => "等待扫码登录",
            WeChatFerryStatus::Running => "运行中",
            WeChatFerryStatus::Stopping => "停止中",
            WeChatFerryStatus::Failed => "启动失败",
        }
    }
}

#[derive(Debug, Clone)]
pub enum WeChatFerryEvent {
    StatusChanged { previous: WeChatFerryStatus, current: WeChatFerryStatus },
    QrCode { image_base64: String },
    LoginSuccess,
    LoginFailed { reason: String },
    Message { message: serde_json::Value },
}

#[derive(Debug, Clone)]
pub struct WeChatFerrySendRequest {
    pub trace_id: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct WeChatFerryRuntime {
    pub app_data_dir: PathBuf,
    pub executable_path: PathBuf,
    pub gateway_url: String,
    pub gateway_secret: Option<String>,
    pub log_dir: PathBuf,
    pub qr_mode: bool,
    pub enable_tcp_fallback: bool,
}

impl Default for WeChatFerryRuntime {
    fn default() -> Self {
        Self {
            app_data_dir: dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("ClawBuddy")
                .join("wechatferry"),
            executable_path: default_wechatferry_executable_path(),
            gateway_url: "http://127.0.0.1:18930".to_string(),
            gateway_secret: None,
            log_dir: PathBuf::from("."),
            qr_mode: true,
            enable_tcp_fallback: true,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WeChatFerryManager {
    pub runtime: Arc<Mutex<WeChatFerryRuntime>>,
    pub status: Arc<Mutex<WeChatFerryStatus>>,
    pub recent_messages: Arc<Mutex<VecDeque<serde_json::Value>>>,
    pub pending_sends: Arc<Mutex<VecDeque<WeChatFerrySendRequest>>>,
    pub stop_token: Arc<AtomicUsize>,
}

impl WeChatFerryManager {
    pub fn current_status(&self) -> WeChatFerryStatus {
        self.status.lock().map(|status| *status).unwrap_or(WeChatFerryStatus::Failed)
    }

    pub fn set_status(&self, status: WeChatFerryStatus) {
        if let Ok(mut guard) = self.status.lock() {
            *guard = status;
        }
    }

    pub async fn start(&self, app: &AppHandle) -> Result<(), String> {
        let runtime = self.runtime.lock().map_err(|e| e.to_string())?.clone();
        self.start_with_runtime(app, &runtime).await
    }

    pub async fn start_with_runtime(&self, app: &AppHandle, runtime: &WeChatFerryRuntime) -> Result<(), String> {
        let current = self.current_status();
        if matches!(current, WeChatFerryStatus::QrPending | WeChatFerryStatus::Running) {
            return Ok(());
        }
        if matches!(current, WeChatFerryStatus::Starting | WeChatFerryStatus::Stopping) {
            return Err("微信接入正在处理中，请稍候".into());
        }

        let token = self.next_token();
        self.stop_token.store(token, Ordering::SeqCst);
        self.set_status(WeChatFerryStatus::Starting);
        self.emit_status(app, self.current_status());

        let config_path = self.generate_config(runtime)?;
        let mut child = self.spawn_wechatferry(runtime, &config_path)?;
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.ok_or("无法捕获 wechatferry stdout")?;
        let app_handle = app.clone();
        let token = self.stop_token.load(Ordering::SeqCst);

        tauri::async_runtime::spawn(async move {
            let _ = Self::watch_process(app_handle, stdout, stdin, token).await;
        });

        self.wait_until_ready(runtime).await?;
        self.set_status(WeChatFerryStatus::Running);
        self.emit_status(app, self.current_status());
        Ok(())
    }

    pub async fn stop(&self, app: &AppHandle) -> Result<(), String> {
        let current = self.current_status();
        if matches!(current, WeChatFerryStatus::Idle | WeChatFerryStatus::Failed) {
            return Ok(());
        }
        if current == WeChatFerryStatus::Stopping {
            return Err("微信接入正在停止".into());
        }

        self.set_status(WeChatFerryStatus::Stopping);
        self.emit_status(app, self.current_status());
        self.stop_token.fetch_add(1, Ordering::SeqCst);

        let runtime = self.runtime.lock().map_err(|e| e.to_string())?.clone();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if self.current_status() == WeChatFerryStatus::Idle {
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.set_status(WeChatFerryStatus::Failed);
                self.emit_status(app, self.current_status());
                return Err("停止微信接入超时".into());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub async fn send_message(&self, payload: serde_json::Value) -> Result<serde_json::Value, String> {
        let status = self.current_status();
        if status != WeChatFerryStatus::Running {
            return Err(format!("当前微信状态不支持发送消息: {}", status as u8));
        }

        let trace_id = payload
            .get("trace_id")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let request = WeChatFerrySendRequest {
            trace_id,
            payload: payload.clone(),
        };

        if let Ok(mut pending) = self.pending_sends.lock() {
            pending.push_back(request.clone());
        }

        self.enqueue_send_request(request.clone());
        Ok(serde_json::json!({
            "ok": true,
            "trace_id": request.trace_id,
            "queued": true,
            "status": status as u8,
        }))
    }

    pub async fn receive_messages(&self) -> Result<Vec<serde_json::Value>, String> {
        let messages = if let Ok(guard) = self.recent_messages.lock() {
            guard.iter().cloned().collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        Ok(messages)
    }

    fn generate_config(&self, runtime: &WeChatFerryRuntime) -> Result<PathBuf, String> {
        let config_dir = runtime.app_data_dir.join("config");
        std::fs::create_dir_all(&config_dir).map_err(|e| format!("创建微信配置目录失败: {e}"))?;
        let config_path = config_dir.join("config.json");
        let config = serde_json::json!({
            "platform": detect_platform(),
            "executable_path": runtime.executable_path.display().to_string(),
            "log_dir": runtime.log_dir.display().to_string(),
            "qr_mode": runtime.qr_mode,
            "gateway": {
                "url": runtime.gateway_url,
                "secret": runtime.gateway_secret,
            }
        });
        std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap_or_default())
            .map_err(|e| format!("写入微信配置失败: {e}"))?;
        Ok(config_path)
    }

    fn spawn_wechatferry(&self, runtime: &WeChatFerryRuntime, config_path: &std::path::Path) -> Result<tokio::process::Child, String> {
        if !runtime.executable_path.exists() {
            return Err(format!(
                "未找到 wechatferry 可执行文件：{}\n\n\
                 请选择以下任一方式获取二进制文件：\n\
                 1. 从 GitHub Release 下载对应版本的 {}-{} 构建产物，解压后将二进制放到 runtime/wechatferry/\n\
                 2. 将已有的 wechatferry 二进制手动放入 runtime/wechatferry/\n\n\
                 如果使用分发目录方式，可直接放在 runtime/wechatferry/{}-{}/ 下，优先采用该路径。",
                runtime.executable_path.display(),
                detect_platform(),
                wechatferry_binary_name(),
                detect_platform(),
                wechatferry_binary_name(),
            ));
        }

        let mut command = TokioCommand::new(runtime.executable_path.clone());
        command
            .arg("--config")
            .arg(config_path)
            .arg("--bridge");

        if runtime.enable_tcp_fallback {
            command.arg("--tcp").arg("--port").arg(WECHATFERRY_TCP_FALLBACK_PORT.to_string());
        }

        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        command.spawn().map_err(|e| format!("启动 wechatferry 失败: {e}"))
    }

    async fn watch_process(
        app: AppHandle,
        mut stdout: ChildStdout,
        mut stdin: Option<ChildStdin>,
        token: usize,
    ) {
        let manager = match app.try_state::<WeChatFerryManager>() {
            Some(state) => state.inner().clone(),
            None => {
                eprintln!("未找到 WeChatFerryManager 状态");
                return;
            }
        };
        let mut reader = BufReader::new(stdout).lines();
        let status = manager.current_status();

        if status == WeChatFerryStatus::Running {
            let _ = manager.set_status(WeChatFerryStatus::Stopping);
            let _ = app.emit("wechatferry-status-changed", manager.current_status() as u8);
        }

        loop {
            tokio::select! {
                line = reader.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            if let Err(error) = manager.handle_event(&app, &line).await {
                                eprintln!("处理 wechatferry 事件失败: {error}");
                            }
                        }
                        Ok(None) => {
                            manager.mark_failed(&app, "wechatferry 已退出");
                            break;
                        }
                        Err(error) => {
                            manager.mark_failed(&app, &format!("读取 wechatferry 输出失败: {error}"));
                            break;
                        }
                    }
                }
                _ = async {
                    loop {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        if !manager.is_token_valid(token) {
                            break;
                        }
                    }
                } => {
                    let message = serde_json::json!({
                        "trace_id": Uuid::new_v4().to_string(),
                        "cmd": "stop",
                        "payload": {},
                    });
                    if let Err(error) = manager.write_command(stdin.as_mut(), &message).await {
                        eprintln!("向 wechatferry 发送停止命令失败: {error}");
                    }
                }
            }
        }

        manager.set_status(WeChatFerryStatus::Idle);
        let _ = app.emit("wechatferry-status-changed", manager.current_status() as u8);
    }

    async fn handle_event(&self, app: &AppHandle, line: &str) -> Result<(), String> {
        let event: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| format!("解析 wechatferry 事件失败: {e}"))?;

        let event_type = event
            .get("event")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");

        match event_type {
            "qr.ready" => {
                let qr_image = event
                    .get("payload")
                    .and_then(|payload| payload.get("qr_image_base64"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                if qr_image.is_null() {
                    self.set_status(WeChatFerryStatus::QrPending);
                    self.emit_status(app, self.current_status());
                    return Ok(());
                }
                self.add_recent_message(app, &event);
                self.set_status(WeChatFerryStatus::QrPending);
                let _ = app.emit(
                    "wechatferry-status-changed",
                    serde_json::json!({
                        "status": self.current_status() as u8,
                        "event": event,
                    }),
                );
            }
            "login.success" => {
                self.set_status(WeChatFerryStatus::Running);
                self.emit_status(app, self.current_status());
                self.add_recent_message(app, &event);
                let _ = app.emit("wechatferry-login-success", event);
            }
            "login.failed" => {
                self.set_status(WeChatFerryStatus::Failed);
                self.emit_status(app, self.current_status());
                self.add_recent_message(app, &event);
            }
            "message.private" | "message.group" | "message.temp" => {
                let message = event
                    .get("payload")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                self.add_recent_message(app, &message);
                let _ = app.emit("wechatferry-message", message);
            }
            other => {
                eprintln!("未知 wechatferry 事件: {other}");
            }
        }

        Ok(())
    }

    async fn write_command(&self, stdin: Option<&mut ChildStdin>, message: &serde_json::Value) -> Result<(), String> {
        let Some(stdin) = stdin else {
            return Ok(());
        };

        let mut command = message.clone();
        if !command.get("trace_id").map(|value| value.is_string()).unwrap_or(false) {
            if let Some(object) = command.as_object_mut() {
                object.insert("trace_id".into(), serde_json::Value::String(Uuid::new_v4().to_string()));
            }
        }

        let payload = serde_json::to_string(&command).map_err(|e| e.to_string())?;
        let line = format!("{payload}\n");
        stdin.write_all(line.as_bytes()).await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    fn enqueue_send_request(&self, request: WeChatFerrySendRequest) {
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let _ = manager.flush_pending_sends().await;
        });
    }

    async fn flush_pending_sends(&self) -> Result<(), String> {
        let runtime = self.runtime.lock().map_err(|e| e.to_string())?.clone();
        let mut send_index = 0;
        loop {
            let request = {
                let pending = self.pending_sends.lock().map_err(|e| e.to_string())?;
                pending.get(send_index).cloned()
            };

            let Some(request) = request else {
                break;
            };

            match self.dispatch_send(&runtime, &request).await {
                Ok(_) => {
                    if let Ok(mut pending) = self.pending_sends.lock() {
                        pending.remove(send_index);
                    } else {
                        break;
                    }
                }
                Err(error) => {
                    if error.contains("未运行") || error.contains("端口") {
                        break;
                    }
                    send_index = send_index.saturating_add(1);
                }
            }
        }

        Ok(())
    }

    async fn dispatch_send(&self, runtime: &WeChatFerryRuntime, request: &WeChatFerrySendRequest) -> Result<serde_json::Value, String> {
        let mut send = request.payload.clone();
        if let Some(object) = send.as_object_mut() {
            object.insert("trace_id".into(), serde_json::Value::String(request.trace_id.clone()));
        }

        let response = self.send_via_stdio(runtime, &send)
            .await
            .map_err(|error| format!("stdio 发送失败: {error}"))?
            .or_else(|| {
                let runtime = runtime.clone();
                tauri::async_runtime::block_on(async move {
                    self.send_via_tcp(&runtime, &send)
                        .await
                        .map_err(|error| format!("tcp 发送失败: {error}"))
                        .ok()
                        .flatten()
                })
            });

        response.ok_or_else(|| "发送消息失败".into())
    }

    async fn send_via_stdio(&self, runtime: &WeChatFerryRuntime, message: &serde_json::Value) -> Result<Option<serde_json::Value>, String> {
        if !runtime.executable_path.exists() {
            return Ok(None);
        }

        let mut command = TokioCommand::new(runtime.executable_path.clone());
        command
            .arg("--config")
            .arg(config_path_placeholder())
            .arg("send")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let mut stdin = child.stdin.take().ok_or("缺少 stdin")?;
        let stdout = child.stdout.take().ok_or("缺少 stdout")?;

        let input = serde_json::to_string(message).map_err(|e| e.to_string())?;
        stdin.write_all(input.as_bytes()).await.map_err(|e| e.to_string())?;
        stdin.write_all(b"\n").await.map_err(|e| e.to_string())?;
        drop(stdin);

        let mut reader = BufReader::new(stdout).lines();
        let response = reader.next_line().await.map_err(|e| e.to_string())?;

        let _ = child.wait().await;

        let value = match response {
            Some(line) => serde_json::from_str(&line).map_err(|e| e.to_string())?,
            None => return Ok(None),
        };
        Ok(Some(value))
    }

    async fn send_via_tcp(&self, runtime: &WeChatFerryRuntime, message: &serde_json::Value) -> Result<Option<serde_json::Value>, String> {
        if !runtime.enable_tcp_fallback {
            return Ok(None);
        }

        let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{}", WECHATFERRY_TCP_FALLBACK_PORT))
            .await
            .map_err(|_| "TCP 回退连接失败".to_string())?;

        let input = serde_json::to_string(message).map_err(|e| e.to_string())?;
        stream.write_all(input.as_bytes()).await.map_err(|e| e.to_string())?;
        stream.write_all(b"\n").await.map_err(|e| e.to_string())?;
        stream.shutdown().await.map_err(|e| e.to_string())?;

        let mut reader = BufReader::new(stream).lines();
        let response = reader.next_line().await.map_err(|e| e.to_string())?;

        let value = match response {
            Some(line) => serde_json::from_str(&line).map_err(|e| e.to_string())?,
            None => return Ok(None),
        };
        Ok(Some(value))
    }

    async fn wait_until_ready(&self, runtime: &WeChatFerryRuntime) -> Result<(), String> {
        let deadline = Instant::now() + DEFAULT_WECHATFERRY_START_TIMEOUT;
        loop {
            if self.is_wechatferry_ready(runtime).await {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("wechatferry 启动超时".into());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    async fn is_wechatferry_ready(&self, runtime: &WeChatFerryRuntime) -> bool {
        if !runtime.executable_path.exists() {
            return false;
        }
        match reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
        {
            Ok(client) => {
                let url = format!("{}/v1/wechatferry/health", runtime.gateway_url.trim_end_matches('/'));
                client.get(&url).send().await.map(|_| true).unwrap_or(false)
            }
            Err(_) => false,
        }
    }

    fn add_recent_message(&self, app: &AppHandle, message: &serde_json::Value) {
        if let Ok(mut recent) = self.recent_messages.lock() {
            if let Some(object) = message.as_object() {
                if let Some(text) = object.get("text").and_then(|value| value.as_str()) {
                    if !text.is_empty() {
                        recent.push_back(serde_json::json!({
                            "text": text,
                            "raw": message,
                        }));
                        if recent.len() > WECHATFERRY_MAX_RECENT {
                            recent.pop_front();
                        }
                    }
                }
            }
        }
        let _ = app.emit("wechatferry-message", message);
    }

    fn mark_failed(&self, app: &AppHandle, reason: &str) {
        self.set_status(WeChatFerryStatus::Failed);
        self.emit_status(app, self.current_status());
        let _ = app.emit("wechatferry-error", reason);
    }

    fn emit_status(&self, app: &AppHandle, status: WeChatFerryStatus) {
        let _ = app.emit("wechatferry-status-changed", status as u8);
    }

    fn next_token(&self) -> usize {
        static COUNTER: std::sync::Mutex<usize> = std::sync::Mutex::new(1);
        let mut counter = COUNTER.lock().unwrap();
        let token = *counter;
        *counter += 1;
        token
    }

    fn is_token_valid(&self, token: usize) -> bool {
        self.stop_token.load(Ordering::SeqCst) == token
    }
}

fn default_wechatferry_root() -> PathBuf {
    resolve_app_root().join("runtime").join("wechatferry")
}

fn default_wechatferry_executable_path() -> PathBuf {
    let platform_arch_dir = detect_platform();
    let binary_name = wechatferry_binary_name();
    let root = default_wechatferry_root();

    let platform_arch_path = root.join(platform_arch_dir).join(binary_name);
    if platform_arch_path.exists() {
        return platform_arch_path;
    }

    if root.join(binary_name).exists() {
        return root.join(binary_name);
    }

    platform_arch_path
}

fn wechatferry_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "wechatferry.exe"
    } else {
        "wechatferry"
    }
}

fn detect_platform() -> String {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    format!("{}-{}", os, arch)
}

fn resolve_app_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|path| path.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()))
}

fn config_path_placeholder() -> PathBuf {
    PathBuf::from("<config>")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WeChatFerryTransport {
    Stdio,
    Tcp,
}
