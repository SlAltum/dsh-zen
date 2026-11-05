#!/usr/bin/env bash
# 把 Rust 核心投放到 plugin/bin，让插件包自带二进制。
# 用法：tools/build-plugin.sh [--debug|--release]，缺省 --release。
set -euo pipefail
cd "$(dirname "$0")/.."

profile="${1:---release}"
case "$profile" in
  --debug)   cargo_args="" ;               outdir="target/debug" ;;
  --release) cargo_args="--release" ;      outdir="target/release" ;;
  *) echo "unknown profile: $profile (use --debug or --release)" >&2; exit 2 ;;
esac

# Windows 的 bash 里 cargo 可能只以 cargo.exe 出现（WSL 不做 PATHEXT 补全）
cargo_bin="$(command -v cargo || command -v cargo.exe || true)"
if [ -z "$cargo_bin" ]; then echo "cargo not found on PATH" >&2; exit 2; fi

"$cargo_bin" build $cargo_args

# 用产物是否存在判断后缀：$OS 在 WSL/Git Bash 里都不一定靠得住
exe="dsh-zen"
if [ -f "$outdir/dsh-zen.exe" ]; then exe="dsh-zen.exe"; fi

mkdir -p plugin/bin
cp "$outdir/$exe" "plugin/bin/$exe"
echo "plugin/bin/$exe: $(wc -c < "plugin/bin/$exe" | tr -d ' ') bytes"
