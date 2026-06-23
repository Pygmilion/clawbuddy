// OpenClaw Gateway WebSocket 客户端。
// 协议要点（均经过对 openclaw v2026.6.6 gateway 的实测验证）：
//  - 帧格式：请求 {type:"req",id,method,params}；响应 {type:"res",id,ok,payload,error}；
//    事件 {type:"event",event,payload}
//  - 连接握手：gateway 先推 connect.challenge 事件，客户端再发 connect 请求（协议版本 4，
//    带 operator 作用域 + 客户端描述），收到 ok 的 connect 响应后才算就绪。
//  - 发消息：chat.send 参数 {sessionKey, message:<纯文本>, deliver:true, idempotencyKey}；
//    其响应只回 {runId,status:"started"}，真正的回复通过 event:"chat" 事件按 runId 推送，
//    文本在 payload.message.content[].text，payload.state 为 delta/final/error。
//  - 本地 loopback 来源（http://localhost、tauri://localhost）会被 gateway 自动放行，
//    浏览器/WebView 会自动带上 Origin 头，无需手动设置。

import { invoke } from '@tauri-apps/api/core';

const GATEWAY_WS_URL = 'ws://127.0.0.1:18789';

const PROTOCOL_VERSION = 4;
const REQUEST_SCOPES = ['operator.admin', 'operator.write', 'operator.read'];

export type SendChatOptions = {
  onChunk?: (chunk: string) => void;
  signal?: AbortSignal;
  provider?: string;
  model?: string;
  apiKey?: string;
  sessionKey?: string;
  attachments?: unknown[];
};

export interface ChatMessage {
  role: string;
  content: string;
}

type GatewayStatus = 'checking' | 'ready' | 'starting' | 'failed';

function statusName(status: number): GatewayStatus {
  if (status === 1) return 'ready';
  if (status === 2) return 'starting';
  if (status === 3) return 'failed';
  return 'checking';
}

function isTauriEnvironment(): boolean {
  try {
    return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
  } catch {
    return false;
  }
}

function delay(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(resolve, ms);
    signal?.addEventListener('abort', () => {
      clearTimeout(timer);
      reject(new Error('操作已取消'));
    });
  });
}

async function pollGatewayStatus(
  tauriReady: () => Promise<boolean>,
  tauriStatus: () => Promise<number>,
  signal?: AbortSignal,
): Promise<GatewayStatus> {
  let status: GatewayStatus = 'checking';

  for (let attempt = 0; attempt < 30; attempt += 1) {
    if (signal?.aborted) {
      throw new Error('操作已取消');
    }

    await delay(1000, signal);

    const ready = await tauriReady().catch(() => false);
    const stateStatus = await tauriStatus().catch(() => 0);

    const current = ready ? 'ready' : statusName(stateStatus);
    if (current === 'ready' || current === 'failed') {
      return current;
    }

    status = current;
  }

  return status;
}

async function checkGatewayReadyViaFetch(signal?: AbortSignal): Promise<GatewayStatus> {
  const timeout = 30_000;
  const start = Date.now();

  for (let attempt = 0; attempt < 30; attempt += 1) {
    if (signal?.aborted) {
      throw new Error('操作已取消');
    }

    try {
      const response = await fetch(`${GATEWAY_WS_URL.replace('ws://', 'http://')}/health`, {
        method: 'GET',
        headers: { accept: '*/*' },
        signal,
      });

      console.log('[gateway] health attempt', attempt + 1, response.status, response.statusText);
      if (response.ok) {
        return 'ready';
      }
    } catch (error) {
      console.warn('[gateway] health request failed', error);
      if (error instanceof Error && error.name === 'AbortError') {
        throw error;
      }
    }

    if (Date.now() - start >= timeout) {
      return 'failed';
    }

    await delay(1000, signal);
  }

  return 'failed';
}

export async function ensureGatewayReady(signal?: AbortSignal): Promise<GatewayStatus> {
  if (signal?.aborted) {
    throw new Error('操作已取消');
  }

  if (!isTauriEnvironment()) {
    console.warn('[gateway] 未检测到 Tauri 环境，尝试直接连接本地 Gateway。');
    return checkGatewayReadyViaFetch(signal);
  }

  await invoke<void>('start_gateway').catch((error) => {
    console.warn('[gateway] start_gateway invoke failed', error);
  });

  const tauriReady = () =>
    invoke<boolean>('check_gateway_ready').catch((error) => {
      console.warn('[gateway] check_gateway_ready failed', error);
      return false;
    });
  const tauriStatus = () =>
    invoke<number>('get_gateway_status').catch((error) => {
      console.warn('[gateway] get_gateway_status failed', error);
      return 0;
    });

  const status = await pollGatewayStatus(tauriReady, tauriStatus, signal);
  console.log('[gateway] ensureGatewayReady final status', status);
  if (status === 'failed') {
    throw new Error('Gateway 启动失败，请检查网关状态');
  }

  return status;
}

