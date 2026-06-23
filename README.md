# ClawBuddy 🦞

你的本地 AI 伙伴：一个用 Tauri + React 封装 [openclaw](https://www.npmjs.com/package/openclaw) 网关的桌面应用，支持在一处与 Claw 对话，并把同一个智能体接入微信 / 飞书。

## 功能

- 💬 与本地 Claw 对话（默认接入阶跃星辰 StepFun，可一键切换模型）
- 🔌 多模型：内置 StepFun 3.5 / 3.7，也可添加任意 OpenAI 兼容 API
- 📱 渠道绑定：微信、飞书扫码接入，消息进出同一个智能体
- 🧠 角色 / 记忆文件：可视化编辑 SOUL.md / USER.md / IDENTITY.md 等
- 🗂 历史会话、附件拖拽、深色模式、StepFun 余额查询
- ⬆️ 应用内检查并升级 openclaw

## 下载安装

前往 [Releases](../../releases) 下载最新的 macOS 安装包（`.dmg`），打开拖入「应用程序」即可。首次启动后在主界面填入 StepFun API Key 即可开始对话。

## 本地开发

环境要求：Node.js ≥ 22.19、Rust 工具链、macOS。

```bash
npm install
npm run tauri dev      # 启动开发版（自带网关）
npm run tauri build    # 打包生成安装文件
```

构建前会执行 `scripts/bundle-gateway.sh` 准备随包的网关运行时；`bundled/`、`src-tauri/bundled/` 等大体积运行时目录不纳入版本库，会在构建时生成。

## 说明

- 网关状态独立存放于 `~/.clawbuddy/state`，不影响系统已有的 `~/.openclaw`。
- 网关默认监听本地 `127.0.0.1:18789`，仅 loopback 绑定。
