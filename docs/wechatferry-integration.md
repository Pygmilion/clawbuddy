# WeChatFerry 内嵌集成技术设计

本设计目标是让 ClawBuddy 作为现有 Gateway 自托管能力的上游，以最小侵入方式支持微信消息接入。
采用“外挂二进制 + 内部桥接”的架构，先把 wechatferry 作为可替换外置进程接入，后续可在 Rust 层逐步增强。
主线符合当前工程约定：本仓库只写代码和设计，外发提交流程由其他人负责。

## 1. wechatferry 二进制打包与内嵌方案

### 1.1 交付物定义
在仓库中新增运行时交付目录：

- `src-tauri/runtime/wechatferry/<platform>-<arch>/`
  - 例：`darwin-arm64`, `darwin-x64`, `windows-msvc-x64`, `linux-gnu-x64`
  - 每个平台目录放置平台专用可执行文件与必要动态库
- `src-tauri/runtime/manifest.json`
  - 记录版本、校验和、下载地址、目标平台
- 现有 `src-tauri/runtime/openclaw-gateway[.exe]` 继续保留，不作为微信接入依赖

### 1.2 内嵌策略：外用二进制，运行时释放
推荐优先采用“外部二进制”而不是 Rust 静态库重打包，原因：

- wechatferry 对外接口仍以独立进程和进程通信为主
- 微信客户端、Hook 逻辑、协议细节随微信版本变化快
- 二进制更新可独立发布，减少主包变动和分发风险

具体做法：

- Tauri build 时，使用 `src-tauri/build.rs` 在构建阶段下载对应平台 wechatferry 发布产物；
- 或在 GitHub release 不连通时，提供本地 fallback：允许开发者将对应平台二进制放置到 `src-tauri/runtime/wechatferry/<platform-arch>/`；
- 最终 App bundle 中将这些 runtime 资源打包进：
  - macOS：`AppName.app/Contents/Resources/app/src-tauri/runtime`
  - Windows：`resources/app/src-tauri/runtime`
  - Linux：`resources/app/src-tauri/runtime`

### 1.3 平台分发边界
按平台处理最小可运行集合：

- macOS arm64
  - 主进程：`wechatferry`
  - 动态依赖：若官方 release 未做全静态链接，则同时附带 `*.dylib` 和 `runtimes` 目录
  - 可能仍需保留对应安装目录上的微信原生安装，用于获取登录态和本地资源
- Windows x64
  - 主进程：`wechatferry.exe`
  - 动态依赖：`vcruntime140.dll`、`api-ms-win-*` 等，根据官方 release 说明补充
- Linux x64（未来可选）
  - 仅当官方提供静态构建时接入，否则引入系统库依赖过高，可暂缓

### 1.4 更新机制
- manifest 版本与 app 版本解耦
- 启动时检测 `runtime/manifest.json` 中的版本号；
- 若远端 release 版本更新，走 update worker 下载并替换；
- 替换时写入临时目录，校验通过后原子替换到运行时目录。

## 2. Rust 端启动与通信

### 2.1 进程启动模型
Rust 负责 wechatferry 生命周期管理：

- 主入口在 Rust 侧实现：`src-tauri/src/wechatferry.rs`
- 负责：
  - 选择平台目录和可执行文件路径
  - 生成或复用配置文件路径
  - 启动子进程
  - 维护 stdout/stderr/stdin 管道
  - 异常退出后按策略重试

配置路径规则：

- macOS：`appSupportDir()/wechatferry/<platform>/config.json`
- Windows：`appLocalDataDir()/wechatferry/<platform>/config.json`
- Linux：`appLocalDataDir()/wechatferry/<platform>/config.json`

最小配置内容：

- `platform`
- `executable_path`
- `log_dir`
- `qr_mode`
- `gateway.url`
- `gateway.secret`

### 2.2 通信协议选型：默认 stdin/stdout，失败降级 TCP
优先 stdin/stdout，次选 TCP loopback。设计理由：

