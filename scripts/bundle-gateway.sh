#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$ROOT_DIR/bundled/bin"
mkdir -p "$BIN_DIR"

copy_runtime_bin() {
  local src="$ROOT_DIR/src-tauri/runtime"
  if [ -d "$src" ]; then
    if ls "$src"/openclaw-gateway* >/dev/null 2>&1; then
      cp "$src"/openclaw-gateway* "$BIN_DIR"/
      return 0
    fi
  fi
  return 1
}

download_from_env() {
  local url="${OPENCLAW_GATEWAY_URL:-}"
  if [ -n "$url" ]; then
    if command -v curl >/dev/null 2>&1; then
      local dest="$BIN_DIR/$(basename "$url")"
      curl -fL "$url" -o "$dest"
      chmod +x "$dest"
      return 0
    fi
  fi
  return 1
}

create_placeholder_bin() {
  local placeholder="$BIN_DIR/openclaw-gateway"
  cat > "$placeholder" <<'PLACEHOLDER'
#!/usr/bin/env bash
echo "OpenClaw Gateway placeholder; replace with real binary."
PLACEHOLDER
  chmod +x "$placeholder"
}

if copy_runtime_bin; then
  exit 0
fi

if download_from_env; then
  exit 0
fi

create_placeholder_bin

cat <<MSG
[bundle-gateway] 未找到 OpenClaw Gateway 二进制，已生成占位文件 bundled/bin/openclaw-gateway。
- 请将其替换为实际二进制
- 或设置环境变量 OPENCLAW_GATEWAY_URL=<二进制下载地址>
MSG
