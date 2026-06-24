use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, MenuEvent};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::time::{sleep, Instant};

mod wechatferry;
use wechatferry::{WeChatFerryManager, WeChatFerryStatus};

mod feishu;
use feishu::SharedFeishuState;

// ClawBuddy 运行自带的独立 gateway：使用专用端口与独立状态目录，避免与用户自己的
// openclaw CLI / launchd 服务（默认 18789）冲突，也不会改动用户真实的 ~/.openclaw 配置。
const GATEWAY_PORT: u16 = 18789;
const GATEWAY_ADDR: &str = "127.0.0.1:18789";

// ClawBuddy 当前默认使用 StepFun（阶跃星辰）作为模型后端。
const STEPFUN_MODEL_REF: &str = "stepfun/step-3.5-flash";
// 默认走国内站（与国内 StepFun key 匹配）；openclaw 内置默认是国际站 api.stepfun.ai，
// 国内 key 打国际站会返回 401。
const STEPFUN_BASE_URL: &str = "https://api.stepfun.com/v1";

fn gateway_state_dir() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".clawbuddy")
        .join("state")
}

fn credentials_path() -> std::path::PathBuf {
    gateway_state_dir().join("clawbuddy-credentials.json")
}

fn read_stepfun_key() -> Option<String> {
    let raw = fs::read_to_string(credentials_path()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("stepfunApiKey")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn write_stepfun_key(key: &str) -> Result<(), String> {
    let dir = gateway_state_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("无法创建状态目录: {e}"))?;
    let body = serde_json::json!({ "stepfunApiKey": key });
    fs::write(credentials_path(), serde_json::to_string_pretty(&body).unwrap_or_default())
        .map_err(|e| format!("无法写入凭据: {e}"))
}

// 杀掉占用网关端口的进程（openclaw 启动器会自我 respawn，按端口杀最可靠）。
fn kill_gateway_on_port() {
    if let Ok(output) = Command::new("lsof")
        .args(["-ti", &format!("tcp:{GATEWAY_PORT}"), "-sTCP:LISTEN"])
        .output()
    {
        if let Ok(text) = std::str::from_utf8(&output.stdout) {
            for pid in text.split_whitespace() {
                let _ = Command::new("kill").arg("-9").arg(pid).status();
            }
        }
    }
}

#[tauri::command]
async fn toggle_window(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().map_err(|e| e.to_string())? {
            window.hide().map_err(|e| e.to_string())?;
        } else {
            window.show().map_err(|e| e.to_string())?;
            window.set_focus().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayStatus {
    Checking,
    Ready,
    Starting,
    Failed,
}

impl Default for GatewayStatus {
    fn default() -> Self {
        Self::Checking
    }
}

#[derive(Debug, Clone, Default)]
pub struct GatewayManager {
    status: Arc<Mutex<GatewayStatus>>,
}

impl GatewayManager {
    pub async fn start(&self) -> Result<(), String> {
        {
            let mut status = self.status.lock().map_err(|e| e.to_string())?;
            if *status == GatewayStatus::Ready {
                return Ok(());
            }
            if *status == GatewayStatus::Starting {
                return Err("Gateway 已启动中，请稍候".to_string());
            }
            *status = GatewayStatus::Starting;
        }

        if !is_listening(GATEWAY_ADDR).await {
            start_gateway_process().map_err(|e| format!("无法启动 Gateway: {e}"))?;
        }

        wait_until_ready().await?;
        self.set_status(GatewayStatus::Ready);

        Ok(())
    }

    pub fn set_status(&self, status: GatewayStatus) {
        if let Ok(mut guard) = self.status.lock() {
            *guard = status;
        }
    }

    // 重启网关：杀掉当前监听进程后重新拉起，使新写入的凭据/配置（如 StepFun key）生效。
    pub async fn restart(&self) -> Result<(), String> {
        kill_gateway_on_port();
        self.set_status(GatewayStatus::Checking);
        for _ in 0..20 {
            if !is_listening(GATEWAY_ADDR).await {
                break;
            }
            sleep(Duration::from_millis(500)).await;
        }
        self.start().await
    }

    pub fn current_status(&self) -> GatewayStatus {
        self.status
            .lock()
            .map(|status| *status)
            .unwrap_or(GatewayStatus::Failed)
    }

    pub async fn is_ready(&self) -> bool {
        check_gateway_ready_internal().await
    }
}

fn resolve_app_root() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|path| path.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()))
}

fn bundled_script_path() -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let app_root = resolve_app_root();
    // 顺序很重要：开发环境优先用项目 node_modules 的 openclaw（可被"应用内升级"更新），
    // 避免被 target/debug 里残留的旧版本副本 shadow；找不到才回退到生产打包路径。
    let candidates = [
        cwd.join("node_modules").join("openclaw").join("openclaw.mjs"),
        cwd.join("..").join("node_modules").join("openclaw").join("openclaw.mjs"),
        // 生产 macOS：Contents/Resources/bundled/lib/...
        app_root
            .join("..")
            .join("Resources")
            .join("bundled")
            .join("lib")
            .join("node_modules")
            .join("openclaw")
            .join("openclaw.mjs"),
        // 生产其它平台：可执行文件同级 bundled/lib/...
        app_root
            .join("bundled")
            .join("lib")
            .join("node_modules")
            .join("openclaw")
            .join("openclaw.mjs"),
        cwd.join("src-tauri")
            .join("bundled")
            .join("lib")
            .join("node_modules")
            .join("openclaw")
            .join("openclaw.mjs"),
    ];
    for candidate in &candidates {
        if candidate.exists() {
            return candidate.clone();
        }
    }
    cwd.join("node_modules").join("openclaw").join("openclaw.mjs")
}

#[tauri::command]
async fn start_gateway(manager: State<'_, GatewayManager>) -> Result<(), String> {
    manager.start().await
}

fn bundled_node_path() -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) { "node.exe" } else { "node" };
    let app_root = resolve_app_root();
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let candidates = [
        // 生产：macOS .app 资源目录 Contents/Resources/bundled/bin/node
        app_root.join("..").join("Resources").join("bundled").join("bin").join(name),
        // 生产（其它平台）：可执行文件同级 bundled/bin/node
        app_root.join("bundled").join("bin").join(name),
        // 开发：项目根 bundled/bin/node
        cwd.join("bundled").join("bin").join(name),
        cwd.join("..").join("bundled").join("bin").join(name),
    ];
    candidates.into_iter().find(|path| path.exists())
}

#[tauri::command]
fn get_node_path() -> Result<String, String> {
    // 优先使用随应用打包的 node，保证一键安装、版本可控（openclaw 要求 Node >= 22.19）。
    if let Some(path) = bundled_node_path() {
        return Ok(path.display().to_string());
    }

    let candidates = [
        std::path::PathBuf::from("/usr/local/bin/node"),
        std::path::PathBuf::from("/opt/homebrew/bin/node"),
    ];

    if let Some(path) = candidates.iter().find(|path| path.exists()) {
        return Ok(path.display().to_string());
    }

    let output = Command::new("which")
        .arg("node")
        .output()
        .map_err(|e| format!("无法定位 Node.js: {e}"))?;

    if output.status.success() {
        let path = std::str::from_utf8(&output.stdout)
            .map_err(|e| format!("Node.js 路径解析失败: {e}"))?
            .trim();
        if !path.is_empty() {
            return Ok(path.to_string());
        }
    }

    Err("未找到 Node.js，请安装后重试".to_string())
}

