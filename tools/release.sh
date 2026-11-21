#!/usr/bin/env bash
# 發佈新版本到 GitHub Releases（客戶端啟動時會比對這裡的最新版本，提示使用者更新）
#
#   tools/release.sh 0.2.0 "這版的更新說明"
#
# 步驟：檢查工作目錄乾淨 → 改 Cargo.toml 版本 → 跑測試 → commit + push → 建立 Release 草稿
#       → 推送標籤 → GitHub Actions（.github/workflows/release.yml）用 MSVC 建置、附上兩個 exe、公開草稿
#       → 重新編譯並部署網頁版（tools/build-web.sh --deploy）
#       → Android 版（另一個專案，預設在 ../../App開發/MultiVoice-Android，可用環境變數 MULTIVOICE_ANDROID_DIR 指定）：
#         在跑測試之後就用同一個版本號建好簽章 APK／AAB（建置失敗就不發佈），Release 公開後把 APK 附上（MultiVoice-Android.apk），
#         再 commit + push Android 專案的版本號。AAB 要另外上傳到 Play Console。真的要跳過 Android：SKIP_ANDROID=1
# exe 不在這台電腦編：GNU 工具鏈編的比較容易被 Windows Defender 誤判。
# 需要：gh 已登入、origin 指向 GitHub。
set -euo pipefail

VER="${1:?用法：tools/release.sh <版本，例如 0.2.0> [更新說明]}"
NOTES="${2:-MultiVoice v$VER}"
VER="${VER#v}"
cd "$(dirname "$0")/.."

# 每次發佈都附上「下載哪個檔案」的說明（三個檔案由 .github/workflows/release.yml 附上）
DOWNLOADS=$(cat <<'EOF'
## 下載哪個檔案？

| 檔案 | 說明 |
|---|---|
| **MultiVoice-Setup.exe** | **安裝版（第一次用推薦這個）**：安裝到電腦，開始選單會有捷徑（可以選擇建立桌面捷徑），之後可以從 Windows「設定 › 應用程式」解除安裝。安裝本身不需要系統管理員權限（開啟程式時會跳 UAC 要求管理員權限） |
| **multivoice.exe** | **免安裝版**：下載後直接執行，放在任何資料夾都可以（例如隨身碟），不用安裝也不會留下捷徑。不想用了直接刪掉檔案就好 |
| **MultiVoice-Android.apk** | **Android 版**：手機直接安裝（要允許「安裝不明來源的應用程式」）。之後有新版，App 裡會提示下載 |
| **multivoice-cli.exe** | **命令列版**：沒有視窗，給除錯、自動測試或進階使用者用，**一般人不用下載** |

- 安裝版和免安裝版的功能完全一樣，設定也共用（存在 `%APPDATA%\MultiVoice`），選一個就好
- 已經在用的人不用重新下載：程式啟動時會在畫面上方提示新版，按一下就能更新（安裝版和免安裝版都可以）
- 沒有 Windows 電腦：Android 手機裝 APK；iPhone、Mac 直接用網頁版 https://multivoice.xiaoyu0708.ip-ddns.com/ ，不用下載
EOF
)
NOTES="$NOTES

$DOWNLOADS"

if [ -n "$(git status --porcelain)" ]; then
  echo "工作目錄有未提交的變更，先 commit 再發佈" >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/v$VER" >/dev/null; then
  echo "標籤 v$VER 已經存在" >&2
  exit 1
fi

# 在 commit 之前失敗（測試沒過、建置失敗…）就把版本號改回來，不留下改到一半的檔案
restore() {
  git checkout -- Cargo.toml Cargo.lock 2>/dev/null || true
  [ -n "${ANDROID_DIR:-}" ] && git -C "$ANDROID_DIR" checkout -- version.properties 2>/dev/null || true
  echo "發佈中止，已還原 Cargo.toml / Cargo.lock（與 Android 的 version.properties）" >&2
}
trap restore ERR

# workspace 版本（所有 crate 共用；crate 之間的 path 相依不寫版本號，所以只要改這裡）
sed -i "s/^version = \".*\"/version = \"$VER\"/" Cargo.toml

# 正在執行的 multivoice.exe 會鎖住檔案
taskkill //F //IM multivoice.exe >/dev/null 2>&1 || true

cargo test --workspace

# Android 版：先建好（建置失敗就不發佈；三種 CPU 架構 + LTO，要幾分鐘）
ANDROID_DIR="${MULTIVOICE_ANDROID_DIR:-../../App開發/MultiVoice-Android}"
APK=""
if [ "${SKIP_ANDROID:-}" != 1 ]; then
  if [ ! -x "$ANDROID_DIR/tools/build-release.sh" ]; then
    echo "找不到 Android 專案（$ANDROID_DIR）；要跳過 Android 版請設 SKIP_ANDROID=1" >&2
    false
  fi
  bash "$ANDROID_DIR/tools/build-release.sh" "$VER"
  APK="$ANDROID_DIR/dist/MultiVoice-$VER.apk"
  [ -f "$APK" ] || { echo "沒有產生 $APK" >&2; false; }
fi

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
  # 用 commit 篩選：同一個版本號取消後重發時，--branch 會抓到之前被取消的那次
  RUN=$(gh run list --workflow release.yml --event push --branch "v$VER" --commit "$(git rev-parse HEAD)" --limit 1 --json databaseId --jq '.[0].databaseId // empty')
  [ -n "$RUN" ] && break
done
if [ -z "$RUN" ]; then
  echo "找不到 v$VER 的建置；請到 GitHub Actions 查看。Release 目前是草稿" >&2
  exit 1
fi
gh run watch "$RUN" --exit-status --interval 30
gh release view "v$VER" --json isDraft,assets --jq '"草稿：" + (.isDraft|tostring), (.assets[] | .name + "  " + .digest)'
echo "已發佈 v$VER"

# Android：APK 附到 Release（固定檔名，App 的更新檢查找 .apk），版本號 commit 到 Android 專案
if [ -n "$APK" ]; then
  cp "$APK" "$ANDROID_DIR/dist/MultiVoice-Android.apk"
  gh release upload "v$VER" "$ANDROID_DIR/dist/MultiVoice-Android.apk" --clobber
  git -C "$ANDROID_DIR" add version.properties
  git -C "$ANDROID_DIR" diff --cached --quiet || git -C "$ANDROID_DIR" commit -m "發佈 v$VER"
  git -C "$ANDROID_DIR" push origin HEAD || echo "Android 專案 push 失敗，請手動 push" >&2
  echo "Android：APK 已附到 Release；上架用的 AAB 在 $ANDROID_DIR/dist/MultiVoice-$VER.aab（要上傳到 Play Console）"
fi

# 網頁版也用新的版本號重新編譯並部署（否則網頁版還顯示舊版本）；失敗不影響已公開的 Release
echo "部署網頁版…"
if bash tools/build-web.sh --deploy; then
  echo "網頁版已部署 v$VER"
else
  echo "網頁版部署失敗：Release 已公開，請手動跑 tools/build-web.sh --deploy" >&2
  exit 1
fi
