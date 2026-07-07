import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { openUrl } from '@tauri-apps/plugin-opener';

export function KeyConfigCard({
  onConfigured,
  onGoSettings,
}: {
  onConfigured: () => void;
  onGoSettings: () => void;
}) {
  const [key, setKey] = useState('');
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);

  const save = async () => {
    const k = key.trim();
    if (!k) {
      setMsg('请先填入 StepFun API Key');
      return;
    }
    setBusy(true);
    setMsg('正在保存并启动网关…');
    try {
      await invoke('set_stepfun_key', { key: k });
      await invoke('set_active_model', { modelRef: 'stepfun/step-3.5-flash' }).catch(() => {});
      setMsg(null);
      onConfigured();
    } catch (error) {
      setMsg(error instanceof Error ? error.message : '保存失败');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="keycard">
      <div className="keycard-title">🔑 先配置模型 Key</div>
      <p className="keycard-desc">
        还没有配置模型 API Key，Claw 暂时无法对话。填入阶跃星辰（StepFun）API Key 即可开始，保存后自动启用默认模型。
      </p>
      <div className="keycard-row">
        <input
          type="password"
          value={key}
          onChange={(e) => setKey(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') save();
          }}
          placeholder="粘贴 StepFun API Key"
          disabled={busy}
        />
        <button type="button" onClick={save} disabled={busy}>
          {busy ? '保存中…' : '保存并启用'}
        </button>
      </div>
      <div className="keycard-foot">
        <button type="button" className="keycard-link" onClick={() => openUrl('https://platform.stepfun.com').catch(() => {})}>
          去 StepFun 获取 Key ↗
        </button>
        <button type="button" className="keycard-link" onClick={onGoSettings}>
          更多模型设置
        </button>
      </div>
      {msg && <div className="keycard-msg">{msg}</div>}
    </div>
  );
}