// 定位随包预置的飞书插件目录（生产在 Resources/bundled/extensions/feishu，开发在 src-tauri/bundled/...）。
fn bundled_feishu_plugin_dir() -> Option<std::path::PathBuf> {
    let app_root = resolve_app_root();
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let candidates = [
        app_root.join("..").join("Resources").join("bundled").join("extensions").join("feishu"),
        app_root.join("bundled").join("extensions").join("feishu"),
        cwd.join("src-tauri").join("bundled").join("extensions").join("feishu"),
        cwd.join("bundled").join("extensions").join("feishu"),
    ];
    candidates
        .into_iter()
        .find(|p| p.join("openclaw.plugin.json").exists())
}

// 随包插件里的 `node_modules/openclaw` 是指向打包机项目目录的绝对软链，在别的电脑上会失效，
// 导致插件 `import 'openclaw'` 报 "Cannot find package 'openclaw'"。这里把失效的软链重新指向
// 随包的 openclaw 运行时目录。
fn bundled_openclaw_pkg_dir() -> Option<std::path::PathBuf> {
    let dir = bundled_script_path().parent()?.to_path_buf();
    if dir.exists() {
        Some(dir)
    } else {
        None
    }
}

fn fix_dangling_openclaw_symlinks(dir: &std::path::Path, bundled_oc: &std::path::Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let ft = meta.file_type();
        if ft.is_symlink() {
            // 仅修复名为 openclaw 且已失效（target 不存在）的软链。
            if entry.file_name() == "openclaw" && !path.exists() {
                let _ = fs::remove_file(&path);
                let _ = std::os::unix::fs::symlink(bundled_oc, &path);
            }
            // 不进入软链，避免环路。
        } else if ft.is_dir() {
            fix_dangling_openclaw_symlinks(&path, bundled_oc);
        }
    }
}

fn fix_bundled_plugin_symlinks(state_dir: &std::path::Path) {
    let Some(bundled_oc) = bundled_openclaw_pkg_dir() else {
        return;
    };
    // 通用兜底：在 state/node_modules 放一个 openclaw 软链。插件里 import 'openclaw' / 'openclaw/plugin-sdk/*'
    // 会沿 node_modules 逐级向上解析，最终命中这里——无论插件自带的软链是失效还是被打包器丢弃。
    let nm = state_dir.join("node_modules");
    let _ = fs::create_dir_all(&nm);
    let link = nm.join("openclaw");
    let needs = match fs::symlink_metadata(&link) {
        Ok(_) => !link.exists(), // 存在但失效 → 重建
        Err(_) => true,          // 不存在 → 建
    };
    if needs {
        let _ = fs::remove_file(&link);
        let _ = fs::remove_dir_all(&link);
        let _ = std::os::unix::fs::symlink(&bundled_oc, &link);
    }
    // 顺带把插件内已存在但失效的 openclaw 软链也重新指向。
    fix_dangling_openclaw_symlinks(&state_dir.join("extensions"), &bundled_oc);
    fix_dangling_openclaw_symlinks(&state_dir.join("npm").join("projects"), &bundled_oc);
}

// 首次启动时把随包的飞书插件复制到状态目录的 extensions/feishu（已存在则跳过），
// 这样无需联网装 clawhub 插件，飞书频道开箱即用。
fn copy_bundled_feishu_into_state(state_dir: &std::path::Path) {
    let dest = state_dir.join("extensions").join("feishu");
    // 已正确安装（含 dist）则不动。
    if dest.join("dist").exists() {
        return;
    }
    let Some(src) = bundled_feishu_plugin_dir() else {
        return;
    };
    let ext_dir = state_dir.join("extensions");
    let _ = fs::create_dir_all(&ext_dir);
    // 清理可能的残缺目录，避免 cp -R 把源目录嵌套进去。
    let _ = fs::remove_dir_all(&dest);
    let _ = std::process::Command::new("cp")
        .arg("-R")
        .arg(&src)
        .arg(&dest)
        .status();
}

