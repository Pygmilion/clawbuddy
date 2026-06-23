import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface ModelOption {
  ref: string;
  label: string;
}

function labelFor(modelRef: string, options: ModelOption[]): string {
  const hit = options.find((o) => o.ref === modelRef);
  return hit?.label || modelRef || '选择模型';
}

export function ModelSwitcher() {
  const [active, setActive] = useState('');
  const [options, setOptions] = useState<ModelOption[]>([]);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);

  const refresh = () => {
    invoke<{ activeModel: string; models?: ModelOption[] }>('get_model_config')
      .then((cfg) => {
        setActive(cfg.activeModel || '');
        setOptions(cfg.models || []);
      })
      .catch(() => {});
  };

  useEffect(() => {
    refresh();
  }, []);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  }, [open]);

  const switchTo = async (modelRef: string) => {
    if (busy || modelRef === active) {
      setOpen(false);
      return;
    }
    setBusy(true);
    setOpen(false);
    try {
      await invoke('set_active_model', { modelRef });
      setActive(modelRef);
    } catch {
      // 切换失败时回读真实状态
      refresh();
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="model-switcher" ref={wrapRef}>
      <button type="button" className="model-switcher-btn" onClick={() => setOpen((o) => !o)} disabled={busy}>
        <span className="model-dot" />
        {busy ? '切换中…' : labelFor(active, options)}
        <span className="model-caret">▾</span>
      </button>
      {open && (
        <div className="model-menu">
          {options.map((o) => (
            <button
              key={o.ref}
              type="button"
              className={o.ref === active ? 'active' : ''}
              onClick={() => switchTo(o.ref)}
            >
              {o.label}
              {o.ref === active && <span className="model-check">✓</span>}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
