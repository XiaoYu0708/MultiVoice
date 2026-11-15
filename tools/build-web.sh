#!/usr/bin/env bash
# 建置網頁版客戶端到 dist/web/（純靜態檔案）。
#
#   tools/build-web.sh            建置
#   tools/build-web.sh --deploy   建置後複製到 deploy/web/（Caddy 直接提供 https://voice.<網域>/）
#   tools/build-web.sh --serve    建置後在 http://127.0.0.1:8080/ 開本機測試伺服器（localhost 才能用麥克風）
#
# 需要：rustup target add wasm32-unknown-unknown、wasm-bindgen-cli（版本要和 Cargo.lock 的 wasm-bindgen 相同）、
# clang（LLVM，用來把 ring 與 libopus 編成 wasm）
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v clang >/dev/null 2>&1 && [ -x "/c/Program Files/LLVM/bin/clang.exe" ]; then
  export PATH="/c/Program Files/LLVM/bin:$PATH"
fi
command -v clang >/dev/null 2>&1 || { echo "找不到 clang（請安裝 LLVM）"; exit 1; }
command -v wasm-bindgen >/dev/null 2>&1 || { echo "找不到 wasm-bindgen（cargo install wasm-bindgen-cli --version <Cargo.lock 裡的版本>）"; exit 1; }

cargo build --profile wasm --target wasm32-unknown-unknown -p web -p web-audio

# build 目錄（本機的 .cargo/config.toml 可能改過）
TARGET=$(cargo metadata --format-version 1 --no-deps | grep -o '"target_directory":"[^"]*"' | sed 's/.*:"//; s/"$//; s/\\/\//g')
WASM="$TARGET/wasm32-unknown-unknown/wasm"
OUT=dist/web
# 只清內容不刪資料夾（Windows 上資料夾被測試伺服器開著時刪不掉）
mkdir -p "$OUT"
find "$OUT" -mindepth 1 -delete
mkdir -p "$OUT/pkg"
wasm-bindgen "$WASM/web.wasm" --target web --out-dir "$OUT/pkg" --no-typescript --remove-name-section --remove-producers-section
cp "$WASM/web_audio.wasm" "$OUT/"
cp crates/web/www/* "$OUT/"
# 每次建置換掉 service worker 的快取名稱
VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')
sed -i "s/multivoice-v1/multivoice-$VERSION-$(date +%s)/" "$OUT/sw.js"
echo "完成：$OUT（$(du -sh "$OUT" | cut -f1)）"
ls -la "$OUT" "$OUT/pkg"

case "${1:-}" in
  --deploy)
    mkdir -p deploy/web
    find deploy/web -mindepth 1 ! -name .gitkeep -delete
    cp -r "$OUT"/. deploy/web/
    echo "已複製到 deploy/web/"
    ;;
  --serve)
    cd "$OUT" && exec python -m http.server 8080 --bind 127.0.0.1
    ;;
esac