// 定位随包预置的 npm/projects 目录（StepFun provider 等以 npm 项目形式安装的插件）。
fn bundled_npm_projects_dir() -> Option<std::path::PathBuf> {
    let app_root = resolve_app_root();
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let candidates = [
        app_root.join("..").join("Resources").join("bundled").join("npm").join("projects"),
        app_root.join("bundled").join("npm").join("projects"),
        cwd.join("src-tauri").join("bundled").join("npm").join("projects"),
        cwd.join("bundled").join("npm").join("projects"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

// 首次启动时把随包的 npm 插件（StepFun provider、微信插件等）复制到状态目录，
// 这样无需联网装 npm 插件即可用。已存在的项目目录会跳过，保留用户状态。
fn copy_bundled_npm_projects_into_state(state_dir: &std::path::Path) {
    let Some(src_projects) = bundled_npm_projects_dir() else {
        return;
    };
    let dest_projects = state_dir.join("npm").join("projects");
    let _ = fs::create_dir_all(&dest_projects);
    let Ok(entries) = fs::read_dir(&src_projects) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let dest = dest_projects.join(entry.file_name());
        if dest.exists() {
            continue;
        }
        let _ = std::process::Command::new("cp")
            .arg("-R")
            .arg(entry.path())
            .arg(&dest)
            .status();
    }
}

// 为 agent 工作区预置默认的角色/记忆文件（仅当文件不存在时写入，避免覆盖用户编辑）。
fn seed_agent_files(workspace: &std::path::Path) {
    let defaults: [(&str, &str); 5] = [
        (
            "SOUL.md",
            "# 人设（SOUL）\n\n我是 ClawBuddy 🦞，你的本地 AI 伙伴。\n\n- 语气：友好、简洁、直接，默认用中文回答。\n- 风格：先给结论，再按需展开；能动手就动手，不绕弯子。\n- 边界：涉及敏感或不确定的操作会先和你确认。\n\n（可在此自由修改 Claw 的性格与说话风格。）\n",
        ),
        (
            "USER.md",
            "# 关于你（USER）\n\n在这里记录关于你的信息，Claw 会在对话中参考：\n\n- 称呼：\n- 职业 / 身份：\n- 常用语言：中文\n- 偏好：（喜欢的回答风格、长度、格式等）\n- 正在做的事：\n\n（填得越具体，Claw 越懂你。）\n",
        ),
        (
            "IDENTITY.md",
            "# 身份（IDENTITY）\n\n- 名称：ClawBuddy\n- 标识：🦞\n- 定位：运行在本机的私人 AI 助手，可在微信 / 飞书一处对话。\n",
        ),
        (
            "AGENTS.md",
            "# 协作说明（AGENTS）\n\n记录与其它 agent / 渠道协作的约定、各自职责与交接方式。\n\n（暂无内容，可按需补充。）\n",
        ),
        (
            "TOOLS.md",
            "# 工具说明（TOOLS）\n\n记录可用工具的用法、注意事项与偏好。\n\n（暂无内容，可按需补充。）\n",
        ),
    ];
    for (name, content) in defaults {
        let path = workspace.join(name);
        if !path.exists() {
            let _ = fs::write(&path, content);
        }
    }
}

fn provision_gateway_config(state_dir: &std::path::Path) -> Result<(), String> {
    let workspace = state_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|e| format!("无法创建工作区目录: {e}"))?;
    seed_agent_files(&workspace);
    copy_bundled_feishu_into_state(state_dir);
    copy_bundled_npm_projects_into_state(state_dir);
    fix_bundled_plugin_symlinks(state_dir);

    let config_path = state_dir.join("openclaw.json");
    let workspace_str = workspace.to_string_lossy().to_string();

    // 写入一个自包含的本地配置：loopback 绑定、无 auth、关闭浏览器/WebView 的设备身份校验
    // （ClawBuddy 是本地单机 UI，等价于受信任的 Control UI），并预置一个默认 dev agent。
    let config = serde_json::json!({
        "gateway": {
            "mode": "local",
            "bind": "loopback",
            "auth": { "mode": "none" },
            "controlUi": { "dangerouslyDisableDeviceAuth": true }
        },
        "agents": {
            "defaults": { "workspace": workspace_str, "skipBootstrap": true, "model": STEPFUN_MODEL_REF },
            "list": [{
                "id": "dev",
                "default": true,
                "workspace": workspace_str,
                "model": STEPFUN_MODEL_REF,
                "identity": { "name": "ClawBuddy", "emoji": "🦞" }
            }]
        },
        "models": {
            "providers": {
                "stepfun": { "baseUrl": STEPFUN_BASE_URL }
            }
        },
        "plugins": {
            "entries": {
                "feishu": { "enabled": true },
                "stepfun": { "enabled": true },
                "openclaw-weixin": { "enabled": true }
            }
        },
        "tools": {
            "web": {
                "search": { "enabled": true, "provider": "duckduckgo" }
            }
        }
    });

    // 已存在则补齐 controlUi 开关与默认模型，尽量保留用户在该独立配置里的其它改动。
    if config_path.exists() {
        if let Ok(raw) = fs::read_to_string(&config_path) {
            if let Ok(mut existing) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(gateway) = existing.get_mut("gateway").and_then(|g| g.as_object_mut()) {
                    gateway
                        .entry("controlUi")
                        .or_insert_with(|| serde_json::json!({}));
                    if let Some(ui) = gateway.get_mut("controlUi").and_then(|u| u.as_object_mut()) {
                        ui.insert("dangerouslyDisableDeviceAuth".into(), serde_json::json!(true));
                    }
                }
                if let Some(defaults) = existing
                    .get_mut("agents")
                    .and_then(|a| a.get_mut("defaults"))
                    .and_then(|d| d.as_object_mut())
                {
                    defaults
                        .entry("model")
                        .or_insert_with(|| serde_json::json!(STEPFUN_MODEL_REF));
                }
                // 确保 stepfun 走国内站 base URL。
                {
                    let models = existing
                        .as_object_mut()
                        .unwrap()
                        .entry("models")
                        .or_insert_with(|| serde_json::json!({}));
                    let providers = models
                        .as_object_mut()
                        .unwrap()
                        .entry("providers")
                        .or_insert_with(|| serde_json::json!({}));
                    let stepfun = providers
                        .as_object_mut()
                        .unwrap()
                        .entry("stepfun")
                        .or_insert_with(|| serde_json::json!({}));
                    if let Some(obj) = stepfun.as_object_mut() {
                        obj.entry("baseUrl")
                            .or_insert_with(|| serde_json::json!(STEPFUN_BASE_URL));
                    }
                }
                // 确保飞书插件在配置里启用（插件文件随包预置到 extensions/feishu）。
                {
                    let plugins = existing
                        .as_object_mut()
                        .unwrap()
                        .entry("plugins")
                        .or_insert_with(|| serde_json::json!({}));
                    let entries = plugins
                        .as_object_mut()
                        .unwrap()
                        .entry("entries")
                        .or_insert_with(|| serde_json::json!({}));
                    if let Some(entries) = entries.as_object_mut() {
                        entries
                            .entry("feishu")
                            .or_insert_with(|| serde_json::json!({ "enabled": true }));
                        entries
                            .entry("stepfun")
                            .or_insert_with(|| serde_json::json!({ "enabled": true }));
                        entries
                            .entry("openclaw-weixin")
                            .or_insert_with(|| serde_json::json!({ "enabled": true }));
                    }
                }
                // 默认启用网页搜索（DuckDuckGo，无需 API key）。仅在用户未配置时补默认值。
                {
                    let tools = existing
                        .as_object_mut()
                        .unwrap()
                        .entry("tools")
                        .or_insert_with(|| serde_json::json!({}));
                    let web = tools
                        .as_object_mut()
                        .unwrap()
                        .entry("web")
                        .or_insert_with(|| serde_json::json!({}));
                    let search = web
                        .as_object_mut()
                        .unwrap()
                        .entry("search")
                        .or_insert_with(|| serde_json::json!({}));
                    if let Some(obj) = search.as_object_mut() {
                        obj.entry("enabled").or_insert_with(|| serde_json::json!(true));
                        obj.entry("provider")
                            .or_insert_with(|| serde_json::json!("duckduckgo"));
                    }
                }
                let _ = fs::write(&config_path, serde_json::to_string_pretty(&existing).unwrap_or(raw));
                return Ok(());
            }
        }
    }

    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap_or_default())
        .map_err(|e| format!("无法写入 Gateway 配置: {e}"))?;
    Ok(())
}

// openclaw 2026.6.9 起 stepfun 是需单独安装的外部 provider 插件。若已配置 key 但插件缺失，
// 自动安装一次（带 key 检查 + 已装检查，正常启动不会触网）。
fn stepfun_provider_installed(state_dir: &std::path::Path) -> bool {
    let projects = state_dir.join("npm").join("projects");
    let Ok(entries) = fs::read_dir(&projects) else {
        return false;
    };
    for entry in entries.flatten() {
        if entry
            .path()
            .join("node_modules")
            .join("@openclaw")
            .join("stepfun-provider")
            .join("package.json")
            .exists()
        {
            return true;
        }
    }
    false
}

fn ensure_stepfun_provider(state_dir: &std::path::Path) {
    if read_stepfun_key().is_none() || stepfun_provider_installed(state_dir) {
        return;
    }
    let Ok(node) = get_node_path() else { return };
    let script = bundled_script_path();
    let _ = Command::new(&node)
        .env("OPENCLAW_STATE_DIR", state_dir)
        .arg(&script)
        .args(["plugins", "install", "@openclaw/stepfun-provider"])
        .status();
}