type GatewayEventCallback = (event: string, payload: unknown) => void;

interface GatewayFrame {
  type?: string;
  id?: string;
  event?: string;
  ok?: boolean;
  payload?: unknown;
  error?: { code?: string; message?: string };
}

function clientPlatform(): string {
  if (typeof navigator !== 'undefined' && navigator.platform) {
    return navigator.platform;
  }
  return 'unknown';
}

class GatewayClient {
  private ws: WebSocket | null = null;
  private url: string;
  private pendingRequests = new Map<
    string,
    { resolve: (value: unknown) => void; reject: (error: Error) => void }
  >();
  private eventCallbacks: GatewayEventCallback[] = [];
  private handshakeDone = false;
  private connectPromise: Promise<void> | null = null;
  private resolveHandshake: (() => void) | null = null;
  private rejectHandshake: ((error: Error) => void) | null = null;

  constructor(url: string) {
    this.url = url;
  }

  onEvent(callback: GatewayEventCallback) {
    this.eventCallbacks.push(callback);
    return () => {
      this.eventCallbacks = this.eventCallbacks.filter((cb) => cb !== callback);
    };
  }

  isConnected(): boolean {
    return this.handshakeDone && this.ws?.readyState === WebSocket.OPEN;
  }

  async connect(): Promise<void> {
    if (this.connectPromise) {
      return this.connectPromise;
    }

    this.connectPromise = new Promise<void>((resolve, reject) => {
      let settled = false;
      const fail = (error: Error) => {
        if (!settled) {
          settled = true;
          this.connectPromise = null;
          reject(error);
        }
      };

      try {
        console.log('[ws] connecting to', this.url);
        this.ws = new WebSocket(this.url);

        this.resolveHandshake = () => {
          if (!settled) {
            settled = true;
            resolve();
          }
        };
        this.rejectHandshake = fail;

        this.ws.onmessage = (event) => {
          let frame: GatewayFrame;
          try {
            frame = JSON.parse(event.data as string);
          } catch (error) {
            console.warn('[ws] parse frame failed', error);
            return;
          }

          // 握手：收到 challenge 后发送 connect 请求。
          if (frame.type === 'event' && frame.event === 'connect.challenge' && !this.handshakeDone) {
            this.sendConnect();
            return;
          }

          if (frame.type === 'res' && frame.id) {
            const pending = this.pendingRequests.get(frame.id);
            if (pending) {
              this.pendingRequests.delete(frame.id);
              if (frame.ok === false) {
                pending.reject(new Error(frame.error?.message ?? JSON.stringify(frame.error)));
              } else {
                pending.resolve(frame.payload ?? frame);
              }
            }
            return;
          }

          if (frame.type === 'event' && frame.event) {
            this.eventCallbacks.forEach((cb) => cb(frame.event as string, frame.payload));
          }
        };

        this.ws.onclose = (event) => {
          console.log('[ws] close', event.code, event.reason);
          this.handshakeDone = false;
          this.connectPromise = null;
          const closeError = new Error(event.reason || 'WebSocket 连接已关闭');
          this.pendingRequests.forEach(({ reject: rejectPending }) => rejectPending(closeError));
          this.pendingRequests.clear();
          fail(closeError);
        };

        this.ws.onerror = () => {
          fail(new Error('WebSocket 连接失败'));
        };
      } catch (error) {
        fail(error instanceof Error ? error : new Error(String(error)));
      }
    });

    return this.connectPromise;
  }

  private sendConnect() {
    const id = crypto.randomUUID();
    const frame = {
      type: 'req',
      id,
      method: 'connect',
      params: {
        minProtocol: PROTOCOL_VERSION,
        maxProtocol: PROTOCOL_VERSION,
        scopes: REQUEST_SCOPES,
        client: {
          id: 'openclaw-control-ui',
          version: '0.1.0',
          platform: clientPlatform(),
          mode: 'ui',
        },
      },
    };

    this.pendingRequests.set(id, {
      resolve: () => {
        this.handshakeDone = true;
        this.resolveHandshake?.();
      },
      reject: (error) => {
        this.rejectHandshake?.(error);
      },
    });
    this.ws?.send(JSON.stringify(frame));
  }

