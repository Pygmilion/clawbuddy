#!/usr/bin/env bash
# 发布 GitHub Release 并上传 macOS 安装包（.dmg）。
# 需要已安装并登录 gh：gh auth login
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

TAG="v0.1.0"
TITLE="ClawBuddy v0.1.0"
DMG="src-tauri/target/release/bundle/dmg/ClawBuddy_0.1.0_aarch64.dmg"
NOTES="docs/release-notes-v0.1.0.md"

GH="$(command -v gh || true)"
if [ -z "$GH" ] && [ -x /tmp/gh_portable/bin/gh ]; then GH=/tmp/gh_portable/bin/gh; fi
if [ -z "$GH" ]; then
  echo "未找到 gh CLI。请先安装并登录：gh auth login" >&2
  exit 1
fi

if [ ! -f "$DMG" ]; then
  echo "未找到安装包：$DMG" >&2
  echo "请先构建：npm run tauri build -- --bundles app  然后用 hdiutil 生成 dmg（见 README）。" >&2
  exit 1
fi

if "$GH" release view "$TAG" >/dev/null 2>&1; then
  echo "已存在 $TAG，更新安装包…"
  "$GH" release upload "$TAG" "$DMG" --clobber
else
  echo "创建 Release $TAG…"
  "$GH" release create "$TAG" "$DMG" --title "$TITLE" --notes-file "$NOTES"
fi

echo "完成 → https://github.com/Pygmilion/clawbuddy/releases/tag/$TAG"
