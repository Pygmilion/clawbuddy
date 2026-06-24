#!/usr/bin/env bash
# 修复 + 诊断：把已保存的 StepFun Key 写进 openclaw 配置（models.providers.stepfun.apiKey），
# 让网关不依赖环境变量也能拿到 key。运行后请退出并重新打开 ClawBuddy 再试对话。
set -uo pipefail

STATE="$HOME/.clawbuddy/state"
CFG="$STATE/openclaw.json"
CRED="$STATE/clawbuddy-credentials.json"

echo "===== 诊断信息（可截图发回）====="
echo "state dir: $STATE"
echo "-- openclaw.json 是否存在: $([ -f "$CFG" ] && echo yes || echo NO)"
echo "-- 凭据文件是否存在: $([ -f "$CRED" ] && echo yes || echo NO)"

if [ ! -f "$CRED" ]; then
  echo "未找到凭据文件，请先在 ClawBuddy 设置里保存一次 StepFun Key。"
  exit 1
fi

KEY=$(python3 -c "import json;print(json.load(open('$CRED')).get('stepfunApiKey',''))" 2>/dev/null)
if [ -z "$KEY" ]; then
  echo "凭据文件里没有 stepfunApiKey，请先在设置里保存 Key。"
  exit 1
fi
echo "-- 已读到 key，长度: ${#KEY}（前4位 ${KEY:0:4}…）"

echo "-- 当前配置里的 stepfun provider:"
python3 -c "import json;d=json.load(open('$CFG'));print('   ', json.dumps(d.get('models',{}).get('providers',{}).get('stepfun',{}), ensure_ascii=False))" 2>/dev/null
echo "-- plugins.entries:"
python3 -c "import json;d=json.load(open('$CFG'));print('   ', json.dumps(d.get('plugins',{}).get('entries',{}), ensure_ascii=False))" 2>/dev/null
echo "-- stepfun provider 插件是否已安装:"
ls -d "$STATE"/npm/projects/openclaw-stepfun-provider-* 2>/dev/null || echo "   未安装（npm/projects 下没有）"
echo "-- 网关 health:"
curl -s --max-time 5 http://127.0.0.1:18789/health 2>/dev/null || echo "   连不上 18789"
echo

echo "===== 开始修复 ====="
python3 - "$CFG" "$KEY" <<'PY'
import json,sys
cfg_path,key=sys.argv[1],sys.argv[2]
d=json.load(open(cfg_path))
sf=d.setdefault("models",{}).setdefault("providers",{}).setdefault("stepfun",{})
sf["apiKey"]=key
sf.setdefault("baseUrl","https://api.stepfun.com/v1")
# 确保 stepfun 插件启用
ent=d.setdefault("plugins",{}).setdefault("entries",{})
ent.setdefault("stepfun",{})["enabled"]=True
json.dump(d,open(cfg_path,"w"),ensure_ascii=False,indent=2)
print("已把 apiKey 写入 models.providers.stepfun，并启用 stepfun 插件。")
PY

echo "-- 重启网关（杀掉占用 18789 的进程）…"
lsof -ti tcp:18789 -sTCP:LISTEN 2>/dev/null | xargs kill 2>/dev/null || true
sleep 1
echo
echo "完成。请【完全退出 ClawBuddy 再重新打开】，然后在对话里再试一次。"
echo "如果还不行，把上面「诊断信息」整段截图发回。"
