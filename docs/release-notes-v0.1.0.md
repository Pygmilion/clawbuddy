# ClawBuddy v0.1.0

首个公开版本 🦞 —— 运行在本机的 AI 伙伴，封装 openclaw 网关。

## 主要功能
- 💬 本地与 Claw 对话，默认接入阶跃星辰 StepFun
- 🔀 输入框内一键切换主模型（StepFun 3.5 / 3.7，及自定义 OpenAI 兼容 API）
- 📱 微信、飞书扫码绑定，消息进出同一个智能体
- 🧠 可视化编辑角色/记忆文件（SOUL.md / USER.md / IDENTITY.md 等，已内置默认内容）
- 🗂 历史会话、附件拖拽、深色模式、StepFun 余额查询
- 🔑 首次启动主界面引导填入 Key
- ⬆️ 应用内检查并升级 openclaw

## 安装（macOS · Apple Silicon）
1. 下载下方的 `ClawBuddy_0.1.0_aarch64.dmg`。
2. 打开 dmg，把 **ClawBuddy** 拖入「应用程序」。
3. 首次打开若提示「无法验证开发者 / 已损坏」，这是因为安装包未做苹果公证，按下述任一方式放行：
   - 右键点击 App → **打开** → 在弹窗里再点「打开」；或
   - 终端执行：`xattr -dr com.apple.quarantine /Applications/ClawBuddy.app`
4. 启动后在主界面填入 StepFun API Key 即可开始对话。

> 说明：本版本为 Apple Silicon（arm64）构建，未签名/未公证；网关状态独立存放于 `~/.clawbuddy/state`，不影响系统已有的 `~/.openclaw`。
