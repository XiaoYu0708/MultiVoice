#!/usr/bin/env bash
# 發佈新版本到 GitHub Releases（客戶端啟動時會比對這裡的最新版本，提示使用者更新）
#
#   tools/release.sh 0.2.0 "這版的更新說明"
#
# 步驟：檢查工作目錄乾淨 → 改 Cargo.toml 版本 → 跑測試 → 建置 release → commit + tag + push
#       → gh release create（附上 multivoice.exe、multivoice-cli.exe）
# 需要：gh 已登入、origin 指向 GitHub。
set -euo pipefail

VER="${1:?用法：tools/release.sh <版本，例如 0.2.0> [更新說明]}"
NOTES="${2:-MultiVoice v$VER}"
VER="${VER#v}"
cd "$(dirname "$0")/.."

if [ -n "$(git status --porcelain)" ]; then
  echo "工作目錄有未提交的變更，先 commit 再發佈" >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/v$VER" >/dev/null; then
  echo "標籤 v$VER 已經存在" >&2
  exit 1
fi

# 在 commit 之前失敗（測試沒過、建置失敗…）就把版本號改回來，不留下改到一半的檔案
restore() { git checkout -- Cargo.toml Cargo.lock 2>/dev/null || true; echo "發佈中止，已還原 Cargo.toml / Cargo.lock" >&2; }
trap restore ERR

# workspace 版本（所有 crate 共用；crate 之間的 path 相依不寫版本號，所以只要改這裡）
sed -i "s/^version = \".*\"/version = \"$VER\"/" Cargo.toml

# 正在執行的 multivoice.exe 會鎖住檔案
taskkill //F //IM multivoice.exe >/dev/null 2>&1 || true

cargo test --workspace
cargo build --release -p desktop -p cli

# build 目錄（本機的 .cargo/config.toml 可能改過）；不用 python 解析，避免中文路徑的編碼問題
TARGET=$(cargo metadata --format-version 1 --no-deps | grep -o '"target_directory":"[^"]*"' | sed 's/.*:"//; s/"$//; s/\\\\/\//g')
mkdir -p dist
cp "$TARGET/release/multivoice.exe" "$TARGET/release/multivoice-cli.exe" dist/

git add Cargo.toml Cargo.lock
git diff --cached --quiet || git commit -m "發佈 v$VER"
trap - ERR
git tag -a "v$VER" -m "v$VER"
git push origin HEAD --follow-tags

gh release create "v$VER" dist/multivoice.exe dist/multivoice-cli.exe --title "v$VER" --notes "$NOTES"
echo "已發佈 v$VER"
