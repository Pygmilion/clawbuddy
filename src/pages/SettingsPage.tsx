import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listAgentFiles, getAgentFile, setAgentFile, type AgentFile } from '../hooks/useChat';

type Provider = 'openai' | 'anthropic';

export interface SettingsState {
  apiKey: string;
  provider: Provider;
  model: string;
}

const STORAGE_KEY = 'clawbuddy_settings';

const DEFAULT_SETTINGS: SettingsState = {
  apiKey: '',
  provider: 'openai',
  model: 'gpt-4o',
};

function encodeKey(apiKey: string): string {
  try {
    return btoa(unescape(encodeURIComponent(apiKey)));
  } catch {
    return apiKey;
  }
}

function decodeKey(encoded: string): string {
  try {
    return decodeURIComponent(escape(atob(encoded)));
  } catch {
    return encoded;
  }
}

// 兼容旧版的本地设置 hook（App 仍在用；当前模型实际由网关配置决定）。
export function useSettings() {
  const [settings, setSettings] = useState<SettingsState>(() => {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored) {
      try {
        const parsed = JSON.parse(stored) as Partial<SettingsState> & { apiKey?: string };
        return {
          ...DEFAULT_SETTINGS,
          ...parsed,
          apiKey: parsed.apiKey ? decodeKey(parsed.apiKey) : '',
        };
      } catch {
        // ignore
      }
    }
    return DEFAULT_SETTINGS;
  });

  const updateSettings = (next: Partial<SettingsState>) => {
    setSettings((current) => {
      const merged = { ...current, ...next };
      localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...merged, apiKey: encodeKey(merged.apiKey) }));
      return merged;
    });
  };

  const resetSettings = () => {
    localStorage.removeItem(STORAGE_KEY);
    setSettings(DEFAULT_SETTINGS);
  };

  return { settings, updateSettings, resetSettings };
}

interface SettingsPageProps {
  messages: Array<{ role: string; content: string }>;
  onClearChat?: () => void;
}

