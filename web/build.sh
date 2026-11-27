#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
OUT_DIR="$SCRIPT_DIR/pkg"

echo "==> Building victr-web (wasm32-unknown-unknown, release)..."
(cd "$APP_DIR" && cargo build -p victr-web --target wasm32-unknown-unknown --release)

WASM_IN="$APP_DIR/target/wasm32-unknown-unknown/release/victr_web.wasm"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

echo "==> Running wasm-bindgen -> $OUT_DIR ..."
wasm-bindgen --target web --no-typescript --out-dir "$OUT_DIR" "$WASM_IN"

if command -v wasm-opt >/dev/null 2>&1; then
  echo "==> Optimizing WASM with wasm-opt -Oz..."
  wasm-opt -Oz "$OUT_DIR/victr_web_bg.wasm" -o "$OUT_DIR/victr_web_bg.wasm"
fi

echo "==> Bundle size:"
ls -lh "$OUT_DIR/victr_web_bg.wasm" "$OUT_DIR/victr_web.js"
if command -v gzip >/dev/null 2>&1; then
  printf "    gzipped wasm: %s\n" "$(gzip -c "$OUT_DIR/victr_web_bg.wasm" | wc -c | numfmt --to=iec-i --suffix=B 2>/dev/null || gzip -c "$OUT_DIR/victr_web_bg.wasm" | wc -c)"
fi
echo "==> Preview locally with: python3 -m http.server -d \"$SCRIPT_DIR\" 8080"
