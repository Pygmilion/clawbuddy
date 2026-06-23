# ClawBuddy 代码审查问题清单

## 审查范围
- src/App.tsx
- src/App.css
- src/hooks/useChat.ts
- src/pages/*.tsx
- src-tauri/src/lib.rs
- src-tauri/src/wechatferry/mod.rs
- src-tauri/src/feishu/http_server.rs

---

## P0 - 严重问题

### 1. ~~Stage Header 缺少 WeChat 分支~~ → 已修复
- **文件**: `src/App.tsx`
- **行号**: 413
- **核实结果**: 三元表达式已包含 `activeView === 'wechat' ? '💬 微信桥接'` 分支
- **结论**: 问题已修复，无需进一步操作。

---

## P1 - 中等问题

### 2. Rust 未使用的字段警告
- **文件**: `src-tauri/src/feishu/http_server.rs`
- **行号**: 95-97
- **问题**: `FeishuMessage` 结构体的 `parent_id`、`create_time`、`message_type` 字段从未被读取
- **影响**: 编译警告，代码冗余
- **修复方案**: 移除未使用的字段或在实际逻辑中使用它们

### 3. Rust 未使用的结构体
- **文件**: `src-tauri/src/feishu/http_server.rs`
- **行号**: 383
- **问题**: `TokenResponse` 结构体从未被构造
- **影响**: 编译警告
- **修复方案**: 删除未使用的结构体或在需要时使用它

### 4. SC 上次任务未完成
- **问题**: 之前的 SC 任务要求生成 `docs/issues.md`，但只输出了分析过程，没有实际生成文件
- **影响**: 问题清单缺失，无法追踪修复进度
- **修复方案**: 已手动创建 issues.md

---

## P2 - 轻微问题

### 5. ~~重复的 fetchFeishuBridgeHealth 函数~~ → 误报，已核实
- **文件**: `src/App.tsx`
- **行号**: 16, 330
- **核实结果**: `fetchFeishuBridgeHealth` 仅在文件顶部定义一次（第 16 行），第 330 行的 `handleFeishuRetry` 只是**调用**该函数，并非重复定义。
- **结论**: 非问题，无需修复。

### 6. ~~缺少 WeChatStatus.Failed 处理~~ → 误报，已核实
- **文件**: `src/App.tsx`
- **行号**: 122-124
- **核实结果**: `WeChatIndicator` 组件**已包含** `WeChatStatus.Failed` 状态处理（第 122-124 行），显示"微信接入失败"。
- **结论**: 非问题，无需修复。

---

## 编译状态
- TypeScript: ✅ 通过（tsc --noEmit）
- Rust: ⚠️ 通过但有 19 个警告（均为功能预留代码，非错误）
- 前端构建: ✅ 通过（npm run build）

## Gateway 状态
- 配置文件: ✅ 存在（~/.config/openclaw/openclaw.json）
- 端口监听: ✅ 18789 端口正在监听（node 进程 PID 2309）

