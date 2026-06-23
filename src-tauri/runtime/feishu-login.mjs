// ClawBuddy 飞书扫码登录 helper。
// 由 Rust 用打包 node 启动；参数为已安装的 @openclaw/feishu 插件的 app-registration 模块绝对路径。
// beginAppRegistration 与 pollAppRegistration 共享进程内状态，必须同一进程先后调用。
//
// 输出协议：仅 "@@CLAWFS@@ " 前缀的行是给 Rust 解析的 JSON（插件日志也会打到 stdout，Rust 忽略非前缀行）：
//   {type:"qr", url, userCode}
//   {type:"connected", clientId, clientSecret}
//   {type:"failed", status, message}
//   {type:"error", message}

const SENTINEL = '@@CLAWFS@@ ';
const emit = (obj) => process.stdout.write(SENTINEL + JSON.stringify(obj) + '\n');

const modulePath = process.argv[2];
if (!modulePath) {
  emit({ type: 'error', message: '缺少 app-registration 模块路径参数' });
  process.exit(1);
}

try {
  const mod = await import(modulePath);
  const begin = await mod.beginAppRegistration('feishu');
  if (!begin?.qrUrl) {
    emit({ type: 'error', message: '未获取到飞书二维码' });
    process.exit(1);
  }
  emit({ type: 'qr', url: begin.qrUrl, userCode: begin.userCode });

  const outcome = await mod.pollAppRegistration({
    deviceCode: begin.deviceCode,
    interval: begin.interval,
    expireIn: begin.expireIn,
    initialDomain: 'feishu',
    tp: 'ob_cli_app',
  });

  if (outcome?.status === 'success') {
    const result = outcome.result || {};
    emit({
      type: 'connected',
      appId: result.appId,
      appSecret: result.appSecret,
      domain: result.domain,
      openId: result.openId,
    });
  } else {
    emit({ type: 'failed', status: outcome?.status, message: outcome?.message || outcome?.status || '登录未完成' });
  }
} catch (err) {
  emit({ type: 'error', message: err instanceof Error ? err.message : String(err) });
} finally {
  process.exit(0);
}