- wechatferry 核心输出以文本事件为主，JSON lines 天然适合 stdin/stdout
- 不需要额外端口，减少防火墙和本地冲突
- 便于统一做日志、协议升级和命令注入

协议格式：

```text
<event>\n
```

事件结构：

```json
{
  "event": "message.private",
  "trace_id": "uuid",
  "ts": 1700000000000,
  "payload": {}
}
```

命令结构：

```text
{"cmd":"send","trace_id":"uuid","payload":{}}\n
```

响应结构：

```json
{"trace_id":"uuid","ok":true,"payload":{}}
```

失败格式：

```json
{"trace_id":"uuid","ok":false,"error":{"code":"...","message":"..."}}
```

### 2.3 Rust 侧实现
增加状态：

- `WeChatFerryManager`
  - 当前状态：`Idle`, `Starting`, `QrPending`, `Running`, `Stopping`, `Failed`
  - 子进程句柄
  - 读任务句柄
  - 写任务句柄
  - 事件发送器：转发到 OpenClaw Gateway
  - 重试配置

启动伪代码：

- 调用 `WeChatFerryManager::start(app_handle)`
- 执行：
  - 检查 `127.0.0.1:18790` 是否已被 Gateway 或 Bridge 占用；
  - 生成 config；
  - 启动 `wechatferry --config <path>`;
  - 读取 stdout，每行解析 JSON；
  - 将 `message.*` 事件写入本地 channel；
  - Bridge 任务消费 channel，并 POST 到 Gateway。

Bridge 策略：

- Bridge 直接复用当前 `GatewayManager` 的网络探测端口 `127.0.0.1:18789` 判断 Gateway 是否在线；
- 若 Bridge 先启动，使用本地 loopback 作为 Gateway 客户端接口；
- 若 Gateway 未就绪，事件进入 `wechatferry.pending` 目录，用于追投递。

### 2.4 降级 TCP 方案
当 stdio 异常时启用 TCP loopback：

- 端口固定为 `127.0.0.1:18790`
- 格式改为标准 `\n` 分隔 JSON
- Rust 端监听 `127.0.0.1:18790` 作为 server
- 适合事件流较大或子进程日志污染严重时使用

### 2.5 命令与状态暴露
新增 Tauri commands：

- `wechatferry_status` → 返回状态枚举
- `wechatferry_start` → 启动
- `wechatferry_stop` → 停止
- `wechatferry_retry_qr` → 重拉二维码
- `wechatferry_send_message` → 发送微信消息
- `gateway_send_wechat_event` → 前端透传测试命令

事件：

- `wechatferry-status-changed`
- `wechatferry-qr-needed`
- `wechatferry-login-success`
- `wechatferry-login-failed`
- `wechatferry-disconnected`

## 3. 二维码生成与展示方案

### 3.1 目标
在 `wechatferry` 进入扫码登录态时，将二维码安全展示给用户，并限制展示范围。

### 3.2 推荐方案：Rust 输出二维码图片 base64，前端在受限弹窗渲染
理由：

- 避免 webview 意外扫码跳转；
- 可控制复制、截屏、分享行为；
- 可基于二维码有效期自动刷新。

实现步骤：

- Rust 收到 `qr.url` 后，用本地库生成 PNG base64：
  - Rust 侧引入 `qrcode` 或 `resvg` 生成图片；
  - 最小版本下，可保留纯文本 URL 并用前端二维码组件兜底，但登录页仍需做安全提示。
- 前端新增页面：`src/pages/WeChatLoginPage.tsx`
- 页面只做两件事：
  - 展示二维码
  - 展示安全提示、操作按钮

### 3.3 前端展示规范
展示边界：

- 仅在登录确认流使用，不允许嵌入聊天主界面
- 不允许自动填充账号/密码
- 扫码成功后自动切回主界面或登录成功状态

二维码生命周期：

- 首次获取：`wechatferry-qr-needed`
- 超时前 30 秒：提示即将过期
- 过期/失效：`wechatferry-qr-needed` 再次触发刷新
- 登录成功：关闭登录页，保留“微信账号已绑定”状态

截图与复制控制：