fn start_gateway_process() -> Result<(), String> {
    let node = get_node_path()?;
    let script = bundled_script_path();

    let state_dir = gateway_state_dir();
    fs::create_dir_all(&state_dir).map_err(|e| format!("无法创建状态目录: {e}"))?;
    provision_gateway_config(&state_dir)?;
    ensure_stepfun_provider(&state_dir);
    // 若已保存 key 但配置里还没有 apiKey（如旧版本升级而来），回填到配置，确保 provider 能拿到。
    if let Some(key) = read_stepfun_key() {
        write_stepfun_apikey_to_config(&state_dir, &key);
    }

    let port = GATEWAY_PORT.to_string();
    println!(
        "[gateway] starting gateway with node={node} script={} state={}",
        script.display(),
        state_dir.display()
    );

    // 关键：--port/--allow-unconfigured 属于 `gateway run` 子命令，必须带上 `run`。
    // OPENCLAW_STATE_DIR 指向 ClawBuddy 独立状态目录，与用户的 ~/.openclaw 完全隔离。
    let mut command = Command::new(node);
    command.env("OPENCLAW_STATE_DIR", &state_dir);
    // 若用户已在界面填入 StepFun key，则通过环境变量注入（openclaw stepfun 插件读取 STEPFUN_API_KEY）。
    if let Some(key) = read_stepfun_key() {
        command.env("STEPFUN_API_KEY", key);
    }

    let child = command
        .arg(script)
        .arg("gateway")
        .arg("run")
        .arg("--port")
        .arg(&port)
        .arg("--allow-unconfigured")
        .arg("--auth")
        .arg("none")
        .arg("--bind")
        .arg("loopback")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map(|child| {
            println!("[gateway] gateway process spawned pid={}", child.id());
            child
        })
        .map_err(|e| format!("无法启动 Gateway: {e}"))?;

    let stdout = child.stdout.ok_or("无法读取 Gateway stdout")?;
    let stderr = child.stderr.ok_or("无法读取 Gateway stderr")?;

    // 把网关输出落到日志文件，便于「导出诊断日志」抓现场（每次启动覆盖为当前会话）。
    let log_dir = state_dir.join("logs");
    let _ = fs::create_dir_all(&log_dir);
    let log_file = std::sync::Arc::new(std::sync::Mutex::new(
        std::fs::File::create(log_dir.join("gateway.log")).ok(),
    ));

    let log_out = log_file.clone();
    std::thread::spawn(move || {
        use std::io::Write;
        let mut out = stdout;
        let mut buf = [0u8; 1024];
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 {
                break;
            }
            if let Ok(text) = std::str::from_utf8(&buf[..n]) {
                eprint!("{}", text);
                if let Ok(mut guard) = log_out.lock() {
                    if let Some(f) = guard.as_mut() {
                        let _ = f.write_all(&buf[..n]);
                    }
                }
            }
        }
    });

    let log_err = log_file.clone();
    std::thread::spawn(move || {
        use std::io::Write;
        let mut err = stderr;
        let mut buf = [0u8; 1024];
        while let Ok(n) = err.read(&mut buf) {
            if n == 0 {
                break;
            }
            if let Ok(text) = std::str::from_utf8(&buf[..n]) {
                eprint!("{}", text);
                if let Ok(mut guard) = log_err.lock() {
                    if let Some(f) = guard.as_mut() {
                        let _ = f.write_all(&buf[..n]);
                    }
                }
            }
        }
    });

    Ok(())
}

async fn wait_until_ready() -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if check_gateway_ready_internal().await {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err("Gateway 启动超时".to_string());
        }

        sleep(Duration::from_secs(2)).await;
    }
}

pub async fn watch_process(app: AppHandle, manager: GatewayManager) {
    loop {
        sleep(Duration::from_secs(10)).await;
        if !is_listening(GATEWAY_ADDR).await {
            manager.set_status(GatewayStatus::Failed);
            let _ = manager.start().await;
            let _ = app.emit("gateway-status-changed", manager.current_status() as u8);
            continue;
        }

        let child_status = Command::new(get_node_path().unwrap_or_else(|_| "node".into()))
            .arg("-e")
            .arg("process.exit(0)")
            .status();

        if child_status.map(|status| !status.success()).unwrap_or(false) {
            manager.set_status(GatewayStatus::Failed);
            let _ = manager.start().await;
            let _ = app.emit("gateway-status-changed", manager.current_status() as u8);
        }
    }
}

async fn is_listening(address: &str) -> bool {
    tokio::net::TcpStream::connect(address).await.is_ok()
}

async fn check_gateway_ready_internal() -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("[gateway] reqwest client build failed: {error}");
            return false;
        }
    };

    let result = match client
        .get(format!("http://{GATEWAY_ADDR}/health"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
    {
        Ok(response) => {
            eprintln!("[gateway] health status {}", response.status());
            response.status().is_success()
        }
        Err(error) => {
            eprintln!("[gateway] health request failed: {error}");
            false
        }
    };

    result
}

#[tauri::command]
async fn check_gateway_ready() -> bool {
    check_gateway_ready_internal().await
}

#[tauri::command]
fn get_gateway_status(manager: State<'_, GatewayManager>) -> u8 {
    manager.current_status() as u8
}

// 是否已配置 StepFun API Key（用于界面回显，不返回明文）。
#[tauri::command]
fn get_stepfun_key_status() -> bool {
    read_stepfun_key().is_some()
}

// 保存界面填入的 StepFun API Key 并重启网关使其生效。
#[tauri::command]
async fn set_stepfun_key(manager: State<'_, GatewayManager>, key: String) -> Result<(), String> {
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("API Key 不能为空".to_string());
    }
    write_stepfun_key(&key)?;
    // 同时把 key 写进 openclaw 配置（models.providers.stepfun.apiKey）。
    // provider 插件通过配置解析 key，不依赖进程环境变量，跨重启/跨机器更可靠。
    write_stepfun_apikey_to_config(&gateway_state_dir(), &key);
    manager.restart().await
}

// 把 StepFun key 写入 openclaw 配置的 models.providers.stepfun.apiKey（并补 baseUrl、启用插件）。
fn write_stepfun_apikey_to_config(state_dir: &std::path::Path, key: &str) {
    let _ = update_config(state_dir, |root| {
        let models = root.entry("models").or_insert_with(|| serde_json::json!({}));
        if let Some(models) = models.as_object_mut() {
            let providers = models.entry("providers").or_insert_with(|| serde_json::json!({}));
            if let Some(providers) = providers.as_object_mut() {
                let stepfun = providers.entry("stepfun").or_insert_with(|| serde_json::json!({}));
                if let Some(obj) = stepfun.as_object_mut() {
                    obj.insert("apiKey".into(), serde_json::json!(key));
                    obj.entry("baseUrl")
                        .or_insert_with(|| serde_json::json!(STEPFUN_BASE_URL));
                }
            }
        }
        let plugins = root.entry("plugins").or_insert_with(|| serde_json::json!({}));
        if let Some(plugins) = plugins.as_object_mut() {
            let entries = plugins.entry("entries").or_insert_with(|| serde_json::json!({}));
            if let Some(entries) = entries.as_object_mut() {
                entries.insert("stepfun".into(), serde_json::json!({ "enabled": true }));
            }
        }
    });
}

// 查询 StepFun 账户余额/充值/赠送金额（GET /v1/accounts）。
#[tauri::command]
async fn get_stepfun_account() -> Result<serde_json::Value, String> {
    let key = read_stepfun_key().ok_or_else(|| "尚未配置 StepFun API Key".to_string())?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("创建请求失败: {e}"))?;
    let resp = client
        .get(format!("{STEPFUN_BASE_URL}/accounts"))
        .bearer_auth(key)
        .send()
        .await
        .map_err(|e| format!("请求 StepFun 失败: {e}"))?;
    if !resp.status().is_success() {
        let code = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("StepFun 返回 {code}: {body}"));
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("解析返回失败: {e}"))
}

// 读改写 openclaw 配置的通用助手。
fn update_config<F: FnOnce(&mut serde_json::Map<String, serde_json::Value>)>(
    state_dir: &std::path::Path,
    f: F,
) -> Result<(), String> {
    let path = state_dir.join("openclaw.json");
    let mut cfg: serde_json::Value = fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let root = cfg.as_object_mut().ok_or("配置根不是对象")?;
    f(root);
    fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap_or_default())
        .map_err(|e| format!("写入配置失败: {e}"))
}

// 把默认 agent 与列表里所有 agent 的模型设为 model_ref（如 "stepfun/step-3.5-flash"）。
fn set_agents_model(root: &mut serde_json::Map<String, serde_json::Value>, model_ref: &str) {
    let agents = root.entry("agents").or_insert_with(|| serde_json::json!({}));
    if let Some(agents) = agents.as_object_mut() {
        let defaults = agents.entry("defaults").or_insert_with(|| serde_json::json!({}));
        if let Some(d) = defaults.as_object_mut() {
            d.insert("model".into(), serde_json::json!(model_ref));
        }
        if let Some(list) = agents.get_mut("list").and_then(|l| l.as_array_mut()) {
            for agent in list {
                if let Some(obj) = agent.as_object_mut() {
                    obj.insert("model".into(), serde_json::json!(model_ref));
                }
            }
        }
    }
}

