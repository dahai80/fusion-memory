#!/usr/bin/env bash
# 真实模型集成测试运行器。
# 全局规则: 禁 mock, 须真实加载模型; 起停 fusion-mlx 用 ~/claude-home/fusion-mlx/start.sh。
#
# 用法:
#   scripts/live-test.sh              # 全部 live 集成测试 (串行, 避 mlx 429)
#   scripts/live-test.sh fm-engine    # 单 crate
#
# 前置: fusion-mlx 已起且加载 bge-m3 + Qwen3.5-9B-4bit
#   ~/claude-home/fusion-mlx/start.sh start
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

FEATURES="--features fm-embed/mlx-live --features fm-engine/mlx-live --features fm-server/mlx-live"

# 健康检查: fusion-mlx 必须 Running + healthy
STATUS_OUT="$(~/claude-home/fusion-mlx/start.sh status 2>/dev/null)"
if ! echo "$STATUS_OUT" | sed 's/\x1b\[[0-9;]*m//g' | grep -q '"status":"healthy"'; then
    echo "[live-test] fusion-mlx 未运行或非 healthy, 先起: ~/claude-home/fusion-mlx/start.sh start" >&2
    exit 1
fi

CRATE_FILTER="${1:-}"

if [ -n "$CRATE_FILTER" ]; then
    echo "[live-test] 单 crate: $CRATE_FILTER (串行)"
    cargo test -p "$CRATE_FILTER" $FEATURES -- --include-ignored --test-threads=1
else
    echo "[live-test] 全 workspace live 测试 (串行, 避 mlx 429 rate-limit)"
    cargo test --workspace $FEATURES -- --include-ignored --test-threads=1
fi

echo "[live-test] PASS"
