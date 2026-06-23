# Feishu Bot Integration

> 目标：在 ClawBuddy 现有架构基础上，新增“飞书机器人”接入层，让用户可以通过飞书群聊/单聊触发 OpenClaw Gateway，并接收统一格式的回复。
>
> 本文基于现有前端网关封装和 Tauri 本地 Gateway 启动能力设计，核心参考资料包括：
> - 现有代码：`src/appGateway.ts`
> - 本地 Tauri 网关管理：`src-tauri/src/lib.rs`
> - 飞书开放平台公开页面：发送消息 / 机器人能力 / 创建应用流程 / 应用可用范围 / Token 选择 / Markdown 消息内容（以 `open.feishu.cn` 公开文档为准）

## 1. 背景与目标

当前 ClawBuddy 已经具备两类能力：
- 前端侧可以通过 `createGatewayClient()` 直接访问 Gateway
- 本地 Tauri 侧可以拉起并监控 `openclaw-gateway` 进程，端口为 `18789`

本方案不改变现有桌面端核心链路，而是在其外层增加一个 FeishuBotService，负责把飞书事件转化为 Gateway 请求，再把结果回写到飞书会话。

## 2. 创建 Bot 的步骤

### 2.1 前置条件
- 拥有飞书企业账号，或创建新的测试企业
- 具备开发者后台访问权限
- 发布流程所需的应用所有者或管理员权限

