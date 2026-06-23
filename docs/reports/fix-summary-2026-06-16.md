# Fix Summary

## 修复文件清单

- `src/hooks/useChat.ts`
- `src-tauri/src/lib.rs`

## 修复点说明

- 在 `src/hooks/useChat.ts` 中补齐前端 WebSocket 重连、状态恢复和超时兜底，避免连接断开后请求永久挂起。
- 在 `src-tauri/src/lib.rs` 中补齐 Tauri 命令与 Gateway 状态轮询，确保前端能同步启动结果与异常态。
- 在 `src-tauri/src/lib.rs` 中补齐进程守护与自动重启，保证 Gateway 异常退出后可恢复。
- 在 `src-tauri/src/lib.rs` 中修正启动命令为 `gateway run`，防止 Gateway 启动失败。

## 验证结果

- 执行 `npm run build` 已通过。
