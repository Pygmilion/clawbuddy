import React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { openUrl } from '@tauri-apps/plugin-opener';
import { Markdown, MessageCopy } from './Markdown';
import type { ChatMessage, SessionSummary, AgentBlock } from './hooks/useChat';
import {
  sendChatMessage,
  getChannelRunning,
  listSessions,
  loadSessionHistory,
  deleteSession,
  toSendSessionKey,
} from './hooks/useChat';
import { Composer, type Attachment } from './Composer';
import { KeyConfigCard } from './KeyConfigCard';
import { SettingsPage } from './pages/SettingsPage';
import { FeishuBridgePage } from './pages/FeishuBridge';
import { WeChatBridgePage } from './pages/WeChatBridge';
import './App.css';

type View = 'chat' | 'settings' | 'feishu' | 'wechat';
type Tone = 'green' | 'amber' | 'red' | 'gray';

export class ErrorBoundary extends React.Component<{ children: React.ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error) {
    console.error('[ClawBuddy] render error', error);
  }

  render() {
    if (this.state.error) {
      return (
        <div style={{ padding: 24, color: '#dc2626' }}>
          <h1>界面加载失败</h1>
          <pre>{this.state.error.message}</pre>
        </div>
      );
    }
    return this.props.children;
  }
}

// 状态圆点（绿=正常，红=未连接，琥珀=检测中）。loading=true 时显示环形加载指示灯。
function Dot({ tone, loading }: { tone: Tone; loading?: boolean }) {
  if (loading) {
    return <span className="status-ring" aria-label="加载中" title="正在加载…" />;
  }
  return <span className={`status-dot ${tone}`} />;
}

const WORKING_PHRASES = ['正在思考', '正在查资料', '正在动手', '正在整理'];

// Claw 工作中指示器：流式回复期间显示，让"不断尝试"的过程可见。
function WorkingIndicator({ compact }: { compact?: boolean }) {
  const [phase, setPhase] = useState(0);
  useEffect(() => {
    const t = window.setInterval(() => setPhase((p) => (p + 1) % WORKING_PHRASES.length), 2600);
    return () => window.clearInterval(t);
  }, []);
  return (
    <div className={`working-indicator ${compact ? 'compact' : ''}`}>
      <span className="work-spinner" aria-hidden />
      <span className="work-text">
        Claw {WORKING_PHRASES[phase]}
        <span className="work-dots"><i>.</i><i>.</i><i>.</i></span>
      </span>
    </div>
  );
}

// 工具名 → 友好标签 + 图标（对标 WorkBuddy 的「运行命令 / 收集资料」样式）。
function toolMeta(name: string): { label: string; icon: string } {
  const n = name.toLowerCase();
  if (n.includes('web_search') || n.includes('search')) return { label: '联网搜索', icon: '🔍' };
  if (n.includes('web_fetch') || n.includes('fetch') || n.includes('browse')) return { label: '抓取网页', icon: '🌐' };
  if (n.includes('exec') || n.includes('command') || n.includes('shell') || n.includes('bash')) return { label: '运行命令', icon: '⌘' };
  if (n.includes('file') || n.includes('read') || n.includes('write') || n.includes('edit')) return { label: '读写文件', icon: '📄' };
  if (n.includes('memory')) return { label: '记忆', icon: '🧠' };
  return { label: name || '操作', icon: '🔧' };
}

// 工具操作卡片：默认折叠，点标题展开查看命令/输出。运行中转圈，完成显示 ✓/✗。
function ToolCard({ block }: { block: Extract<AgentBlock, { kind: 'tool' }> }) {
  const [open, setOpen] = useState(false);
  const { label, icon } = toolMeta(block.name);
  const detail = block.output || block.error;
  return (
    <div className={`tool-card ${block.status}`}>
      <button type="button" className="tool-head" onClick={() => setOpen((o) => !o)} disabled={!detail}>
        <span className="tool-icon">{icon}</span>
        <span className="tool-label">{label}</span>
        <span className="tool-title" title={block.title}>{block.title}</span>
        <span className="tool-status">
          {block.status === 'running' ? (
            <span className="work-spinner" aria-hidden />
          ) : block.status === 'failed' ? (
            <span className="tool-x">✗</span>
          ) : (
            <span className="tool-ok">✓</span>
          )}
        </span>
        {detail && <span className={`tool-chevron ${open ? 'open' : ''}`}>⌄</span>}
      </button>
      {open && detail && <pre className="tool-output">{detail}</pre>}
    </div>
  );
}