- macOS：`CGWindowListCreateImage` 检测截图事件仅用于提示，不做主动阻止
- Windows：可选监听 `PrintScreen`
- 统一 UI 文案说明截图可能导致二维码失效

### 3.4 可选备选方案
- WebView 直接渲染二维码 URL：仅用于内部调试
- 服务端生成二维码：不推荐，因为会经过中转服务器，增加账号风控

## 4. 消息路由：微信消息 → wechatferry → OpenClaw Gateway

### 4.1 总路由图
```text
微信客户端/微信协议
        ↓
   wechatferry
        ↓ JSON line event stream
   Rust Bridge / Gateway Client
        ↓
   OpenClaw Gateway
        ↓ HTTP REST/SSE/WebSocket
   ClawBuddy UI
```

### 4.2 事件标准化
将 wechatferry 原始事件转换为内部统一消息模型：

```json
{
  "source": "wechat",
  "direction": "inbound",
  "message_type": "private|group|official",
  "from": {"wxid": "...", "alias": "..."},
  "to": {"wxid": "self"},
  "content": {"text": "...", "media": []},
  "raw": {"wx_event": "..."},
  "meta": {"trace_id": "uuid", "ts": 1700000000000}
}
```

映射规则：

- `message.private` → `private`
- `message.group` → `group`
- `message.temp` → 仅用于日志或暂不接入
- `message.official` → `official`

### 4.3 发送路径
前端发送请求：

```text
ClawBuddy UI
  → Tauri command: wechatferry_send_message
  → Rust Bridge 写入 wechatferry stdin
  → wechatferry 发送到微信
  → 成功后返回发送结果
  → 前端展示成功态
```

写入规则：

- 每条命令必须带 `trace_id`
- Rust 侧维护 pending commands map；
- 成功或失败都回传；
- 超时 10 秒视为失败，错误码 `timeout`

### 4.4 接收与投递
Bridge 消费循环：

```text
for event in wechatferry stdout:
  parse JSON
  enrich source=wechat
  forward to Gateway if direction=inbound
  if Gateway offline:
    persist to queue dir
```

Gateway 离线队列：

- 目录：`appSupportDir()/wechatferry/pending`
- 文件命名：`<trace_id>.json`
- 最大保留时间：24 小时
- Gateway 恢复后按时间顺序重放

### 4.5 并发与顺序保证
- 对同一会话保持发送 FIFO
- 对同一会话接收允许并发到达，但保留 `msg_id`
- 去重窗口：10 分钟内按 `msg_id` 去重

## 5. 封号风险提示 UI

### 5.1 触发时机
风险提示在以下节点显示：

- 用户启用微信接入时
- wechatferry 登录态异常时
- 检测到限流或登录验证升级提示时
- 用户首次进入微信相关功能页时

### 5.2 提示等级
分为三级：

- `notice`：一般提示
  - 示例：当前版本为第三方接入方式，使用请遵守服务协议
- `warning`：功能限制提示
  - 示例：本次登录可能需要二次验证，如频繁失败请暂停使用
- `critical`：高风险提示
  - 示例：当前账号已触发风控特征，建议立即停止自动发送并等待验证

### 5.3 UI 位置与样式
位置：

- 登录页顶部固定提示条
- 聊天页面右上角状态区
- 设置页微信接入条目下

样式：

- 使用高对比色区分等级
- critical 增加关闭按钮和跳转到帮助页
- 不阻断界面使用，但提供“暂停自动功能”按钮

### 5.4 帮助链接
内置帮助页面覆盖：

- 常见风控表现
- 临时缓解步骤
- 如何导出日志用于反馈
- 明确说明本功能并非官方接口

## 6. 与现有 Gateway 自托管的集成点

### 6.1 复用现有生命周期
ClawBuddy 已有 Gateway 自托管能力，微信接入与其集成原则：

- 不改变现有 `GatewayManager` 主状态机
- 新增 `WeChatFerryManager` 作为下游模块
- UI 上的 Gateway 状态栏继续独立展示

### 6.2 启动顺序
推荐顺序：