// 添加/更新一个自定义 OpenAI 兼容 provider，并将其设为当前模型。
#[tauri::command]
async fn save_model_provider(
    manager: State<'_, GatewayManager>,
    id: String,
    base_url: String,
    api_key: String,
    model: String,
) -> Result<(), String> {
    let id = id.trim().to_string();
    let base_url = base_url.trim().to_string();
    let model = model.trim().to_string();
    if id.is_empty() || base_url.is_empty() || model.is_empty() {
        return Err("名称、Base URL、模型名都不能为空".to_string());
    }
    let state_dir = gateway_state_dir();
    let model_ref = format!("{id}/{model}");
    update_config(&state_dir, |root| {
        let models = root.entry("models").or_insert_with(|| serde_json::json!({}));
        if let Some(models) = models.as_object_mut() {
            let providers = models.entry("providers").or_insert_with(|| serde_json::json!({}));
            if let Some(providers) = providers.as_object_mut() {
                providers.insert(
                    id.clone(),
                    serde_json::json!({
                        "api": "openai-completions",
                        "baseUrl": base_url,
                        "apiKey": api_key.trim(),
                        "models": [{ "id": model }]
                    }),
                );
            }
        }
        set_agents_model(root, &model_ref);
    })?;
    manager.restart().await
}

// 切换当前使用的模型（如切回 StepFun 默认）。
#[tauri::command]
async fn set_active_model(manager: State<'_, GatewayManager>, model_ref: String) -> Result<(), String> {
    let model_ref = model_ref.trim().to_string();
    if model_ref.is_empty() {
        return Err("模型不能为空".to_string());
    }
    let state_dir = gateway_state_dir();
    update_config(&state_dir, |root| set_agents_model(root, &model_ref))?;
    manager.restart().await
}

// 读取当前模型与已配置的 provider 列表（供设置页回显）。
#[tauri::command]
fn get_model_config() -> Result<serde_json::Value, String> {
    let path = gateway_state_dir().join("openclaw.json");
    let cfg: serde_json::Value = fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let active = cfg
        .get("agents")
        .and_then(|a| a.get("defaults"))
        .and_then(|d| d.get("model"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let providers_obj = cfg
        .get("models")
        .and_then(|m| m.get("providers"))
        .and_then(|p| p.as_object())
        .cloned()
        .unwrap_or_default();
    let providers: Vec<String> = providers_obj.keys().cloned().collect();

    // 可一键切换的模型列表：StepFun 两个固定模型 + 其它自定义 provider 的模型。
    let mut models: Vec<serde_json::Value> = vec![
        serde_json::json!({ "ref": "stepfun/step-3.5-flash", "label": "StepFun 3.5 Flash" }),
        serde_json::json!({ "ref": "stepfun/step-3.7-flash", "label": "StepFun 3.7（多模态）" }),
    ];
    for (id, prov) in &providers_obj {
        if id == "stepfun" {
            continue;
        }
        if let Some(ms) = prov.get("models").and_then(|m| m.as_array()) {
            for m in ms {
                if let Some(mid) = m.get("id").and_then(|x| x.as_str()) {
                    let r = format!("{id}/{mid}");
                    models.push(serde_json::json!({ "ref": r, "label": r }));
                }
            }
        }
    }

    Ok(serde_json::json!({ "activeModel": active, "providers": providers, "models": models }))
}

#[tauri::command]
async fn start_wechat_login(manager: State<'_, WeChatFerryManager>, app: AppHandle) -> Result<(), String> {
    manager.start(&app).await
}

#[tauri::command]
fn get_wechat_status(manager: State<'_, WeChatFerryManager>) -> u8 {
    manager.current_status() as u8
}

#[tauri::command]
async fn send_wechat_message(
    manager: State<'_, WeChatFerryManager>,
    payload: String,
) -> Result<serde_json::Value, String> {
    let payload: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|e| format!("payload 解析失败: {e}"))?;
    manager.send_message(payload).await
}

#[tauri::command]
async fn receive_wechat_messages(
    manager: State<'_, WeChatFerryManager>,
) -> Result<Vec<serde_json::Value>, String> {
    manager.receive_messages().await
}

#[tauri::command]
async fn create_feishu_bot(
    app: AppHandle,
    state: State<'_, SharedFeishuState>,
    app_id: String,
    app_secret: String,
    verification_token: Option<String>,
    encrypt_key: Option<String>,
    oauth_redirect_uri: Option<String>,
    default_chat_id: Option<String>,
) -> Result<String, String> {
    let mut bot = state.bot.write().await;
    bot.config.app_id = Some(app_id);
    bot.config.app_secret = Some(app_secret);
    bot.config.verification_token = verification_token;
    bot.config.encrypt_key = encrypt_key;
    bot.config.oauth_redirect_uri = oauth_redirect_uri;
    bot.config.default_chat_id = default_chat_id;
    bot.tenant_access_token = None;
    bot.last_error = None;
    drop(bot);
    let _ = app.emit("feishu-config-changed", ());
    Ok("saved".into())
}

#[tauri::command]
async fn send_feishu_text(
    state: State<'_, SharedFeishuState>,
    receive_id: String,
    text: String,
) -> Result<String, String> {
    let bot = state.bot.read().await;
    let token = bot.tenant_access_token.clone().ok_or("未配置 tenant_access_token")?;
    drop(bot);

    let client = reqwest::Client::builder()
        .user_agent("ClawBuddy/feishu-bridge")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let response = client
        .post("https://open.feishu.cn/open-apis/im/v1/messages")
        .query(&[("receive_id_type", "open_id")])
        .bearer_auth(token)
        .json(&serde_json::json!({
            "receive_id": receive_id,
            "msg_type": "text",
            "content": {"text": text}
        }))
        .send()
        .await
        .map_err(|e| format!("发送消息失败: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("发送消息失败: {}", response.status()));
    }

    Ok("sent".into())
}

#[tauri::command]
async fn get_feishu_health(state: State<'_, SharedFeishuState>) -> Result<serde_json::Value, String> {
    let bot = state.bot.read().await;
    let configured = bot.config.app_id.is_some() && bot.config.app_secret.is_some();
    let token_ready = bot.tenant_access_token.is_some();
    Ok(serde_json::json!({
        "status": "ok",
        "configured": configured,
        "token_ready": token_ready,
        "last_error": bot.last_error
    }))
}

#[tauri::command]
async fn oauth_exchange(
    state: State<'_, SharedFeishuState>,
    code: String,
    app_id: String,
    app_secret: String,
    redirect_uri: String,
) -> Result<serde_json::Value, String> {
    let mut bot = state.bot.write().await;
    bot.config.app_id = Some(app_id);
    bot.config.app_secret = Some(app_secret);
    bot.config.oauth_redirect_uri = Some(redirect_uri);

    let client = reqwest::Client::builder()
        .user_agent("ClawBuddy/feishu-bridge")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let response = client
        .post("https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal")
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "code": code
        }))
        .send()
        .await
        .map_err(|e| format!("OAuth 请求失败: {e}"))?;

    let body = response.text().await.map_err(|e| format!("读取 OAuth 响应失败: {e}"))?;

    Ok(serde_json::json!({"raw": body}))
}

// ===== 微信扫码登录（基于 openclaw 官方 @tencent-weixin/openclaw-weixin 插件）=====