// 深度思考块：默认折叠。
function ReasoningBlock({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="reasoning-block">
      <button type="button" className="reasoning-head" onClick={() => setOpen((o) => !o)}>
        <span>💭 深度思考</span>
        <span className={`tool-chevron ${open ? 'open' : ''}`}>⌄</span>
      </button>
      {open && <div className="reasoning-body">{text}</div>}
    </div>
  );
}

// 按顺序渲染 Claw 的结构化输出块（正文 / 思考 / 工具操作）。
function MessageBlocks({ blocks }: { blocks: AgentBlock[] }) {
  return (
    <>
      {blocks.map((b, i) => {
        if (b.kind === 'text') {
          return b.text.trim() ? <Markdown key={i}>{b.text}</Markdown> : null;
        }
        if (b.kind === 'reasoning') {
          return b.text.trim() ? <ReasoningBlock key={i} text={b.text} /> : null;
        }
        return <ToolCard key={i} block={b} />;
      })}
    </>
  );
}


const BOOT_MESSAGES = ['正在唤醒 Claw…', '启动本地网关…', '预热模型与插件…', '马上就好…'];

// 启动画面：网关就绪前的过渡，避免用户对着空白/报错干等。
function StartupSplash({ slow, onEnter }: { slow: boolean; onEnter: () => void }) {
  const [step, setStep] = useState(0);
  useEffect(() => {
    const timer = window.setInterval(() => {
      setStep((s) => (s + 1) % BOOT_MESSAGES.length);
    }, 2200);
    return () => window.clearInterval(timer);
  }, []);
  return (
    <div className="boot-splash">
      <div className="boot-inner">
        <div className="boot-emoji">🦞</div>
        <div className="boot-title">ClawBuddy</div>
        <div className="boot-spinner" aria-hidden />
        <div className="boot-status">{BOOT_MESSAGES[step]}</div>
        {slow && (
          <div className="boot-slow">
            <p>首次启动要预热模型和插件，稍慢是正常的。</p>
            <button type="button" className="boot-enter" onClick={onEnter}>
              直接进入
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

const VIEW_TITLES: Record<View, string> = {
  chat: '对话',
  settings: '设置',
  feishu: '飞书绑定',
  wechat: '微信绑定',
};

function newSessionKey() {
  return `ui-${crypto.randomUUID()}`;
}

const SKILL_PLAZA_URL = 'https://clawhub.ai/skills';

function App() {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [loading, setLoading] = useState(false);
  const [activeView, setActiveView] = useState<View>('chat');
  const [error, setError] = useState<string | null>(null);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [gatewayReady, setGatewayReady] = useState(false);
  const [gatewayChecking, setGatewayChecking] = useState(true);
  const [weChatRunning, setWeChatRunning] = useState<boolean | null>(null);
  const [feishuRunning, setFeishuRunning] = useState<boolean | null>(null);
  const [theme, setTheme] = useState<'light' | 'dark'>(() =>
    localStorage.getItem('clawbuddy_theme') === 'dark' ? 'dark' : 'light',
  );
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [currentSessionKey, setCurrentSessionKey] = useState<string>(newSessionKey);
  const [historyOpen, setHistoryOpen] = useState(true);
  const [historyExpanded, setHistoryExpanded] = useState(false);
  const [keyConfigured, setKeyConfigured] = useState<boolean | null>(null);
  // 启动画面：网关首次就绪前显示，避免用户对着空白干等。
  const [booted, setBooted] = useState(false);
  const [bootSlow, setBootSlow] = useState(false);

  useEffect(() => {
    invoke<boolean>('get_stepfun_key_status')
      .then(setKeyConfigured)
      .catch(() => setKeyConfigured(true));
  }, []);

  // 网关一旦就绪就收起启动画面（此后不再显示）。
  useEffect(() => {
    if (gatewayReady) {
      setBooted(true);
    }
  }, [gatewayReady]);

  // 启动偏慢（首启动要预热模型/插件）时给个「直接进入」出口，别把人卡死。
  useEffect(() => {
    if (booted) {
      return;
    }
    const timer = window.setTimeout(() => setBootSlow(true), 40000);
    return () => window.clearTimeout(timer);
  }, [booted]);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem('clawbuddy_theme', theme);
  }, [theme]);

  const abortRef = useRef<AbortController | null>(null);
  const endRef = useRef<HTMLDivElement | null>(null);

  const scrollToBottom = useCallback(() => {
    endRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, []);

  useEffect(() => {
    scrollToBottom();
  }, [messages, scrollToBottom]);

  const refreshSessions = useCallback(() => {
    listSessions().then(setSessions).catch(() => {});
  }, []);

  // 网关状态：启动 + 监听变化。
  useEffect(() => {
    let unlistenGateway: (() => void) | undefined;

    const fetchGateway = async () => {
      try {
        const ready = await invoke<boolean>('check_gateway_ready');
        setGatewayReady(ready);
        setGatewayChecking(false);
      } catch {
        setGatewayReady(false);
        setGatewayChecking(false);
      }
    };

    const setup = async () => {
      unlistenGateway = await listen<boolean>('gateway-status-changed', () => {
        fetchGateway();
      });
      try {
        const ready = await invoke<boolean>('check_gateway_ready');
        if (!ready) {
          await invoke('start_gateway');
        }
      } catch {
        // ignore
      }
      await fetchGateway();
      refreshSessions();
    };

    setup();
    return () => {
      unlistenGateway?.();
    };
  }, [refreshSessions]);

  // 渠道运行状态 + 网关就绪，定时刷新。
  useEffect(() => {
    let active = true;
    const poll = async () => {
      const ready = await invoke<boolean>('check_gateway_ready').catch(() => false);
      if (!active) return;
      setGatewayReady(ready);
      setGatewayChecking(false);
      if (!ready) {
        // 网关还在启动，渠道视为「加载中」（保持 null → 侧边栏显示环形指示灯）。
        setWeChatRunning(null);
        setFeishuRunning(null);
        return;
      }
      const [wechat, feishu] = await Promise.all([
        getChannelRunning('openclaw-weixin'),
        getChannelRunning('feishu'),
      ]);
      if (active) {
        setWeChatRunning(wechat);
        setFeishuRunning(feishu);
      }
    };
    poll();
    const timer = window.setInterval(poll, 8000);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, []);

  const handleSend = useCallback(
    async (text: string, attachments: Attachment[]) => {
      if ((!text && attachments.length === 0) || loading) {
        return;
      }

      const displayContent = text || `[${attachments.length} 个附件]`;
      const next: ChatMessage[] = [...messages, { role: 'user', content: displayContent }];
      setMessages(next);
      setLoading(true);
      setError(null);

      if (abortRef.current) {
        abortRef.current.abort();
      }
      const controller = new AbortController();
      abortRef.current = controller;

      let assistant = '';
      let latestBlocks: AgentBlock[] = [];

      const applyAssistant = () => {
        setMessages((current) => {
          const last = current[current.length - 1];
          const msg: ChatMessage = { role: 'assistant', content: assistant, blocks: latestBlocks };
          if (last?.role === 'assistant') {
            return [...current.slice(0, -1), msg];
          }
          return [...current, msg];
        });
      };

      try {
        await sendChatMessage(next, {
          sessionKey: currentSessionKey,
          attachments,
          signal: controller.signal,
          onChunk: (chunk) => {
            assistant += chunk;
            applyAssistant();
          },
          onBlocks: (blocks) => {
            latestBlocks = blocks;
            applyAssistant();
          },
        });

        setMessages((current) => {
          const last = current[current.length - 1];
          if (last?.role === 'assistant') {
            return current;
          }
          return [...current, { role: 'assistant', content: assistant, blocks: latestBlocks }];
        });
      } catch (err) {
        setError(err instanceof Error ? err.message : '发送消息失败');
      } finally {
        setLoading(false);
        abortRef.current = null;
        refreshSessions();
      }
    },
    [loading, messages, currentSessionKey, refreshSessions],
  );

  const handleNewChat = useCallback(() => {
    setMessages([]);
    setCurrentSessionKey(newSessionKey());
    setError(null);
    setActiveView('chat');
  }, []);

  const handleOpenSession = useCallback(async (fullKey: string) => {
    setActiveView('chat');
    setError(null);
    setCurrentSessionKey(toSendSessionKey(fullKey));
    try {
      const history = await loadSessionHistory(fullKey);
      setMessages(history);
    } catch (err) {
      setError(err instanceof Error ? err.message : '加载会话失败');
    }
  }, []);

  const handleDeleteSession = useCallback(
    async (fullKey: string) => {
      // 立即从列表移除该行（乐观更新），再发删除请求并刷新对齐。
      setSessions((cur) => cur.filter((s) => s.key !== fullKey));
      try {
        await deleteSession(fullKey);
      } catch (err) {
        setError(err instanceof Error ? err.message : '删除会话失败');
      }
      if (toSendSessionKey(fullKey) === currentSessionKey) {
        setMessages([]);
        setCurrentSessionKey(newSessionKey());
      }
      refreshSessions();
    },
    [currentSessionKey, refreshSessions],
  );

  const gatewayTone: Tone = gatewayReady ? 'green' : gatewayChecking ? 'amber' : 'red';
  const channelTone = (running: boolean | null): Tone =>
    running === null ? 'amber' : running ? 'green' : 'red';
  const weChatTone = channelTone(weChatRunning);
  const feishuTone = channelTone(feishuRunning);
  const visibleSessions = sessions.filter((s) => !s.key.endsWith(':main'));
  const shownSessions = historyExpanded ? visibleSessions : visibleSessions.slice(0, 5);
  // 启动画面：仅在「已配置 key 但网关还没首次就绪」时挡一下；未配置 key 时直接进入以显示配置卡。
  const showSplash = keyConfigured !== false && !booted;

  return (
    <div className="app">
      {showSplash && <StartupSplash slow={bootSlow} onEnter={() => setBooted(true)} />}
      <aside className={`sidebar ${sidebarOpen ? 'open' : 'closed'}`}>
        <div className="sidebar-header">
          <span className="sidebar-title">
            ClawBuddy <span className="sidebar-ver">v0.1</span>
          </span>
          <button
            type="button"
            className="sidebar-toggle"
            onClick={() => setSidebarOpen((prev) => !prev)}
            aria-label="折叠侧边栏"
            title="折叠侧边栏"
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
              <rect x="3" y="4" width="18" height="16" rx="2.5" />
              <line x1="9.5" y1="4" x2="9.5" y2="20" />
            </svg>
          </button>
        </div>

        <nav className="sidebar-nav">
          <button type="button" className={activeView === 'chat' ? 'active' : ''} onClick={() => setActiveView('chat')}>
            <span>对话</span>
            <Dot tone={gatewayTone} />
          </button>
          <button type="button" onClick={handleNewChat}>
            <span>＋ 新对话</span>
          </button>
          <button type="button" className={activeView === 'feishu' ? 'active' : ''} onClick={() => setActiveView('feishu')}>
            <span>飞书绑定</span>
            <Dot tone={feishuTone} loading={feishuRunning === null} />
          </button>
          <button type="button" className={activeView === 'wechat' ? 'active' : ''} onClick={() => setActiveView('wechat')}>
            <span>微信绑定</span>
            <Dot tone={weChatTone} loading={weChatRunning === null} />
          </button>
          <button type="button" onClick={() => openUrl(SKILL_PLAZA_URL).catch(() => {})}>
            <span>技能广场 ↗</span>
          </button>
        </nav>

        <div className="sidebar-history">
          <button type="button" className="history-toggle" onClick={() => setHistoryOpen((o) => !o)}>
            <span>历史会话{visibleSessions.length ? `（${visibleSessions.length}）` : ''}</span>
            <span>{historyOpen ? '▾' : '▸'}</span>
          </button>
          {historyOpen && (
            <div className="history-list">
              {visibleSessions.length === 0 && <div className="history-empty">暂无历史会话</div>}
              {shownSessions.map((s) => (
                <div
                  key={s.key}
                  className={`history-item ${toSendSessionKey(s.key) === currentSessionKey ? 'active' : ''}`}
                >
                  <button type="button" className="history-open" title={s.preview || s.title} onClick={() => handleOpenSession(s.key)}>
                    {s.title || '未命名会话'}
                  </button>
                  <button type="button" className="history-del" title="删除" onClick={() => handleDeleteSession(s.key)}>
                    ×
                  </button>
                </div>
              ))}
              {visibleSessions.length > 5 && (
                <button type="button" className="history-more" onClick={() => setHistoryExpanded((v) => !v)}>
                  {historyExpanded ? '收起' : `查看更多（${visibleSessions.length - 5}）`}
                </button>
              )}
            </div>
          )}
        </div>

        <div className="sidebar-bottom">
          <button
            type="button"
            className={`sidebar-row ${activeView === 'settings' ? 'active' : ''}`}
            onClick={() => setActiveView('settings')}
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3" />
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
            </svg>
            <span>设置</span>
          </button>
          <div className="sidebar-row appearance-row">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="9" />
              <path d="M12 3a6 9 0 0 0 0 18 6 9 0 0 0 0-18z" />
            </svg>
            <span>外观</span>
            <div className="theme-switch" role="group" aria-label="外观">
              <button type="button" className={theme === 'light' ? 'active' : ''} onClick={() => setTheme('light')}>
                浅色
              </button>
              <button type="button" className={theme === 'dark' ? 'active' : ''} onClick={() => setTheme('dark')}>
                深色
              </button>
            </div>
          </div>
        </div>
      </aside>

      <section className="stage">
        <header className="stage-header">
          <span>{VIEW_TITLES[activeView]}</span>
        </header>

        <main className="stage-main">
          {activeView === 'chat' && (
            <div className="chat">
              <div className="messages">
                {keyConfigured === false && (
                  <KeyConfigCard
                    onConfigured={() => setKeyConfigured(true)}
                    onGoSettings={() => setActiveView('settings')}
                  />
                )}
                {messages.length === 0 && (
                  <div className="empty-hero">
                    <div className="empty-emoji">🦞</div>
                    <h1>ClawBuddy</h1>
                    <p>你的本地 AI 伙伴 · 微信 / 飞书一处对话</p>
                  </div>
                )}
                {messages.map((message, index) => {
                  const isUser = message.role === 'user';
                  const isStreaming = loading && !isUser && index === messages.length - 1;
                  return (
                    <div key={index} className={`message ${message.role}`}>
                      {!isUser && <div className="message-avatar">🦞</div>}
                      <div className="message-content">
                        {!isUser && (
                          <div className="message-head">
                            <span className="message-role">Claw</span>
                            {message.content && <MessageCopy text={message.content} />}
                          </div>
                        )}
                        <div className="message-body">
                          {isUser ? (
                            <div className="plain-text">{message.content}</div>
                          ) : message.blocks && message.blocks.length > 0 ? (
                            <MessageBlocks blocks={message.blocks} />
                          ) : message.content ? (
                            <Markdown>{message.content}</Markdown>
                          ) : isStreaming ? (
                            <WorkingIndicator />
                          ) : (
                            <span className="typing">…</span>
                          )}
                          {isStreaming &&
                            (message.content || (message.blocks && message.blocks.length > 0)) && (
                              <WorkingIndicator compact />
                            )}
                        </div>
                      </div>
                    </div>
                  );
                })}
                <div ref={endRef} />
              </div>

              <Composer onSend={handleSend} loading={loading} />

              {error && <div className="error">{error}</div>}
            </div>
          )}

          {activeView === 'settings' && <SettingsPage messages={messages} onClearChat={() => setMessages([])} />}

          {activeView === 'feishu' && <FeishuBridgePage />}

          {activeView === 'wechat' && <WeChatBridgePage />}
        </main>
      </section>
    </div>
  );
}

export default App;