```text
App start
  → GatewayManager.start()
  → Gateway ready 检查
  → 用户开启微信接入
  → WeChatFerryManager.start()
```

依赖：

- wechatferry 可在 Gateway 未就绪时启动
- 但发送/接收消息需要 Gateway 在线
- 离线期间事件进入 pending queue

### 6.3 端口与资源隔离
- Gateway：`127.0.0.1:18789`
- WeChat Bridge：`127.0.0.1:18790`，仅用于 stdio 降级
- 两者目录隔离，日志目录独立
- 共享 `appSupportDir()` 根目录，但各自子目录隔离

### 6.4 配置集成
设置页新增区块：微信接入

- 平台检测
- 是否启用
- 绑定状态
- 日志路径
- 隐私开关：是否允许上传错误日志

存储：

- 继续沿用前端 localStorage 保存 UI 设置
- 敏感配置保存到 Rust 侧 app data，避免放在前端
- 所有 secrets 不进入 git

### 6.5 现有 UI 的集成点
目前需要改动的 UI 位置：

- `src/App.tsx`：增加微信接入状态指示
- `src/pages/SettingsPage.tsx`：增加微信接入设置
- `src/pages/WeChatLoginPage.tsx`：新增
- `src/hooks/useWeChatFerry.ts`：新增 hook

现有 Gateway 状态逻辑无需重写，只做事件追加。

### 6.6 向后兼容
- 未内嵌 wechatferry 时，App 仍可正常使用 Gateway
- 若用户未开启微信接入，二进制和 runtime 目录不参与启动
- 若 release 包缺失平台二进制，Rust 侧直接进入 `disabled` 状态
- 不因微信接入失败导致 Gateway 被重启或关闭

## 7. 目录与职责分工

```text
ClawBuddy
├── docs/
│   └── wechatferry-integration.md
├── src
│   ├── pages
│   │   ├── WeChatLoginPage.tsx
│   │   └── SettingsPage.tsx
│   ├── hooks
│   │   └── useWeChatFerry.ts
│   └── appGateway.ts
├── src-tauri
│   ├── runtime
│   │   ├── manifest.json
│   │   ├── openclaw-gateway[.exe]
│   │   └── wechatferry/<platform-arch>/
│   ├── build.rs
│   ├── src
│   │   ├── lib.rs
│   │   ├── main.rs
│   │   ├── gateway.rs
│   │   └── wechatferry.rs
│   └── Cargo.toml
└── README.md
```

职责：

- Frontend：消息展示、登录页、设置页、风险提示、用户交互
- Rust：进程管理、协议转换、事件投递、Gateway 状态协同
- Bridge：wechatferry ↔ Gateway 协议适配、排队重试

## 8. 风险与边界说明

### 8.1 法律与合规
- 接入前需确认目标账号已同意相关服务条款
- 不用于自动群发营销内容
- 日志仅保留脱敏后字段，不存储密码和会话完整内容

### 8.2 运行风险
- 微信升级可能导致 wechatferry 失效
- 封号风险由用户承担，App 侧仅提供提示
- 不承诺长期稳定可用

### 8.3 稳定性风险
- 二进制依赖网络下载，需要提供 fallback
- macOS 权限变化可能导致登录态不可用
- 建议提供“手动选择微信安装路径”作为 fallback

## 9. 发布与维护策略

- 第一版先支持 macOS arm64 + Windows x64
- 通过 manifest 版本机制逐步扩展平台
- 每次 wechatferry 升级，独立发 patch，不强制带 Gateway 升级
- 对外提交由指定负责人执行，核心实现与代码留在本仓库

## 10. 验收标准

- `src-tauri/runtime/wechatferry/<platform>` 能在对应平台正确打包进 App
- Rust 可成功启动 wechatferry，并在 stdout 中读到初始事件
- 二维码事件能触达前端登录页
- 登录成功后，微信消息可转化为统一内部事件并到达 Gateway
- Gateway 离线时事件可进入 pending queue，并可在恢复后重放
- 设置页可开关微信接入，且不影响现有 Gateway 使用
- 至少完成 macOS arm64 的本地联调