const WEIXIN_PLUGIN_SPEC: &str = "@tencent-weixin/openclaw-weixin@2.4.3";

// 在独立 state 目录下解析已安装插件的 login-qr.js（安装路径含随机 hash，需遍历查找）。
fn resolve_weixin_qr_module(state_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let projects = state_dir.join("npm").join("projects");
    let entries = fs::read_dir(&projects).ok()?;
    for entry in entries.flatten() {
        let candidate = entry
            .path()
            .join("node_modules")
            .join("@tencent-weixin")
            .join("openclaw-weixin")
            .join("dist")
            .join("src")
            .join("auth")
            .join("login-qr.js");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

// 确保微信插件已安装+启用；缺失时通过 openclaw CLI 安装（首次需联网下载）。
fn ensure_weixin_plugin(state_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    if let Some(path) = resolve_weixin_qr_module(state_dir) {
        return Ok(path);
    }

    let node = get_node_path()?;
    let script = bundled_script_path();
    let run_cli = |args: &[&str]| -> Result<(), String> {
        let status = Command::new(&node)
            .env("OPENCLAW_STATE_DIR", state_dir)
            .arg(&script)
            .args(args)
            .status()
            .map_err(|e| format!("运行 openclaw {args:?} 失败: {e}"))?;
        if !status.success() {
            return Err(format!("openclaw {args:?} 退出码非 0"));
        }
        Ok(())
    };

    run_cli(&["plugins", "install", WEIXIN_PLUGIN_SPEC])?;
    run_cli(&["plugins", "enable", "openclaw-weixin"])?;

    resolve_weixin_qr_module(state_dir).ok_or_else(|| "插件安装后仍未找到 login-qr.js".to_string())
}

// 启动微信扫码登录：调用 openclaw 官方 `channels login`（它会显示二维码、阻塞等待扫码、
// 成功后正式注册账号到独立 state）。解析 stdout/stderr 里的 liteapp 二维码链接转成 Tauri
// 事件；登录成功后重启网关，让 openclaw-weixin 渠道在网关内运行（收发消息都归网关）。
#[tauri::command]
async fn wechat_login_start(app: AppHandle, manager: State<'_, GatewayManager>) -> Result<(), String> {
    let manager = manager.inner().clone();
    std::thread::spawn(move || {
        let emit = |value: serde_json::Value| {
            let _ = app.emit("wechat-login-event", value);
        };

        emit(serde_json::json!({ "type": "preparing", "message": "正在准备微信插件…" }));

        let state_dir = gateway_state_dir();
        if let Err(error) = ensure_weixin_plugin(&state_dir) {
            emit(serde_json::json!({ "type": "error", "message": error }));
            return;
        }

        let node = match get_node_path() {
            Ok(node) => node,
            Err(error) => {
                emit(serde_json::json!({ "type": "error", "message": error }));
                return;
            }
        };
        let script = bundled_script_path();

        let child = Command::new(&node)
            .env("OPENCLAW_STATE_DIR", &state_dir)
            .arg(&script)
            .arg("channels")
            .arg("login")
            .arg("--channel")
            .arg("openclaw-weixin")
            .arg("--verbose")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();

        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                emit(serde_json::json!({ "type": "error", "message": format!("启动微信登录进程失败: {error}") }));
                return;
            }
        };

        // 在 stdout / stderr 两路里找二维码链接（openclaw 的用户提示与日志可能分流）。
        let emit_for_qr = app.clone();
        let scan_qr = move |line: &str, app: &AppHandle| {
            if let Some(pos) = line.find("https://liteapp.weixin.qq.com/") {
                let url: String = line[pos..]
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(|c: char| c == '"' || c == '\'' || c == ')')
                    .to_string();
                if !url.is_empty() {
                    let _ = app.emit(
                        "wechat-login-event",
                        serde_json::json!({ "type": "qr", "url": url, "message": "请用手机微信扫描二维码" }),
                    );
                }
            }
        };

        if let Some(stderr) = child.stderr.take() {
            let app = emit_for_qr.clone();
            let scan = scan_qr.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    scan(&line, &app);
                }
            });
        }

        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                scan_qr(&line, &emit_for_qr);
            }
        }

        let succeeded = child.wait().map(|status| status.success()).unwrap_or(false);

        if succeeded {
            emit(serde_json::json!({ "type": "connected", "message": "微信已连接，正在启动消息通道…" }));
            // 重启网关，使其加载刚注册的微信账号并运行 openclaw-weixin 渠道（收发消息）。
            tauri::async_runtime::block_on(async {
                let _ = manager.restart().await;
            });
            emit(serde_json::json!({ "type": "connected", "message": "微信已连接，消息通道已就绪" }));
        } else {
            emit(serde_json::json!({ "type": "failed", "message": "登录未完成或二维码已过期，请重试" }));
        }
    });

    Ok(())
}

// ===== 飞书扫码登录（基于 openclaw 官方 @openclaw/feishu 插件的设备码"扫码自动建 bot"流程）=====

const FEISHU_PLUGIN_SPEC: &str = "clawhub:@openclaw/feishu";

fn resolve_feishu_qr_module(state_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = state_dir.join("extensions").join("feishu").join("dist");
    let entries = fs::read_dir(&dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("app-registration-") && name.ends_with(".js") {
            return Some(entry.path());
        }
    }
    None
}

fn ensure_feishu_plugin(state_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    if let Some(path) = resolve_feishu_qr_module(state_dir) {
        return Ok(path);
    }
    let node = get_node_path()?;
    let script = bundled_script_path();
    let run_cli = |args: &[&str]| -> Result<(), String> {
        let status = Command::new(&node)
            .env("OPENCLAW_STATE_DIR", state_dir)
            .arg(&script)
            .args(args)
            .status()
            .map_err(|e| format!("运行 openclaw {args:?} 失败: {e}"))?;
        if !status.success() {
            return Err(format!("openclaw {args:?} 退出码非 0"));
        }
        Ok(())
    };
    run_cli(&["plugins", "install", FEISHU_PLUGIN_SPEC])?;
    run_cli(&["plugins", "enable", "feishu"])?;
    resolve_feishu_qr_module(state_dir)
        .ok_or_else(|| "飞书插件安装后仍未找到 app-registration 模块".to_string())
}

fn resolve_feishu_login_helper() -> Result<std::path::PathBuf, String> {
    let app_root = resolve_app_root();
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let candidates = [
        app_root.join("..").join("Resources").join("runtime").join("feishu-login.mjs"),
        app_root.join("runtime").join("feishu-login.mjs"),
        cwd.join("src-tauri").join("runtime").join("feishu-login.mjs"),
        cwd.join("runtime").join("feishu-login.mjs"),
        cwd.join("..").join("runtime").join("feishu-login.mjs"),
    ];
    candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| "未找到 feishu-login.mjs".to_string())
}

// 把扫码自动创建的 bot 凭据写入配置：channels.feishu（单账号默认层，isConfigured 检查读此处）。
fn write_feishu_account(state_dir: &std::path::Path, app_id: &str, app_secret: &str) -> Result<(), String> {
    let config_path = state_dir.join("openclaw.json");
    let mut config: serde_json::Value = fs::read_to_string(&config_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    let root = config.as_object_mut().ok_or("配置根不是对象")?;
    let channels = root.entry("channels").or_insert_with(|| serde_json::json!({}));
    let feishu = channels
        .as_object_mut()
        .ok_or("channels 不是对象")?
        .entry("feishu")
        .or_insert_with(|| serde_json::json!({}));
    let feishu = feishu.as_object_mut().ok_or("channels.feishu 不是对象")?;
    feishu.insert("enabled".into(), serde_json::json!(true));
    feishu.insert("appId".into(), serde_json::json!(app_id));
    feishu.insert("appSecret".into(), serde_json::json!(app_secret));

    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap_or_default())
        .map_err(|e| format!("写入飞书配置失败: {e}"))
}

