#!/usr/bin/env bash
# 發佈新版本到 GitHub Releases（客戶端啟動時會比對這裡的最新版本，提示使用者更新）
#
#   tools/release.sh 0.2.0 "這版的更新說明"
#
# 步驟：檢查工作目錄乾淨 → 改 Cargo.toml 版本 → 跑測試 → commit + push → 建立 Release 草稿
#       → 推送標籤 → GitHub Actions（.github/workflows/release.yml）用 MSVC 建置、附上兩個 exe、公開草稿
#       → 重新編譯並部署網頁版（tools/build-web.sh --deploy）
# exe 不在這台電腦編：GNU 工具鏈編的比較容易被 Windows Defender 誤判。
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

git add Cargo.toml Cargo.lock
git diff --cached --quiet || git commit -m "發佈 v$VER"
trap - ERR
git push origin HEAD

# 先建草稿（客戶端查的是「最新公開版」，草稿看不到），exe 附上後由 Actions 公開
gh release create "v$VER" --draft --target "$(git rev-parse HEAD)" --title "v$VER" --notes "$NOTES"
git tag -a "v$VER" -m "v$VER"
git push origin "v$VER"

echo "等 GitHub Actions 建置…"
RUN=""
for _ in $(seq 30); do
  sleep 5
  RUN=$(gh run list --workflow release.yml --event push --branch "v$VER" --limit 1 --json databaseId --jq '.[0].databaseId // empty')
  [ -n "$RUN" ] && break
done
if [ -z "$RUN" ]; then
  echo "找不到 v$VER 的建置；請到 GitHub Actions 查看。Release 目前是草稿" >&2
  exit 1
fi
gh run watch "$RUN" --exit-status --interval 30
gh release view "v$VER" --json isDraft,assets --jq '"草稿：" + (.isDraft|tostring), (.assets[] | .name + "  " + .digest)'
echo "已發佈 v$VER"

# 網頁版也用新的版本號重新編譯並部署（否則網頁版還顯示舊版本）；失敗不影響已公開的 Release
echo "部署網頁版…"
if bash tools/build-web.sh --deploy; then
  echo "網頁版已部署 v$VER"
else
  echo "網頁版部署失敗：Release 已公開，請手動跑 tools/build-web.sh --deploy" >&2
  exit 1
fi
