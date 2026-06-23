import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { QRCodeSVG } from 'qrcode.react';
import { getChannelRunning } from '../hooks/useChat';
import { ChannelPairingPanel } from './ChannelPairingPanel';

// 微信扫码登录事件（由 Rust 转发自 openclaw 官方 weixin 插件的登录流程）。
type WeChatLoginEvent =
  | { type: 'preparing'; message?: string }
  | { type: 'qr'; url: string; sessionKey?: string; message?: string }
  | { type: 'connected'; message?: string }
  | { type: 'failed'; message?: string }
  | { type: 'error'; message?: string };

type Phase = 'idle' | 'preparing' | 'qr' | 'connected' | 'failed' | 'error';

export function WeChatBridgePage() {
  const [phase, setPhase] = useState<Phase>('idle');
  const [qrUrl, setQrUrl] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  const cleanup = useCallback(() => {
    unlistenRef.current?.();
    unlistenRef.current = null;
  }, []);

  useEffect(() => cleanup, [cleanup]);

  // 打开页面时回显真实状态：若微信渠道已在网关内运行，直接显示"已连接"。
  useEffect(() => {
    let active = true;
    getChannelRunning('openclaw-weixin').then((running) => {
      if (active && running) {
        setPhase('connected');
        setMessage('微信已连接');
      }
    });
    return () => {
      active = false;
    };
  }, []);

  const handleStart = useCallback(async () => {
    cleanup();
    setQrUrl(null);
    setMessage('正在准备微信插件…');
    setPhase('preparing');

    unlistenRef.current = await listen<WeChatLoginEvent>('wechat-login-event', (event) => {
      const payload = event.payload;
      switch (payload.type) {
        case 'preparing':
          setPhase('preparing');
          setMessage(payload.message ?? '正在准备…');
          break;
        case 'qr':
          setQrUrl(payload.url);
          setPhase('qr');
          setMessage(payload.message ?? '请用手机微信扫描二维码');
          break;
        case 'connected':
          setPhase('connected');
          setMessage(payload.message ?? '微信已连接');
          cleanup();
          break;
        case 'failed':
          setPhase('failed');
          setMessage(payload.message ?? '登录未完成，请重试');
          cleanup();
          break;
        case 'error':
          setPhase('error');
          setMessage(payload.message ?? '微信登录出错');
          cleanup();
          break;
      }
    });

    try {
      await invoke('wechat_login_start');
    } catch (err) {
      setPhase('error');
      setMessage(err instanceof Error ? err.message : '启动微信登录失败');
      cleanup();
    }
  }, [cleanup]);

  return (
    <div className="settings-card">
      <h2>微信桥接</h2>
      <p className="placeholder">
        通过 openclaw 官方微信插件扫码登录（首次会自动下载插件，可能需要等待几秒）。
      </p>

      <div className="settings-section">
        {phase === 'idle' && (
          <button type="button" onClick={handleStart}>
            启动微信扫码登录
          </button>
        )}

        {phase === 'preparing' && <span className="gateway-status connecting">{message}</span>}

        {phase === 'qr' && qrUrl && (
          <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 12 }}>
            <div style={{ background: '#fff', padding: 16, borderRadius: 12 }}>
              <QRCodeSVG value={qrUrl} size={220} />
            </div>
            <span>{message}</span>
            <button type="button" className="ghost-button" onClick={handleStart}>
              二维码失效？点此重新生成
            </button>
          </div>
        )}

        {phase === 'connected' && <span className="gateway-status ready">✅ {message}</span>}

        {(phase === 'failed' || phase === 'error') && (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            <span className="gateway-status failed">{message}</span>
            <button type="button" onClick={handleStart}>
              重试
            </button>
          </div>
        )}
      </div>

      <ChannelPairingPanel channel="openclaw-weixin" />
    </div>
  );
}