#[tauri::command]
async fn feishu_login_start(app: AppHandle, manager: State<'_, GatewayManager>) -> Result<(), String> {
    let manager = manager.inner().clone();
    std::thread::spawn(move || {
        let emit = |value: serde_json::Value| {
            let _ = app.emit("feishu-login-event", value);
        };

        emit(serde_json::json!({ "type": "preparing", "message": "正在准备飞书插件…" }));

        let state_dir = gateway_state_dir();
        let qr_module = match ensure_feishu_plugin(&state_dir) {
            Ok(path) => path,
            Err(error) => {
                emit(serde_json::json!({ "type": "error", "message": error }));
                return;
            }
        };
        let node = match get_node_path() {
            Ok(node) => node,
            Err(error) => {
                emit(serde_json::json!({ "type": "error", "message": error }));
                return;
            }
        };
        let helper = match resolve_feishu_login_helper() {
            Ok(helper) => helper,
            Err(error) => {
                emit(serde_json::json!({ "type": "error", "message": error }));
                return;
            }
        };

        let child = Command::new(&node)
            .env("OPENCLAW_STATE_DIR", &state_dir)
            .arg(&helper)
            .arg(&qr_module)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();

        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                emit(serde_json::json!({ "type": "error", "message": format!("启动飞书登录进程失败: {error}") }));
                return;
            }
        };

        let mut credentials: Option<(String, String)> = None;
        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if let Some(rest) = line.strip_prefix("@@CLAWFS@@ ") {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(rest) {
                        if value.get("type").and_then(|t| t.as_str()) == Some("connected") {
                            if let (Some(id), Some(secret)) = (
                                value.get("appId").and_then(|v| v.as_str()),
                                value.get("appSecret").and_then(|v| v.as_str()),
                            ) {
                                credentials = Some((id.to_string(), secret.to_string()));
                            }
                        }
                        emit(value);
                    }
                }
            }
        }
        let _ = child.wait();

        if let Some((app_id, app_secret)) = credentials {
            if let Err(error) = write_feishu_account(&state_dir, &app_id, &app_secret) {
                emit(serde_json::json!({ "type": "error", "message": format!("保存飞书凭据失败: {error}") }));
                return;
            }
            tauri::async_runtime::block_on(async {
                let _ = manager.restart().await;
            });
            emit(serde_json::json!({ "type": "ready", "message": "飞书已连接，消息通道已就绪" }));
        }
    });

    Ok(())
}

// ===== 渠道配对审批（陌生发送者首次需 bot 主人批准）=====

fn run_openclaw_capture(state_dir: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let node = get_node_path()?;
    let script = bundled_script_path();
    let output = Command::new(&node)
        .env("OPENCLAW_STATE_DIR", state_dir)
        .arg(&script)
        .args(args)
        .output()
        .map_err(|e| format!("运行 openclaw {args:?} 失败: {e}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// 列出某渠道待审批的配对请求（返回 requests 数组）。
#[tauri::command]
fn pairing_list(channel: String) -> Result<serde_json::Value, String> {
    let state_dir = gateway_state_dir();
    let out = run_openclaw_capture(&state_dir, &["pairing", "list", "--channel", &channel, "--json"])?;
    // 输出里可能混有告警行，截取首个 '{' 到末个 '}' 作为 JSON。
    let json = match (out.find('{'), out.rfind('}')) {
        (Some(start), Some(end)) if end > start => &out[start..=end],
        _ => return Ok(serde_json::json!([])),
    };
    let parsed: serde_json::Value = serde_json::from_str(json).unwrap_or_else(|_| serde_json::json!({}));
    Ok(parsed.get("requests").cloned().unwrap_or_else(|| serde_json::json!([])))
}

// 批准一个配对码。
#[tauri::command]
fn pairing_approve(channel: String, code: String) -> Result<(), String> {
    let state_dir = gateway_state_dir();
    let node = get_node_path()?;
    let script = bundled_script_path();
    let status = Command::new(&node)
        .env("OPENCLAW_STATE_DIR", &state_dir)
        .arg(&script)
        .args(["pairing", "approve", "--channel", &channel, &code, "--notify"])
        .status()
        .map_err(|e| format!("批准配对失败: {e}"))?;
    if !status.success() {
        return Err("批准配对失败（退出码非 0）".to_string());
    }
    Ok(())
}

// ===== 应用内升级 OpenClaw =====

fn resolve_npm() -> Result<String, String> {
    if let Ok(output) = Command::new("which").arg("npm").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(path);
            }
        }
    }
    for candidate in ["/opt/homebrew/bin/npm", "/usr/local/bin/npm"] {
        if std::path::Path::new(candidate).exists() {
            return Ok(candidate.to_string());
        }
    }
    // 从 `which node` 同级推导 npm。
    if let Ok(output) = Command::new("which").arg("node").output() {
        if output.status.success() {
            let node = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Some(npm) = std::path::Path::new(&node).parent().map(|d| d.join("npm")) {
                if npm.exists() {
                    return Ok(npm.display().to_string());
                }
            }
        }
    }
    Err("未找到 npm，无法升级（开发环境需要 npm）".to_string())
}

fn project_root() -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    if cwd.join("package.json").exists() {
        return cwd;
    }
    if cwd.join("..").join("package.json").exists() {
        return cwd.join("..");
    }
    cwd
}

// 升级 openclaw 到最新版并重启网关（开发环境，用 npm；生产打包另议）。
#[tauri::command]
async fn upgrade_openclaw(manager: State<'_, GatewayManager>) -> Result<String, String> {
    let manager = manager.inner().clone();
    let npm = resolve_npm()?;
    let root = project_root();

    let output = Command::new(&npm)
        .current_dir(&root)
        .args(["install", "openclaw@latest", "--no-audit", "--no-fund"])
        .output()
        .map_err(|e| format!("npm 执行失败: {e}"))?;
    if !output.status.success() {
        return Err(format!("npm install 失败: {}", String::from_utf8_lossy(&output.stderr)));
    }

    // 删除可能残留的旧版本副本（dev 下会 shadow node_modules，导致升级不生效）。
    let _ = fs::remove_dir_all(root.join("src-tauri/target/debug/bundled/lib"));

    manager.restart().await?;

    let version = fs::read_to_string(root.join("node_modules/openclaw/package.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("version").and_then(|x| x.as_str()).map(String::from))
        .unwrap_or_else(|| "未知".to_string());
    Ok(version)
}

// 检查 openclaw 是否有新版本（对比 node_modules 当前版本与 npm 最新版本）。
#[tauri::command]
fn check_openclaw_update() -> Result<serde_json::Value, String> {
    let root = project_root();
    let current = fs::read_to_string(root.join("node_modules").join("openclaw").join("package.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("version").and_then(|x| x.as_str()).map(String::from))
        .unwrap_or_else(|| "unknown".to_string());

    let npm = resolve_npm()?;
    let output = Command::new(&npm)
        .args(["view", "openclaw", "version"])
        .output()
        .map_err(|e| format!("检查更新失败: {e}"))?;
    let latest = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let update_available = !latest.is_empty() && latest != "unknown" && latest != current;
    Ok(serde_json::json!({
        "current": current,
        "latest": latest,
        "updateAvailable": update_available
    }))
}

// 读取 claw 配置文件原文（供设置页高级编辑）。
#[tauri::command]
fn get_claw_config() -> Result<String, String> {
    let path = gateway_state_dir().join("openclaw.json");
    fs::read_to_string(&path).map_err(|e| format!("读取配置失败: {e}"))
}

// 写入 claw 配置文件（校验 JSON 合法后写入并重启网关）。
#[tauri::command]
async fn set_claw_config(manager: State<'_, GatewayManager>, raw: String) -> Result<(), String> {
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("JSON 格式错误，未保存: {e}"))?;
    let pretty = serde_json::to_string_pretty(&parsed).unwrap_or(raw);
    let path = gateway_state_dir().join("openclaw.json");
    fs::write(&path, pretty).map_err(|e| format!("写入配置失败: {e}"))?;
    manager.restart().await
}

// 递归脱敏：把 apiKey / secret / token / password 等字段的值替换为占位符。
fn redact_secrets(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                let kl = k.to_lowercase();
                let sensitive = kl.contains("apikey")
                    || kl.contains("secret")
                    || kl.contains("token")
                    || kl.contains("password");
                if sensitive {
                    if let serde_json::Value::String(s) = val {
                        let n = s.chars().count();
                        *val = serde_json::json!(format!("<redacted:{n} chars>"));
                        continue;
                    }
                }
                redact_secrets(val);
            }
        }
        serde_json::Value::Array(arr) => {
            for it in arr.iter_mut() {
                redact_secrets(it);
            }
        }
        _ => {}
    }
}