export function SettingsPage({ onClearChat }: SettingsPageProps) {
  const [status, setStatus] = useState<string | null>(null);

  // StepFun（默认模型后端）
  const [stepfunKey, setStepfunKey] = useState('');
  const [stepfunConfigured, setStepfunConfigured] = useState(false);
  const [stepfunSaving, setStepfunSaving] = useState(false);
  const [stepfunMsg, setStepfunMsg] = useState<string | null>(null);

  // StepFun 账户余额/用量
  interface StepfunAccount {
    type?: string;
    balance?: number;
    total_cash_balance?: number;
    total_voucher_balance?: number;
  }
  const [account, setAccount] = useState<StepfunAccount | null>(null);
  const [accountBusy, setAccountBusy] = useState(false);
  const [accountMsg, setAccountMsg] = useState<string | null>(null);

  const loadAccount = async () => {
    setAccountBusy(true);
    setAccountMsg(null);
    try {
      const data = await invoke<StepfunAccount>('get_stepfun_account');
      setAccount(data);
    } catch (error) {
      setAccountMsg(error instanceof Error ? error.message : '查询失败');
    } finally {
      setAccountBusy(false);
    }
  };

  // 模型 API 管理
  const [activeModel, setActiveModel] = useState('');
  const [providers, setProviders] = useState<string[]>([]);
  const [cp, setCp] = useState({ id: '', baseUrl: '', apiKey: '', model: '' });
  const [cpBusy, setCpBusy] = useState(false);
  const [cpMsg, setCpMsg] = useState<string | null>(null);

  const STEPFUN_MODELS = [
    { id: 'step-3.5-flash', label: 'Step 3.5 Flash（文本）' },
    { id: 'step-3.7-flash', label: 'Step 3.7（多模态）' },
  ];
  const handleSwitchStepfunModel = async (model: string) => {
    setCpBusy(true);
    setCpMsg('正在切换 StepFun 模型并重启网关…');
    try {
      await invoke('set_active_model', { modelRef: `stepfun/${model}` });
      setCpMsg(`已切换到 stepfun/${model}`);
      refreshModelConfig();
    } catch (error) {
      setCpMsg(error instanceof Error ? error.message : '切换失败');
    } finally {
      setCpBusy(false);
    }
  };

  // OpenClaw 升级
  const [upgrading, setUpgrading] = useState(false);
  const [upgradeMsg, setUpgradeMsg] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [updateInfo, setUpdateInfo] = useState<{ current: string; latest: string; updateAvailable: boolean } | null>(null);

  const handleCheckUpdate = async () => {
    setChecking(true);
    setUpgradeMsg(null);
    try {
      const info = await invoke<{ current: string; latest: string; updateAvailable: boolean }>('check_openclaw_update');
      setUpdateInfo(info);
    } catch (error) {
      setUpgradeMsg(error instanceof Error ? error.message : '检查更新失败');
    } finally {
      setChecking(false);
    }
  };

  // 高级配置（直接编辑 claw 配置文件：soul/memory 等）
  const [configText, setConfigText] = useState('');
  const [configOpen, setConfigOpen] = useState(false);
  const [configBusy, setConfigBusy] = useState(false);
  const [configMsg, setConfigMsg] = useState<string | null>(null);

  const loadConfig = async () => {
    try {
      const raw = await invoke<string>('get_claw_config');
      setConfigText(raw);
      setConfigOpen(true);
      setConfigMsg(null);
    } catch (error) {
      setConfigMsg(error instanceof Error ? error.message : '读取配置失败');
    }
  };

  const saveConfig = async () => {
    setConfigBusy(true);
    setConfigMsg('正在保存并重启网关…');
    try {
      await invoke('set_claw_config', { raw: configText });
      setConfigMsg('已保存，网关已重启。');
    } catch (error) {
      setConfigMsg(error instanceof Error ? error.message : '保存失败');
    } finally {
      setConfigBusy(false);
    }
  };

  // Agent 角色/记忆文件（SOUL.md / USER.md 等）
  const [agentFiles, setAgentFiles] = useState<AgentFile[]>([]);
  const [selectedFile, setSelectedFile] = useState('');
  const [fileContent, setFileContent] = useState('');
  const [fileBusy, setFileBusy] = useState(false);
  const [fileMsg, setFileMsg] = useState<string | null>(null);

  useEffect(() => {
    listAgentFiles('dev').then(setAgentFiles).catch(() => {});
  }, []);

  const openAgentFile = async (name: string) => {
    setSelectedFile(name);
    setFileMsg(null);
    if (!name) {
      setFileContent('');
      return;
    }
    try {
      setFileContent(await getAgentFile(name, 'dev'));
    } catch (error) {
      setFileMsg(error instanceof Error ? error.message : '读取失败');
    }
  };

  const saveAgentFile = async () => {
    if (!selectedFile) return;
    setFileBusy(true);
    setFileMsg('保存中…');
    try {
      await setAgentFile(selectedFile, fileContent, 'dev');
      setFileMsg('已保存');
      listAgentFiles('dev').then(setAgentFiles).catch(() => {});
    } catch (error) {
      setFileMsg(error instanceof Error ? error.message : '保存失败');
    } finally {
      setFileBusy(false);
    }
  };

  const refreshModelConfig = () => {
    invoke<{ activeModel: string; providers: string[] }>('get_model_config')
      .then((cfg) => {
        setActiveModel(cfg.activeModel || '');
        setProviders(cfg.providers || []);
      })
      .catch(() => {});
  };

  useEffect(() => {
    invoke<boolean>('get_stepfun_key_status')
      .then((ok) => {
        setStepfunConfigured(ok);
        if (ok) loadAccount();
      })
      .catch(() => setStepfunConfigured(false));
    refreshModelConfig();
  }, []);

  const handleSaveStepfun = async () => {
    const key = stepfunKey.trim();
    if (!key) {
      setStepfunMsg('请先填入 StepFun API Key');
      return;
    }
    setStepfunSaving(true);
    setStepfunMsg('正在保存并重启网关…');
    try {
      await invoke('set_stepfun_key', { key });
      await invoke('set_active_model', { modelRef: 'stepfun/step-3.5-flash' }).catch(() => {});
      setStepfunConfigured(true);
      setStepfunKey('');
      setStepfunMsg('已保存，网关已重启。');
      refreshModelConfig();
      loadAccount();
    } catch (error) {
      setStepfunMsg(error instanceof Error ? error.message : '保存失败');
    } finally {
      setStepfunSaving(false);
    }
  };

  const handleSaveProvider = async () => {
    if (!cp.id.trim() || !cp.baseUrl.trim() || !cp.model.trim()) {
      setCpMsg('名称、Base URL、模型名都要填');
      return;
    }
    setCpBusy(true);
    setCpMsg('正在保存并重启网关…');
    try {
      await invoke('save_model_provider', {
        id: cp.id.trim(),
        baseUrl: cp.baseUrl.trim(),
        apiKey: cp.apiKey.trim(),
        model: cp.model.trim(),
      });
      setCpMsg(`已添加并启用 ${cp.id.trim()}/${cp.model.trim()}`);
      setCp({ id: '', baseUrl: '', apiKey: '', model: '' });
      refreshModelConfig();
    } catch (error) {
      setCpMsg(error instanceof Error ? error.message : '保存失败');
    } finally {
      setCpBusy(false);
    }
  };

  const handleUseStepfun = async () => {
    setCpBusy(true);
    setCpMsg('正在切回 StepFun…');
    try {
      await invoke('set_active_model', { modelRef: 'stepfun/step-3.5-flash' });
      setCpMsg('已切回 StepFun 默认模型');
      refreshModelConfig();
    } catch (error) {
      setCpMsg(error instanceof Error ? error.message : '切换失败');
    } finally {
      setCpBusy(false);
    }
  };

  const handleUpgradeOpenclaw = async () => {
    setUpgrading(true);
    setUpgradeMsg('正在升级 OpenClaw 并重启网关…（可能要一两分钟）');
    try {
      const version = await invoke<string>('upgrade_openclaw');
      setUpgradeMsg(`已升级到 OpenClaw ${version}，网关已重启。`);
    } catch (error) {
      setUpgradeMsg(error instanceof Error ? error.message : '升级失败');
    } finally {
      setUpgrading(false);
    }
  };

  return (
    <div className="settings-page">
      <section className="settings-section">
        <h2>StepFun API Key（默认模型）</h2>
        <p className="hint">
          当前状态：{stepfunConfigured ? '✅ 已配置' : '❌ 未配置'}。填入阶跃星辰 API Key 即可，保存后自动重启网关并使用 stepfun/step-3.5-flash。
        </p>
        <label className="field">
          <span>StepFun Key</span>
          <input
            type="password"
            value={stepfunKey}
            onChange={(event) => setStepfunKey(event.currentTarget.value)}
            placeholder="填入后点击保存"
          />
          <button type="button" onClick={handleSaveStepfun} disabled={stepfunSaving}>
            {stepfunSaving ? '保存中…' : '保存并启用'}
          </button>
        </label>
        <label className="field">
          <span>StepFun 模型</span>
          <select
            value={activeModel.startsWith('stepfun/') ? activeModel.slice('stepfun/'.length) : 'step-3.5-flash'}
            onChange={(e) => handleSwitchStepfunModel(e.currentTarget.value)}
            disabled={cpBusy}
          >
            {STEPFUN_MODELS.map((m) => (
              <option key={m.id} value={m.id}>
                {m.label}
              </option>
            ))}
          </select>
        </label>
        {stepfunMsg && <div className="status">{stepfunMsg}</div>}

        <div className="account-card">
          <div className="account-card-head">
            <span>账户余额 / 用量</span>
            <button type="button" className="ghost-button" onClick={loadAccount} disabled={accountBusy}>
              {accountBusy ? '查询中…' : '刷新'}
            </button>
          </div>
          {account ? (
            <div className="account-grid">
              <div className="account-item">
                <span className="account-label">可用余额</span>
                <span className="account-value">¥{(account.balance ?? 0).toFixed(2)}</span>
              </div>
              <div className="account-item">
                <span className="account-label">累计充值</span>
                <span className="account-value">¥{(account.total_cash_balance ?? 0).toFixed(2)}</span>
              </div>
              <div className="account-item">
                <span className="account-label">赠送金额</span>
                <span className="account-value">¥{(account.total_voucher_balance ?? 0).toFixed(2)}</span>
              </div>
              <div className="account-item">
                <span className="account-label">累计消费</span>
                <span className="account-value">
                  ¥
                  {Math.max(
                    0,
                    (account.total_cash_balance ?? 0) + (account.total_voucher_balance ?? 0) - (account.balance ?? 0),
                  ).toFixed(2)}
                </span>
              </div>
            </div>
          ) : (
            <p className="hint">{accountMsg || (stepfunConfigured ? '点击刷新查询账户余额。' : '配置 Key 后可查询账户余额。')}</p>
          )}
          <p className="hint">注：StepFun 仅提供余额接口，「累计消费」由充值+赠送−余额估算，非当月用量；当月明细请到 StepFun 控制台查看。</p>
        </div>
      </section>

      <section className="settings-section">
        <h2>模型 API 管理</h2>
        <p className="hint">
          当前模型：<strong>{activeModel || '（未设置）'}</strong>
          {providers.length > 0 && <>　已配置 provider：{providers.join('、')}</>}
        </p>
        <p className="hint">添加其他 OpenAI 兼容 API（如 OpenRouter、DeepSeek、本地模型等），保存后立即启用：</p>
        <label className="field">
          <span>名称/ID</span>
          <input value={cp.id} onChange={(e) => setCp({ ...cp, id: e.currentTarget.value })} placeholder="如 openrouter" />
        </label>
        <label className="field">
          <span>Base URL</span>
          <input value={cp.baseUrl} onChange={(e) => setCp({ ...cp, baseUrl: e.currentTarget.value })} placeholder="https://.../v1" />
        </label>
        <label className="field">
          <span>API Key</span>
          <input type="password" value={cp.apiKey} onChange={(e) => setCp({ ...cp, apiKey: e.currentTarget.value })} placeholder="sk-..." />
        </label>
        <label className="field">
          <span>模型名</span>
          <input value={cp.model} onChange={(e) => setCp({ ...cp, model: e.currentTarget.value })} placeholder="如 gpt-4o" />
        </label>
        <div className="actions">
          <button type="button" onClick={handleSaveProvider} disabled={cpBusy}>
            {cpBusy ? '处理中…' : '保存并启用'}
          </button>
          <button type="button" className="ghost-button" onClick={handleUseStepfun} disabled={cpBusy}>
            切回 StepFun 默认
          </button>
        </div>
        {cpMsg && <div className="status">{cpMsg}</div>}
      </section>

      {import.meta.env.DEV && (
      <section className="settings-section">
        <h2>OpenClaw 升级</h2>
        <p className="hint">检查并升级到最新版 OpenClaw（升级会自动重启网关，期间对话/绑定短暂中断）。</p>
        {updateInfo && (
          <p className="hint">
            当前版本 <strong>{updateInfo.current}</strong>，最新版本 <strong>{updateInfo.latest}</strong>
            {updateInfo.updateAvailable ? '　🔴 有新版本可升级' : '　✅ 已是最新'}
          </p>
        )}
        <div className="actions">
          <button type="button" className="ghost-button" onClick={handleCheckUpdate} disabled={checking || upgrading}>
            {checking ? '检查中…' : '检查更新'}
          </button>
          <button type="button" onClick={handleUpgradeOpenclaw} disabled={upgrading}>
            {upgrading ? '升级中…' : '升级 OpenClaw'}
          </button>
        </div>
        {upgradeMsg && <div className="status">{upgradeMsg}</div>}
      </section>
      )}

      <section className="settings-section">
        <h2>高级配置（claw 配置文件）</h2>
        <p className="hint">
          直接编辑 openclaw 配置（soul / memory / agents 等）。保存会校验 JSON 并重启网关，改错可能导致网关启动失败，请谨慎。
        </p>
        {!configOpen ? (
          <button type="button" className="ghost-button" onClick={loadConfig}>
            加载配置文件
          </button>
        ) : (
          <>
            <textarea
              className="config-editor"
              value={configText}
              onChange={(e) => setConfigText(e.currentTarget.value)}
              spellCheck={false}
              rows={18}
            />
            <div className="actions">
              <button type="button" onClick={saveConfig} disabled={configBusy}>
                {configBusy ? '保存中…' : '保存并重启'}
              </button>
              <button type="button" className="ghost-button" onClick={loadConfig} disabled={configBusy}>
                重新加载
              </button>
            </div>
          </>
        )}
        {configMsg && <div className="status">{configMsg}</div>}
      </section>

      <section className="settings-section">
        <h2>Agent 角色/记忆文件</h2>
        <p className="hint">
          分文件编辑 SOUL.md（人设）、USER.md（关于你）、IDENTITY.md、AGENTS.md、TOOLS.md 等，保存即写入 agent 工作区。
        </p>
        <label className="field">
          <span>选择文件</span>
          <select value={selectedFile} onChange={(e) => openAgentFile(e.currentTarget.value)}>
            <option value="">-- 选择文件 --</option>
            {agentFiles.map((f) => (
              <option key={f.name} value={f.name}>
                {f.name}
                {f.missing ? '（未创建）' : ''}
              </option>
            ))}
          </select>
        </label>
        {selectedFile && (
          <>
            <textarea
              className="config-editor"
              value={fileContent}
              onChange={(e) => setFileContent(e.currentTarget.value)}
              rows={14}
              spellCheck={false}
              placeholder={`${selectedFile} 内容…`}
            />
            <div className="actions">
              <button type="button" onClick={saveAgentFile} disabled={fileBusy}>
                {fileBusy ? '保存中…' : '保存'}
              </button>
            </div>
          </>
        )}
        {fileMsg && <div className="status">{fileMsg}</div>}
      </section>

      <section className="settings-section">
        <h2>操作</h2>
        <div className="actions">
          <button
            type="button"
            onClick={() => {
              onClearChat?.();
              setStatus('当前会话已清空');
            }}
          >
            清空当前会话
          </button>
        </div>
        {status && <div className="status">{status}</div>}
      </section>
    </div>
  );
}