  async request(
    method: string,
    params: Record<string, unknown> = {},
    timeoutMs = 30000,
  ): Promise<unknown> {
    if (!this.isConnected()) {
      await this.connect();
    }

    const id = crypto.randomUUID();
    const frame = { type: 'req', id, method, params };

    return new Promise((resolve, reject) => {
      if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
        reject(new Error('WebSocket 未连接'));
        return;
      }

      this.pendingRequests.set(id, { resolve, reject });
      this.ws.send(JSON.stringify(frame));

      setTimeout(() => {
        if (this.pendingRequests.has(id)) {
          this.pendingRequests.delete(id);
          reject(new Error(`请求超时：${method}`));
        }
      }, timeoutMs);
    });
  }

  disconnect() {
    if (this.ws) {
      this.ws.onclose = null;
      this.ws.close(1000, 'client disconnect');
      this.ws = null;
    }
    this.handshakeDone = false;
    this.connectPromise = null;
  }
}

// 单例
let clientInstance: GatewayClient | null = null;
// 同一应用生命周期内复用一个 sessionKey，让 gateway 维护多轮对话上下文。
const appSessionKey = `ui-${crypto.randomUUID()}`;

async function getGatewayClient(signal?: AbortSignal): Promise<GatewayClient> {
  if (clientInstance?.isConnected()) {
    return clientInstance;
  }

  if (signal?.aborted) {
    throw new Error('操作已取消');
  }

  const gatewayStatus = await ensureGatewayReady(signal);
  if (gatewayStatus !== 'ready') {
    throw new Error('Gateway 未就绪');
  }

  if (!clientInstance) {
    clientInstance = new GatewayClient(GATEWAY_WS_URL);
  }

  await clientInstance.connect();
  return clientInstance;
}

function extractAssistantText(payload: Record<string, unknown>): string {
  const message = payload?.message as { content?: unknown } | undefined;
  const content = message?.content;
  if (Array.isArray(content)) {
    return content
      .map((part) => {
        if (part && typeof part === 'object' && 'text' in part) {
          const text = (part as { text?: unknown }).text;
          return typeof text === 'string' ? text : '';
        }
        return '';
      })
      .join('');
  }
  if (typeof (payload as { delta?: unknown }).delta === 'string') {
    return (payload as { delta: string }).delta;
  }
  if (typeof (payload as { text?: unknown }).text === 'string') {
    return (payload as { text: string }).text;
  }
  return '';
}

export async function sendChatMessage(
  messages: ChatMessage[],
  options: SendChatOptions = {},
): Promise<string> {
  const { onChunk, signal } = options;
  console.log('[sendChatMessage] start', { messages: messages.length, provider: options.provider, model: options.model });

  if (signal?.aborted) {
    throw new Error('操作已取消');
  }

  const client = await getGatewayClient(signal);
  const sessionKey = options.sessionKey ?? appSessionKey;

  // gateway 按 sessionKey 维护服务端会话历史，因此只需发送最新一条用户消息文本。
  const lastUser = [...messages].reverse().find((m) => m.role === 'user');
  const text =
    typeof lastUser?.content === 'string' ? lastUser.content : JSON.stringify(lastUser?.content ?? '');

  let emitted = '';
  let runId: string | null = null;

  return new Promise<string>((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup();
      reject(new Error('等待回复超时'));
    }, 120000);

    const onAbort = () => {
      cleanup();
      if (runId) {
        client.request('chat.abort', { sessionKey, runId }).catch(() => {});
      }
      reject(new Error('操作已取消'));
    };
    signal?.addEventListener('abort', onAbort);

    const unsubscribe = client.onEvent((event, rawPayload) => {
      if (event !== 'chat') {
        return;
      }
      const payload = (rawPayload ?? {}) as Record<string, unknown>;
      console.log('[sendChatMessage] chat event', { event, runId, state: payload.state });
      // 仅处理本次请求对应 run 的回复。
      if (runId && payload.runId && payload.runId !== runId) {
        return;
      }

      const state = payload.state as string | undefined;
      if (state === 'error') {
        cleanup();
        reject(new Error((payload.errorMessage as string) || '模型返回错误'));
        return;
      }

      const fullText = extractAssistantText(payload);
      if (fullText) {
        // chat 事件携带累积文本，按差量推送增量片段。
        const chunk = fullText.startsWith(emitted) ? fullText.slice(emitted.length) : fullText;
        if (chunk && onChunk) {
          onChunk(chunk);
        }
        emitted = fullText;
      }

      if (state === 'final') {
        cleanup();
        resolve(emitted);
      }
    });

    function cleanup() {
      console.log('[sendChatMessage] cleanup');
      clearTimeout(timeout);
      unsubscribe();
      signal?.removeEventListener('abort', onAbort);
    }

    // 发送 chat.send，拿到 runId 后开始匹配回复事件。
    console.log('[sendChatMessage] sending chat.send', { sessionKey, text: text.slice(0, 50) });
    client
      .request('chat.send', {
        sessionKey,
        message: text,
        deliver: true,
        idempotencyKey: crypto.randomUUID(),
        ...(options.attachments && options.attachments.length > 0
          ? { attachments: options.attachments }
          : {}),
      })
      .then((res) => {
        console.log('[sendChatMessage] chat.send response', res);
        const payload = (res ?? {}) as { runId?: string };
        if (payload.runId) {
          runId = payload.runId;
        }
      })
      .catch((error) => {
        cleanup();
        reject(error instanceof Error ? error : new Error(String(error)));
      });
  });
}

