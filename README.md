# ClawBuddy 🦞

> 你的本地 AI 伙伴 —— 一个用 Tauri + React 封装 [openclaw](https://www.npmjs.com/package/openclaw) 网关的桌面应用：在一处与 Claw 对话，并把同一个智能体接入微信 / 飞书。

## ⬇️ 下载安装（macOS · Apple Silicon）

**[→ 前往 Releases 下载最新版 .dmg](https://github.com/Pygmilion/clawbuddy/releases/latest)**

1. 下载 `ClawBuddy_0.1.0_aarch64.dmg`，打开后把 **ClawBuddy** 拖入「应用程序」。
2. 首次打开若提示「无法验证开发者 / 已损坏」（安装包未做苹果公证），按任一方式放行：
   - 右键点击 App → **打开** → 弹窗里再点「打开」；或
   - 终端执行：`xattr -dr com.apple.quarantine /Applications/ClawBuddy.app`
3. 启动后在主界面填入阶跃星辰 StepFun API Key 即可开始对话。

## ✨ 功能

- 💬 与本地 Claw 对话（默认接入 StepFun，可一键切换模型）
- 🔀 输入框内一键切换主模型：内置 StepFun 3.5 / 3.7，也可添加任意 OpenAI 兼容 API
- 📱 渠道绑定：微信、飞书扫码接入，消息进出同一个智能体
- 🧠 角色 / 记忆文件：可视化编辑 SOUL.md / USER.md / IDENTITY.md 等（已内置默认内容）
- 🗂 历史会话、附件拖拽、深色模式、StepFun 余额查询
- 🔑 首次启动主界面引导填入 Key
- ⬆️ 应用内检查并升级 openclaw

## 🛠 本地开发

环境要求：Node.js ≥ 22.19、Rust 工具链、macOS。

```bash
npm install
npm run tauri dev      # 启动开发版（自带网关）
```

## 📦 打包与发布

```bash
# 1. 构建 .app（构建前会执行 scripts/bundle-gateway.sh 准备随包的 node + 网关运行时）
npm run tauri build -- --bundles app

# 2. 由 .app 生成 .dmg（headless 环境下 Tauri 自带的 dmg 步骤会失败，故手动生成）
APP="src-tauri/target/release/bundle/macos/ClawBuddy.app"
OUT="src-tauri/target/release/bundle/dmg/ClawBuddy_0.1.0_aarch64.dmg"
STAGE=$(mktemp -d); cp -R "$APP" "$STAGE/"; ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "ClawBuddy" -srcfolder "$STAGE" -ov -format UDZO "$OUT"; rm -rf "$STAGE"

# 3. 发布到 GitHub Release（需先 gh auth login）
scripts/publish-release.sh
```

> `bundled/`、`src-tauri/bundled/` 等大体积运行时目录不纳入版本库，会在构建时生成（需要先把一份与目标架构一致的 Node.js 放到 `bundled/bin/node`）。

## ℹ️ 说明

- 网关状态独立存放于 `~/.clawbuddy/state`，不影响系统已有的 `~/.openclaw`。
- 网关默认监听本地 `127.0.0.1:18789`，仅 loopback 绑定。
- 当前发布版本未签名 / 未公证，仅面向 Apple Silicon（arm64）。
