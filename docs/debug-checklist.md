# ClawBuddy Debug Checklist

## P0 - Gateway 启动失败
- 问题：`~/.config/openclaw/` 不存在，Gateway 拒绝启动
- 需要：创建最小配置或找到无配置启动方式
- 验证：`curl http://localhost:18789/` 有响应

## P0 - 前端白板
- 问题：Tauri 窗口打开但内容是白的
- 可能原因：JS 报错、WebSocket 连接失败、CSS 问题
- 需要：检查浏览器 DevTools 控制台错误

## P0 - 消息收发不通
- 问题：发消息没反应
- 需要：验证前端 → Gateway → 模型响应的完整链路

## P1 - 微信桥接未实现
- 只有骨架，没有实际代码

## P1 - 飞书桥接 UI 缺失
- Rust 端已实现，但前端没有 FeishuBridgePage.tsx
