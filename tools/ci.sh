#!/usr/bin/env bash
# 一次跑完四道闸门：单测（含并发与端到端）、发布构建、插件对账、规格一致性。
# 任何一道不过就以非零退出。CI 与本地都用它，避免两边跑的东西不一样。
set -uo pipefail
cd "$(dirname "$0")/.."

# 解释器按名字找可能落空：Windows 的 bash 里 cargo 只以 cargo.exe 出现（WSL 不做 PATHEXT）
cargo_bin="$(command -v cargo || command -v cargo.exe || true)"
node_bin="$(command -v node || command -v node.exe || true)"
if [ -z "$cargo_bin" ]; then echo "cargo not found on PATH" >&2; exit 2; fi
if [ -z "$node_bin" ]; then echo "node not found on PATH" >&2; exit 2; fi

fail=0
step() {
  echo
  echo "==> $*"
  "$@" || { echo "--- 失败：$*"; fail=1; }
}

step "$cargo_bin" test --quiet
# 发布构建：cargo test 只编 debug 配置，README 里发布的却是 release 产物。
# 插件包内的二进制由这一步产出（.gitignore 排除，按平台各自构建）。
step bash tools/build-plugin.sh
# 快照不入库：先导出一次，插件自检才有真数据可解析
step "$node_bin" tools/check-plugin.mjs
step bash tools/check-spec.sh

echo
if [ "$fail" -ne 0 ]; then
  echo "有检查未通过"
  exit 1
fi
echo "全部通过"
