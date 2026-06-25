// CI 冒烟测试:连接本地网关,验证频道/插件状态 + 跑一轮真实对话。
// 用法:OC_DIR=<openclaw 包目录> node ci-smoke.mjs
// 退出码 0=对话成功拿到回复;非 0=失败。
const ocDir = process.env.OC_DIR;
if (!ocDir) {
  console.error('OC_DIR not set');
  process.exit(2);
}
const { WebSocket } = await import(`file://${ocDir}/node_modules/ws/wrapper.mjs`);

const URL = process.env.GW_URL || 'ws://127.0.0.1:18789';
const ws = new WebSocket(URL, { headers: { Origin: 'http://localhost' } });
const ids = {};
let emitted = '';
let runId = null;
let finished = false;

function req(method, params = {}) {
  const id = crypto.randomUUID();
  ids[id] = method;
  ws.send(JSON.stringify({ type: 'req', id, method, params }));
}

const timer = setTimeout(() => {
  console.error('SMOKE TIMEOUT; partial reply =', JSON.stringify(emitted));
  process.exit(1);
}, 90000);

ws.on('message', (data) => {
  let f;
  try { f = JSON.parse(data.toString()); } catch { return; }

  if (f.type === 'event' && f.event === 'connect.challenge') {
    req('connect', {
      minProtocol: 4, maxProtocol: 4,
      scopes: ['operator.admin', 'operator.write', 'operator.read'],
      client: { id: 'openclaw-control-ui', version: '0', platform: 'win32', mode: 'ui' },
    });
    return;
  }
  if (f.type === 'res' && ids[f.id] === 'connect') {
    if (f.ok === false) { console.error('connect failed', JSON.stringify(f.error)); process.exit(1); }
    req('channels.status', {});
    return;
  }
  if (f.type === 'res' && ids[f.id] === 'channels.status') {
    const p = f.payload || {};
    console.log('channels:', JSON.stringify(Object.keys(p.channels || {})));
    req('chat.send', { sessionKey: 'ci-smoke', message: '只回复两个字:你好', deliver: true, idempotencyKey: crypto.randomUUID() });
    return;
  }
  if (f.type === 'res' && ids[f.id] === 'chat.send') {
    runId = f.payload?.runId;
    console.log('chat.send started runId=', runId);
    return;
  }
  if (f.type === 'event' && f.event === 'chat') {
    const p = f.payload || {};
    if (p.state === 'error') {
      console.error('CHAT ERROR:', p.errorMessage || JSON.stringify(p));
      clearTimeout(timer); process.exit(1);
    }
    const c = (p.message?.content || []).map((x) => x.text || '').join('');
    if (c) emitted = c;
    if (p.state === 'final') {
      finished = true;
      clearTimeout(timer);
      console.log('CHAT FINAL:', JSON.stringify(emitted));
      process.exit(emitted.trim() ? 0 : 1);
    }
  }
});

ws.on('error', (e) => { console.error('ws error', e.message); process.exit(1); });
ws.on('close', () => { if (!finished) { console.error('ws closed before final'); process.exit(1); } });