fn tail_file(path: &std::path::Path, max_bytes: usize) -> String {
    match fs::read(path) {
        Ok(bytes) => {
            let start = bytes.len().saturating_sub(max_bytes);
            String::from_utf8_lossy(&bytes[start..]).to_string()
        }
        Err(_) => "(无)".to_string(),
    }
}

// 一键导出诊断报告（已脱敏）：系统信息 + 网关状态 + 已装插件 + 配置 + 日志，写到下载目录。
#[tauri::command]
async fn export_diagnostics() -> Result<String, String> {
    let state_dir = gateway_state_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut s = String::new();
    s.push_str("# ClawBuddy 诊断报告\n\n");
    s.push_str(&format!("- 生成(epoch秒): {ts}\n"));
    s.push_str(&format!("- 平台: {}/{}\n", std::env::consts::OS, std::env::consts::ARCH));
    s.push_str(&format!("- 状态目录: {}\n", state_dir.display()));
    s.push_str(&format!("- StepFun Key 已配置: {}\n", read_stepfun_key().is_some()));

    let script = bundled_script_path();
    s.push_str(&format!("- openclaw 脚本: {}\n", script.display()));
    if let Some(pkg) = script.parent().map(|p| p.join("package.json")) {
        if let Ok(raw) = fs::read_to_string(&pkg) {
            if let Ok(j) = serde_json::from_str::<serde_json::Value>(&raw) {
                s.push_str(&format!(
                    "- openclaw 版本: {}\n",
                    j.get("version").and_then(|v| v.as_str()).unwrap_or("?")
                ));
            }
        }
    }
    if let Ok(node) = get_node_path() {
        if let Ok(o) = Command::new(&node).arg("--version").output() {
            s.push_str(&format!("- node: {}\n", String::from_utf8_lossy(&o.stdout).trim()));
        }
    }

    // 网关健康
    let url = format!("http://127.0.0.1:{GATEWAY_PORT}/health");
    let health = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .build()
    {
        Ok(c) => match c.get(&url).send().await {
            Ok(r) => {
                let code = r.status().as_u16();
                format!("{code} {}", r.text().await.unwrap_or_default())
            }
            Err(e) => format!("连接失败: {e}"),
        },
        Err(e) => format!("client err: {e}"),
    };
    s.push_str(&format!("- 网关 /health: {health}\n\n"));

    s.push_str("## 已安装插件\n");
    if let Ok(entries) = fs::read_dir(state_dir.join("npm").join("projects")) {
        for e in entries.flatten() {
            s.push_str(&format!("- npm/projects: {}\n", e.file_name().to_string_lossy()));
        }
    }
    if let Ok(entries) = fs::read_dir(state_dir.join("extensions")) {
        for e in entries.flatten() {
            s.push_str(&format!("- extensions: {}\n", e.file_name().to_string_lossy()));
        }
    }
    s.push('\n');

    s.push_str("## openclaw.json（已脱敏）\n```json\n");
    match fs::read_to_string(state_dir.join("openclaw.json")) {
        Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(mut j) => {
                redact_secrets(&mut j);
                s.push_str(&serde_json::to_string_pretty(&j).unwrap_or_default());
            }
            Err(_) => s.push_str("(解析失败)"),
        },
        Err(_) => s.push_str("(无)"),
    }
    s.push_str("\n```\n\n");

    s.push_str("## gateway.log（末尾 60KB）\n```\n");
    s.push_str(&tail_file(&state_dir.join("logs").join("gateway.log"), 60_000));
    s.push_str("\n```\n\n");
    s.push_str("## config-health.json\n```json\n");
    s.push_str(&tail_file(&state_dir.join("logs").join("config-health.json"), 8_000));
    s.push_str("\n```\n\n");
    s.push_str("## config-audit.jsonl（末尾 16KB）\n```\n");
    s.push_str(&tail_file(&state_dir.join("logs").join("config-audit.jsonl"), 16_000));
    s.push_str("\n```\n");

    let home = std::env::var("HOME").map_err(|_| "无法定位用户目录".to_string())?;
    let out_path = std::path::Path::new(&home)
        .join("Downloads")
        .join(format!("clawbuddy-diagnostics-{ts}.txt"));
    fs::write(&out_path, s).map_err(|e| format!("写入失败: {e}"))?;
    Ok(out_path.to_string_lossy().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(GatewayManager::default())
        .manage(WeChatFerryManager::default())
        .manage(SharedFeishuState::new(Default::default()))
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            toggle_window,
            check_gateway_ready,
            get_node_path,
            get_gateway_status,
            start_gateway,
            get_stepfun_key_status,
            set_stepfun_key,
            get_stepfun_account,
            save_model_provider,
            set_active_model,
            get_model_config,
            wechat_login_start,
            feishu_login_start,
            pairing_list,
            pairing_approve,
            upgrade_openclaw,
            check_openclaw_update,
            get_claw_config,
            set_claw_config,
            export_diagnostics,
            start_wechat_login,
            get_wechat_status,
            send_wechat_message,
            receive_wechat_messages,
            create_feishu_bot,
            send_feishu_text,
            get_feishu_health,
            oauth_exchange
        ])
        .setup(|app| {
            let manager = app.state::<GatewayManager>().inner().clone();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(watch_process(app_handle, manager));

            let feishu_state = app.state::<SharedFeishuState>().inner().clone();
            let feishu_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let _ = feishu::start_http_server(feishu_app, feishu_state, feishu::build_feishu_router).await;
            });

            let show = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
            let hide = MenuItem::with_id(app, "hide", "隐藏窗口", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &hide, &quit])?;

            TrayIconBuilder::new()
                .tooltip("ClawBuddy")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(move |app: &AppHandle, event: MenuEvent| {
                    match event.id().as_ref() {
                        "show" => {
                            let _ = toggle_window(app.clone());
                        }
                        "hide" => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.hide();
                            }
                        }
                        "quit" => {
                            let _ = app.exit(0);
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