### 2.2 创建企业自建应用
1. 进入 [开发者后台](https://open.feishu.cn/app)
2. 创建企业自建应用，记录 `App ID` 和 `App Secret`
3. 在“应用能力”中添加“机器人”

### 2.3 最小能力配置
- 应用能力：机器人
- 基础信息：应用名称、描述、图标
- 版本管理：开发完成后发布并上线

### 2.4 常用配置清单
| 配置项 | 推荐做法 |
| --- | --- |
| 能力 | 仅开启机器人能力 |
| 可用范围 | 测试阶段可使用“部分成员”，上线前按部门或成员配置 |
| 发布 | 修改配置后必须发布版本，机器人能力才会生效 |
| 可发现性 | 在飞书客户端“工作台”或“群设置 > 群机器人”中可被搜索 |

## 3. OAuth 2.0 授权流程

### 3.1 Token 选型
- 机器人发消息、回消息等应用层操作优先使用 `tenant_access_token`
- 若后续要读取用户私有资源，再考虑 `user_access_token`

### 3.2 自建应用获取 tenant_access_token
- 接口：`POST /open-apis/auth/v3/tenant_access_token/internal`
- 请求头：`Content-Type: application/json; charset=utf-8`
- 请求体：
```json
{
  "app_id": "<App ID>",
  "app_secret": "<App Secret>"
}
```
- 响应：
```json
{
  "code": 0,
  "msg": "ok",
  "tenant_access_token": "<token>",
  "expire": 7200
}
```

### 3.3 客户端授权流程（如需 user_access_token）
- 若后续需要用户登录态，推荐采用“自建应用 OAuth 授权码流程”
- 使用飞书提供的授权地址，带上 `app_id`、`redirect_uri`、`state`
- 用户同意后跳转回 redirect_uri，换取 `code`
- 再用 `code` 换取 `user_access_token`

### 3.4 鉴权建议
- `tenant_access_token` 只在服务端存储
- 建议实现“自动续签”：有效期小于 30 分钟时重新申请
- 在 Tauri 侧通过环境变量或密钥链注入，避免把 App Secret 随前端资源打包

## 4. Webhook 接收消息

### 4.1 两种接收方式
- 长连接：适合本地开发、单实例服务
- HTTP 回调：适合部署到公网服务器

### 4.2 HTTP 回调流程
1. 开发者后台“事件与回调”选择“将事件发送至开发者服务器”
2. 输入公网可访问的 URL，必须是 IPv4 公网地址
3. 配置 `Verification Token`，必要时配置 `Encrypt Key`

### 4.3 请求校验
- 飞书会先发送 `type: url_verification` 的 POST 请求
- 服务端需在 1 秒内原样返回 `challenge`
- 如果开启加密，需先解密再返回 `challenge`

### 4.4 核心事件
- 文本消息：`im.message.receive_v1`
- 卡片交互：卡片回调事件

### 4.5 Webhook 签名与幂等
- 建议服务端校验 `Verification Token`
- 按事件唯一标识做幂等处理，防止重复消费
- 建议保留原始事件日志，便于后续排障

## 5. 消息发送 API

### 5.1 接口概要
- 地址：`POST https://open.feishu.cn/open-apis/im/v1/messages`
- 请求头：
```text
Authorization: Bearer <tenant_access_token>
Content-Type: application/json; charset=utf-8
```
- 查询参数：`receive_id_type=open_id|user_id|union_id|email|chat_id`

### 5.2 文本消息示例
```json
{
  "receive_id": "oc_xxx",
  "msg_type": "text",
  "content": "{\"text\":\"hello\"}"
}
```

### 5.3 富文本消息示例
- 适合返回结构化的答复结果
- 支持 Markdown、超链接、样式、图片
```json
{
  "receive_id": "oc_xxx",
  "msg_type": "post",
  "content": "{\"zh_cn\":{\"title\":\"结果\",\"content\":[[{\"tag\":\"md\",\"text\":\"**结果**\\n1. 条目 A\\n2. 条目 B\"}]]}}"
}
```

### 5.4 卡片消息示例
- 适合展示更丰富的操作入口和状态展示
- 发送消息示例：
```json
{
  "receive_id": "oc_xxx",
  "msg_type": "interactive",
  "content": "{\"header\":{\"template\":\"blue\",\"title\":{\"tag\":\"plain_text\",\"content\":\"查询结果\"}},\"elements\":[{\"tag\":\"markdown\",\"content\":\"任务执行完成\"}]}"
}
```

### 5.5 消息发送限制
- 向同一用户发消息限频约为 5 QPS
- 向同一群发消息限频为群内机器人共享 5 QPS
- 文本消息最大约 150 KB；卡片/富文本消息最大约 30 KB
- 若使用 `template_id`，实际大小还包括模板数据大小

## 6. 群聊 @提及支持

### 6.1 文本消息 @用户
- 使用 `<at user_id="open_id">名称</at>` 语法
- 示例：
```json
{
  "receive_id": "oc_xxx",
  "msg_type": "text",
  "content": "{\"text\":\"<at user_id=\\\"ou_xxx\\\">Tom</at> 请确认\"}"
}
```

### 6.2 @所有人
- 文本消息中可使用 `<at user_id="all"></at>`
- 所在群必须开启“@所有人”权限

### 6.3 富文本 @用户
- 使用 `tag: at`：
```json
{
  "tag": "at",
  "user_id": "ou_xxx",
  "style": ["bold"]
}
```

### 6.4 消息结构中的 mention 信息
- 发送成功后返回体包含 `mentions`
- 可得到 `key`、`id`、`id_type`、`name`、`tenant_key`
- 便于系统后续还原“谁被提到”的上下文

### 6.5 群聊添加机器人
- 在群设置 > 群机器人中搜索应用机器人并添加
- 机器人加入群后才能被 @ 并接收群消息

## 7. 与 OpenClaw Gateway 的对接方案

### 7.1 总体架构

```text
飞书客户端
   -> 飞书开放平台事件推送
   -> FeishuBotController (HTTP callback)
   -> EventNormalizer (统一事件格式)
   -> GatewayRequestBuilder
   -> OpenClawGatewayClient -> http://127.0.0.1:18789/v1/chat/completions
   -> GatewayResponseParser
   -> FeishuReplyService -> 发送文本/富文本/卡片消息
```

### 7.2 现有代码复用
- `src/appGateway.ts` 已提供：
  - `buildGatewayHeaders()`
  - `createGatewayClient()`
  - `defineGatewayClient()`
  - `publishGatewayClientUpdate()`
- 这些函数说明当前前端已经是“统一网关调用层”风格
- 新增 Feishu 接入层时，应复用相同语义：

| 概念 | 当前前端 | FeishuBotService |
| --- | --- | --- |
| 用户配置来源 | `gateway:client-update` 事件 | 从 Feishu 消息上下文或用户绑定配置读取 |
| 鉴权头 | `Authorization: Bearer ...` | 由 FeishuBotController 根据 provider 决定 |
| 请求体 | `provider/model/messages/stream` | 由 Feishu 消息映射为 messages |
| 流式响应 | `onChunk` | 先缓存片段，最后汇总成一条 Feishu 消息；复杂场景可改为卡片+更新消息 |

### 7.3 FeishuBotController 职责
- 提供 `/webhook/feishu` 接收飞书事件
- 完成：
  - 验签
  - challenge 响应
  - 事件解密（如开启加密）
  - 业务分发

### 7.4 EventNormalizer 职责
- 输出统一结构：
```ts
interface UnifiedMessage {
  source: 'feishu';
  chatId: string;
  senderOpenId: string;
  senderUserId?: string;
  messageId: string;
  rootMessageId?: string;
  text: string;
  mentions: Array<{ openId: string; name?: string }>;
  raw: any;
  timestamp: number;
}
```

### 7.5 GatewayRequestBuilder 职责
- 将 `UnifiedMessage.text` 和 `mentions` 映射为 messages：
```ts
const messages = [
  {
    role: 'user',
    content: `[Feishu chat=${chatId} user=${senderOpenId}] ${text}`
  }
];
```

### 7.6 FeishuReplyService 职责
- 负责发送回复消息
- 最小可用：文本消息
- 建议版本：富文本/卡片消息
- 发送后统一返回：
```ts
interface FeishuReplyResult {
  messageId: string;
  chatId: string;
  status: 'sent' | 'failed';
}
```

### 7.7 群聊 @提及实现
1. 解析入站消息中的 `mentions`
2. 若消息里有人 @机器人：
   - 文本消息通过响应体中 `mentions` 还原
3. 发送回复时：
   - 文本消息：在 `content` 中使用 `<at user_id="ou_xxx">名称</at>`
   - 富文本：使用 `tag: at` 节点
4. 可选增强：
   - 在机器人回复中附带“原始提问人”的 mention，方便溯源

### 7.8 本地开发方案
- 使用 [frp / ngrok / Cloudflare Tunnel] 暴露本地 HTTP 服务
- 飞书事件回调地址配置为公网 URL
- 本地 FeishuBotController 再转发到桌面端或本地 Gateway 服务

### 7.9 与现有桌面端 Tauri 启动流程的关系
当前项目已在 `src-tauri/src/lib.rs` 内实现：
- `GatewayManager`
- 检测 `127.0.0.1:18789`
- 拉起 `src-tauri/runtime/openclaw-gateway`
- 轮询健康检查

因此 FeishuBotService 可直接复用同一 Gateway 端口，不需要新建模型服务。

### 7.10 推荐目录结构
```text
src/
  services/
    feishu/
      feishu.controller.ts
      feishu.event.ts
      feishu.normalizer.ts
      feishu.reply.ts
      feishu.types.ts
```

### 7.11 运行流程示例
1. 用户在群聊 @机器人并提问
2. 飞书推送 `im.message.receive_v1`
3. FeishuBotController 接收并验签
4. EventNormalizer 提取：
   - `chatId`
   - `senderOpenId`
   - `text`
   - `mentions`
5. GatewayRequestBuilder 构造 messages
6. GatewayClient 调 `/v1/chat/completions`
7. 根据返回结果，FeishuReplyService 发送：
   - 纯文本
   - 或卡片消息
8. 发送失败时记录错误并避免重复重发

## 8. 上线清单

| 项目 | 说明 |
| --- | --- |
| App ID / App Secret | 记录到 Tauri 配置，不要提交到前端代码库 |
| 机器人能力 | 已开启并发布上线 |
| 权限 | 至少申请“以应用的身份发消息(im:message:send_as_bot)” |
| 可用范围 | 配置到测试成员或部门 |
| 事件订阅 | 配置 `Verification Token`，选择 HTTP 订阅或长连接 |
| 回调地址 | 必须是 IPv4 公网地址 |
| 日志与监控 | 保留 webhook 请求日志、OpenAPI 日志 |
| 限频处理 | 实现本地消息发送队列，避免单点触发限频 |

## 9. 参考页面

以下资料来自飞书开放平台公开文档页面：
- 发送消息：`https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/message/create`
- 消息内容格式：`https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/im-v1/message/create_json`
- 自建应用获取 tenant_access_token：`https://open.feishu.cn/document/ukTMukTMukTM/ukDNz4SO0MjL5QzM/auth-v3/auth/tenant_access_token_internal`
- 如何选择 Token：`https://open.feishu.cn/document/uAjLw4CM/ugTN1YjL4UTN24CO1UjN/trouble-shooting/how-to-choose-which-type-of-token-to-use`
- 事件订阅配置：`https://open.feishu.cn/document/home/introduction-to-scope-and-authorization/availability`
- 三分钟快速开发机器人：`https://open.feishu.cn/document/uAjLw4CM/uMzNwEjLzcDMx4yM3ATM/develop-an-echo-bot/introduction`
