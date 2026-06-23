import { useCallback, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

type PairingRequest = Record<string, unknown> & {
  code?: string;
  pairingCode?: string;
  id?: string;
  senderId?: string;
  from?: string;
  displayName?: string;
};

function requestCode(req: PairingRequest): string | null {
  return (req.code || req.pairingCode || req.id || null) as string | null;
}

function requestLabel(req: PairingRequest): string {
  return (req.displayName || req.senderId || req.from || '未知发送者') as string;
}

// 渠道配对审批面板：列出待审批的陌生发送者并一键批准。
export function ChannelPairingPanel({ channel }: { channel: string }) {
  const [requests, setRequests] = useState<PairingRequest[]>([]);
  const [loading, setLoading] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setMsg(null);
    try {
      const list = (await invoke<PairingRequest[]>('pairing_list', { channel })) ?? [];
      setRequests(Array.isArray(list) ? list : []);
      if (!list.length) setMsg('暂无待审批的配对请求');
    } catch (err) {
      setMsg(err instanceof Error ? err.message : '获取配对请求失败');
    } finally {
      setLoading(false);
    }
  }, [channel]);

  const approve = useCallback(
    async (code: string) => {
      setMsg(null);
      try {
        await invoke('pairing_approve', { channel, code });
        setMsg(`已批准 ${code}`);
        await refresh();
      } catch (err) {
        setMsg(err instanceof Error ? err.message : '批准失败');
      }
    },
    [channel, refresh],
  );

  return (
    <div className="settings-section" style={{ marginTop: 16 }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <strong>配对审批</strong>
        <button type="button" className="ghost-button" onClick={refresh} disabled={loading}>
          {loading ? '刷新中…' : '刷新待审批'}
        </button>
      </div>
      <p className="hint" style={{ marginTop: 4 }}>
        陌生人首次给 bot 发消息需在此批准后，才会收到回复。
      </p>
      {requests.map((req, i) => {
        const code = requestCode(req);
        return (
          <div key={i} style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 6 }}>
            <span>
              {requestLabel(req)}
              {code ? `（${code}）` : ''}
            </span>
            {code && (
              <button type="button" onClick={() => approve(code)}>
                批准
              </button>
            )}
          </div>
        );
      })}
      {msg && <div className="status" style={{ marginTop: 6 }}>{msg}</div>}
    </div>
  );
}