// 查询某渠道是否已在网关内运行（用于界面打开时回显"已连接"，避免总显示"待扫码"）。
export async function getChannelRunning(channelId: string): Promise<boolean> {
  try {
    const client = await getGatewayClient();
    const res = (await client.request('channels.status', {})) as {
      channelAccounts?: Record<string, Array<{ running?: boolean }>>;
      channels?: Record<string, { configured?: boolean }>;
    };
    const accounts = res?.channelAccounts?.[channelId];
    if (Array.isArray(accounts) && accounts.length > 0) {
      return accounts.some((account) => account?.running === true);
    }
    return res?.channels?.[channelId]?.configured === true;
  } catch {
    return false;
  }
}

// ===== 历史会话（openclaw 服务端 session）=====

export interface SessionSummary {
  key: string; // 完整 key，如 agent:dev:ui-xxx
  title: string;
  preview: string;
}

// 把完整 session key（agent:<id>:ui-xxx）转成 chat.send 用的 sessionKey（ui-xxx）。
export function toSendSessionKey(fullKey: string): string {
  if (fullKey.startsWith('agent:')) {
    const parts = fullKey.split(':');
    return parts.slice(2).join(':') || fullKey;
  }
  return fullKey;
}

export async function listSessions(): Promise<SessionSummary[]> {
  try {
    const client = await getGatewayClient();
    const res = (await client.request('sessions.list', {
      limit: 50,
      includeLastMessage: true,
      includeDerivedTitles: true,
    })) as { sessions?: Array<{ key: string; derivedTitle?: string; lastMessagePreview?: string }> };
    return (res.sessions || []).map((s) => ({
      key: s.key,
      title: (s.derivedTitle || '').trim() || toSendSessionKey(s.key),
      preview: (s.lastMessagePreview || '').replace(/\n/g, ' ').slice(0, 60),
    }));
  } catch {
    return [];
  }
}

function extractMessageText(content: unknown): string {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    return content
      .map((part) => {
        if (part && typeof part === 'object' && (part as { type?: string }).type === 'text') {
          const t = (part as { text?: unknown }).text;
          return typeof t === 'string' ? t : '';
        }
        return '';
      })
      .join('');
  }
  return '';
}

export async function loadSessionHistory(fullKey: string): Promise<ChatMessage[]> {
  const client = await getGatewayClient();
  const res = (await client.request('chat.history', { sessionKey: fullKey, limit: 200 })) as {
    messages?: Array<{ role: string; content: unknown }>;
  };
  const out: ChatMessage[] = [];
  for (const m of res.messages || []) {
    if (m.role !== 'user' && m.role !== 'assistant') continue; // 跳过 tool 消息
    const text = extractMessageText(m.content).trim();
    if (text) out.push({ role: m.role, content: text });
  }
  return out;
}

export async function deleteSession(fullKey: string): Promise<void> {
  const client = await getGatewayClient();
  await client.request('sessions.delete', { key: fullKey, deleteTranscript: true });
}

// ===== Agent 文件（SOUL.md / USER.md / IDENTITY.md 等）=====

export interface AgentFile {
  name: string;
  missing?: boolean;
}

export async function listAgentFiles(agentId = 'dev'): Promise<AgentFile[]> {
  try {
    const client = await getGatewayClient();
    const res = (await client.request('agents.files.list', { agentId })) as { files?: AgentFile[] };
    return res.files || [];
  } catch {
    return [];
  }
}

export async function getAgentFile(name: string, agentId = 'dev'): Promise<string> {
  const client = await getGatewayClient();
  const res = (await client.request('agents.files.get', { agentId, name })) as {
    content?: string;
    text?: string;
  };
  return res.content ?? res.text ?? '';
}

export async function setAgentFile(name: string, content: string, agentId = 'dev'): Promise<void> {
  const client = await getGatewayClient();
  await client.request('agents.files.set', { agentId, name, content });
}
