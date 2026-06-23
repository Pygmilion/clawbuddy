import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { QRCodeSVG } from 'qrcode.react';
import { getChannelRunning } from '../hooks/useChat';
import { ChannelPairingPanel } from './ChannelPairingPanel';

export enum FeishuBridgeStatus {
  Idle = 'idle',
  Checking = 'checking',
  Ready = 'ready',
  Failed = 'failed',
}

export function FeishuStatusIndicator({
  status,
  onRetry,
}: {
  status: FeishuBridgeStatus;
  onRetry: () => Promise<void>;
}) {
  if (status === FeishuBridgeStatus.Ready) {
    return <span className="gateway-status ready">飞书桥接已就绪</span>;
  }
  if (status === FeishuBridgeStatus.Checking) {
    return <span className="gateway-status connecting">检查飞书桥接中...</span>;
  }
  return (
    <span className="gateway-status failed">
      <button type="button" onClick={onRetry}>
        飞书桥接未就绪，点击重试
      </button>
    </span>
  );
}

// 飞书扫码登录事件（由 Rust 转发自 openclaw 官方 feishu 插件的设备码扫码流程）。
type FeishuLoginEvent =
  | { type: 'preparing'; message?: string }
  | { type: 'qr'; url: string; userCode?: string }
  | { type: 'connected'; message?: string }
  | { type: 'ready'; message?: string }
  | { type: 'failed'; status?: string; message?: string }
  | { type: 'error'; message?: string };

type Phase = 'idle' | 'preparing' | 'qr' | 'connecting' | 'ready' | 'failed' | 'error';

function FeishuBridgePage() {
  const [phase, setPhase] = useState<Phase>('idle');
  const [qrUrl, setQrUrl] = useState<string | null>(null);
  const [userCode, setUserCode] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  const cleanup = useCallback(() => {
    unlistenRef.current?.();
    unlistenRef.current = null;
  }, []);

  useEffect(() => cleanup, [cleanup]);

  // 打开页面时回显真实状态：若飞书渠道已在网关内运行，直接显示"已连接"。
  useEffect(() => {
    let active = true;
    getChannelRunning('feishu').then((running) => {
      if (active && running) {
        setPhase('ready');
        setMessage('飞书已连接');
      }
    });
    return () => {
      active = false;
    };
  }, []);

  const handleStart = useCallback(async () => {
    cleanup();
    setQrUrl(null);
    setUserCode(null);
    setMessage('正在准备飞书插件…');
    setPhase('preparing');

    unlistenRef.current = await listen<FeishuLoginEvent>('feishu-login-event', (event) => {
      const payload = event.payload;
      switch (payload.type) {
        case 'preparing':
          setPhase('preparing');
          setMessage(payload.message ?? '正在准备…');
          break;
        case 'qr':
          setQrUrl(payload.url);
          setUserCode(payload.userCode ?? null);
          setPhase('qr');
          setMessage('请用飞书（个人版可用）扫描二维码授权');
          break;
        case 'connected':
          setPhase('connecting');
          setMessage(payload.message ?? '扫码成功，正在连接…');
          break;
        case 'ready':
          setPhase('ready');
          setMessage(payload.message ?? '飞书已连接');
          cleanup();
          break;
        case 'failed':
          setPhase('failed');
          setMessage(payload.message ?? '登录未完成，请重试');
          cleanup();
          break;
        case 'error':
          setPhase('error');
          setMessage(payload.message ?? '飞书登录出错');
          cleanup();
          break;
      }
    });

    try {
      await invoke('feishu_login_start');
    } catch (err) {
      setPhase('error');
      setMessage(err instanceof Error ? err.message : '启动飞书登录失败');
      cleanup();
    }
  }, [cleanup]);

  return (
    <div className="settings-card">
      <h2>飞书桥接</h2>
      <p className="placeholder">
        通过 openclaw 官方飞书插件扫码自动创建 bot（个人版飞书即可，无需手动建应用；首次会自动下载插件）。
      </p>

      <div className="settings-section">
        {phase === 'idle' && (
          <button type="button" onClick={handleStart}>
            启动飞书扫码登录
          </button>
        )}

        {(phase === 'preparing' || phase === 'connecting') && (
          <span className="gateway-status connecting">{message}</span>
        )}

        {phase === 'qr' && qrUrl && (
          <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 12 }}>
            <div style={{ background: '#fff', padding: 16, borderRadius: 12 }}>
              <QRCodeSVG value={qrUrl} size={220} />
            </div>
            <span>{message}</span>
            {userCode && <span style={{ opacity: 0.7 }}>验证码：{userCode}</span>}
            <button type="button" className="ghost-button" onClick={handleStart}>
              二维码失效？点此重新生成
            </button>
          </div>
        )}

        {phase === 'ready' && <span className="gateway-status ready">✅ {message}</span>}

        {(phase === 'failed' || phase === 'error') && (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            <span className="gateway-status failed">{message}</span>
            <button type="button" onClick={handleStart}>
              重试
            </button>
          </div>
        )}
      </div>

      <ChannelPairingPanel channel="feishu" />
    </div>
  );
}

export { FeishuBridgePage };
