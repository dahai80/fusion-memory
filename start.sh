#!/bin/bash
# fusion-memory 服务生命周期管理 (start|stop|restart|status|log|doctor)
# PRD §11.2。UDS JSON-RPC + HTTP(Bearer)。
#
# 用法:
#   ./start.sh start    # 启 fm-server (默认真 bge-m3; FUSION_MEMORY_STUB=1 离线)
#   ./start.sh stop     # 优雅停 (SIGTERM)
#   ./start.sh restart
#   ./start.sh status   # PID/端口/sock/内存
#   ./start.sh log [-f] # tail 日志
#   ./start.sh doctor   # 健康检查 (端口/sock/mlx 连通)
#
# env 覆盖 (见 ServerConfig::from_env):
#   FM_HOME (默认 ~/.fusion-memory)
#   FUSION_MEMORY_SOCK / FUSION_MEMORY_HTTP_PORT (默认 11435) / FUSION_MEMORY_API_KEY (HTTP 必配)
#   FUSION_MEMORY_DIM (默认 1024) / FUSION_MEMORY_STUB
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

FM_HOME="${FM_HOME:-$HOME/.fusion-memory}"
mkdir -p "$FM_HOME"
PID_FILE="$FM_HOME/fm-server.pid"
LOG_DIR="$FM_HOME/logs"
STDOUT_LOG="$LOG_DIR/stdout.log"
STDERR_LOG="$LOG_DIR/stderr.log"
HTTP_PORT="${FUSION_MEMORY_HTTP_PORT:-11435}"
SOCK="${FUSION_MEMORY_SOCK:-$FM_HOME/fusion-memory.sock}"
BIN="$SCRIPT_DIR/target/release/fm-server"
ALT_BIN="$SCRIPT_DIR/target/debug/fm-server"

# 颜色
C_BLUE=$'\033[0;34m'
C_GREEN=$'\033[0;32m'
C_RED=$'\033[0;31m'
C_YELLOW=$'\033[0;33m'
C_RESET=$'\033[0m'

resolve_bin() {
    if [[ -x "$BIN" ]]; then
        echo "$BIN"
    elif [[ -x "$ALT_BIN" ]]; then
        echo "$ALT_BIN"
    else
        echo ""
    fi
}

is_running() {
    [[ -f "$PID_FILE" ]] || return 1
    local pid
    pid="$(cat "$PID_FILE" 2>/dev/null || echo "")"
    [[ -n "$pid" ]] || return 1
    kill -0 "$pid" 2>/dev/null
}

cmd_start() {
    if is_running; then
        echo "${C_YELLOW}● Already running${C_RESET} PID=$(cat "$PID_FILE")"
        return 0
    fi
    local b
    b="$(resolve_bin)"
    if [[ -z "$b" ]]; then
        echo "${C_RED}✘ fm-server binary not found. Build first:${C_RESET} cargo build -p fm-server --release" >&2
        exit 1
    fi
    # 端口占用检查 (lsof)
    if command -v lsof >/dev/null 2>&1; then
        if lsof -iTCP:"$HTTP_PORT" -sTCP:LISTEN -P -n >/dev/null 2>&1; then
            echo "${C_RED}✘ port $HTTP_PORT already in use${C_RESET}" >&2
            exit 1
        fi
    fi
    # 残留 sock 清理
    if [[ -e "$SOCK" ]]; then
        rm -f "$SOCK"
    fi
    mkdir -p "$LOG_DIR"
    echo "${C_BLUE}━━━ Starting fusion-memory ━━━${C_RESET}"
    echo "  bin:  $b"
    echo "  home: $FM_HOME"
    echo "  http: 127.0.0.1:$HTTP_PORT  sock: $SOCK"
    nohup "$b" >>"$STDOUT_LOG" 2>>"$STDERR_LOG" &
    local pid=$!
    echo "$pid" >"$PID_FILE"
    # 等就绪 (最多 5s): sock 出现或 healthz 200
    local i
    for ((i = 0; i < 50; i++)); do
        if [[ -e "$SOCK" ]] || curl -sf "http://127.0.0.1:$HTTP_PORT/healthz" >/dev/null 2>&1; then
            echo "${C_GREEN}● Running${C_RESET} PID=$pid"
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "${C_RED}✘ process exited early, see $STDERR_LOG${C_RESET}" >&2
            rm -f "$PID_FILE"
            exit 1
        fi
        sleep 0.1
    done
    echo "${C_YELLOW}● Started (readiness not confirmed in 5s)${C_RESET} PID=$pid  log: $STDERR_LOG"
}

cmd_stop() {
    if ! is_running; then
        echo "${C_YELLOW}● Not running${C_RESET}"
        rm -f "$PID_FILE" "$SOCK"
        return 0
    fi
    local pid
    pid="$(cat "$PID_FILE")"
    echo "${C_BLUE}━━━ Stopping fusion-memory ━━━${C_RESET} PID=$pid"
    kill -TERM "$pid" 2>/dev/null || true
    local i
    for ((i = 0; i < 30; i++)); do
        if ! kill -0 "$pid" 2>/dev/null; then
            break
        fi
        sleep 0.1
    done
    if kill -0 "$pid" 2>/dev/null; then
        echo "${C_RED}✘ graceful stop timeout, SIGKILL${C_RESET}" >&2
        kill -KILL "$pid" 2>/dev/null || true
    fi
    rm -f "$PID_FILE" "$SOCK"
    echo "${C_GREEN}● Stopped${C_RESET}"
}

cmd_status() {
    echo "${C_BLUE}━━━ Fusion-Memory Status ━━━${C_RESET}"
    if is_running; then
        local pid
        pid="$(cat "$PID_FILE")"
        local mem=""
        if command -v ps >/dev/null 2>&1; then
            mem="$(ps -o rss= -p "$pid" 2>/dev/null | awk '{printf "%.1f MB", $1/1024}')"
        fi
        echo "${C_GREEN}● Running${C_RESET} PID=$pid  MEM=${mem:-?}"
    else
        echo "${C_RED}○ Not running${C_RESET}"
    fi
    echo "  home: $FM_HOME"
    echo "  http: 127.0.0.1:$HTTP_PORT"
    echo "  sock: $SOCK  ($([[ -e "$SOCK" ]] && echo present || echo absent))"
    if is_running && curl -sf "http://127.0.0.1:$HTTP_PORT/healthz" >/dev/null 2>&1; then
        echo "  healthz: ${C_GREEN}ok${C_RESET}"
    elif is_running; then
        echo "  healthz: ${C_YELLOW}no response${C_RESET}"
    fi
}

cmd_log() {
    if [[ ! -f "$STDERR_LOG" ]]; then
        echo "no log: $STDERR_LOG"
        return 1
    fi
    tail -n 200 "$STDERR_LOG"
}

cmd_doctor() {
    echo "${C_BLUE}━━━ Fusion-Memory Doctor ━━━${C_RESET}"
    local ok=1
    # 1. binary
    local b
    b="$(resolve_bin)"
    if [[ -n "$b" ]]; then
        echo "  binary: ${C_GREEN}ok${C_RESET} ($b)"
    else
        echo "  binary: ${C_RED}missing${C_RESET} (cargo build -p fm-server --release)"
        ok=0
    fi
    # 2. running
    if is_running; then
        echo "  process: ${C_GREEN}running${C_RESET} PID=$(cat "$PID_FILE")"
    else
        echo "  process: ${C_YELLOW}stopped${C_RESET}"
    fi
    # 3. http port
    if curl -sf "http://127.0.0.1:$HTTP_PORT/healthz" >/dev/null 2>&1; then
        echo "  http :11435: ${C_GREEN}healthz ok${C_RESET}"
    else
        echo "  http :$HTTP_PORT: ${C_YELLOW}no response${C_RESET} (HTTP 可能未启或需 FUSION_MEMORY_API_KEY)"
    fi
    # 4. mlx 连通性 (非 stub 模式依赖)
    local mlx_url="${FUSION_MLX_URL:-http://127.0.0.1:11434/v1}"
    if curl -sf "${mlx_url%/v1}/health" >/dev/null 2>&1 || curl -sf "${mlx_url}/models" -H "Authorization: Bearer ${FUSION_MEMORY_MLX_API_KEY:-dahai168}" >/dev/null 2>&1; then
        echo "  mlx $mlx_url: ${C_GREEN}reachable${C_RESET}"
    else
        echo "  mlx $mlx_url: ${C_RED}unreachable${C_RESET} (非 stub 模式需起 fusion-mlx)"
        ok=0
    fi
    # 5. data dir
    if [[ -d "$FM_HOME" ]]; then
        echo "  data dir: ${C_GREEN}ok${C_RESET} ($FM_HOME)"
    else
        echo "  data dir: ${C_YELLOW}absent${C_RESET} (start 时创建)"
    fi
    if [[ $ok -eq 1 ]]; then
        echo "${C_GREEN}● healthy${C_RESET}"
    else
        echo "${C_RED}✘ issues found${C_RESET}"
        exit 1
    fi
}

# P0-1: 装/卸载进程守护单元 (systemd Linux / launchd macOS)。崩溃自动重启。
cmd_install() {
    local unit_src="$SCRIPT_DIR/deploy/fusion-memory.service"
    local plist_src="$SCRIPT_DIR/deploy/io.fusion.memory.plist"
    if [[ "$(uname -s)" == "Darwin" ]]; then
        if [[ ! -f "$plist_src" ]]; then
            echo "${C_RED}✘ plist missing: $plist_src${C_RESET}" >&2
            exit 1
        fi
        local dst_dir="$HOME/Library/LaunchAgents"
        mkdir -p "$dst_dir"
        local dst="$dst_dir/io.fusion.memory.plist"
        cp "$plist_src" "$dst"
        # ~ 在 plist 内不展开为绝对路径, launchd 需绝对路径。sed 替换。
        local home_abs
        home_abs="$(cd "$HOME" && pwd)"
        sed -i '' "s|~/.fusion-memory|$FM_HOME|g" "$dst"
        sed -i '' "s|~/fusion-memory/target/release/fm-server|$BIN|g" "$dst"
        launchctl unload "$dst" 2>/dev/null || true
        launchctl load "$dst"
        echo "${C_GREEN}● installed launchd agent${C_RESET} $dst"
        echo "  KeepAlive=true (崩溃自动拉起), RunAtLoad=true (开机自启)"
        echo "  log: $FM_HOME/logs/launchd-stderr.log"
    elif command -v systemctl >/dev/null 2>&1; then
        if [[ ! -f "$unit_src" ]]; then
            echo "${C_RED}✘ unit missing: $unit_src${C_RESET}" >&2
            exit 1
        fi
        local dst="/etc/systemd/system/fusion-memory.service"
        echo "${C_BLUE}━━━ Installing systemd unit (needs sudo) ━━━${C_RESET}"
        sudo cp "$unit_src" "$dst"
        # 二进制路径按实际 build 产物改。
        sudo sed -i "s|/usr/local/bin/fm-server|$BIN|g" "$dst"
        sudo sed -i "s|/var/lib/fusion-memory|$FM_HOME|g" "$dst"
        sudo systemctl daemon-reload
        sudo systemctl enable --now fusion-memory
        echo "${C_GREEN}● installed systemd unit${C_RESET} $dst"
        echo "  Restart=always RestartSec=5s (崩溃 5s 内拉起)"
        echo "  status: systemctl status fusion-memory | log: journalctl -u fusion-memory -f"
    else
        echo "${C_RED}✘ neither systemd nor launchd found on this system${C_RESET}" >&2
        echo "  fallback: run './start.sh start' manually (no auto-restart guard)" >&2
        exit 1
    fi
}

cmd_uninstall() {
    if [[ "$(uname -s)" == "Darwin" ]]; then
        local dst="$HOME/Library/LaunchAgents/io.fusion.memory.plist"
        if [[ -f "$dst" ]]; then
            launchctl unload "$dst" 2>/dev/null || true
            rm -f "$dst"
            echo "${C_GREEN}● uninstalled launchd agent${C_RESET} $dst"
        else
            echo "${C_YELLOW}● no launchd agent installed${C_RESET}"
        fi
    elif command -v systemctl >/dev/null 2>&1; then
        local dst="/etc/systemd/system/fusion-memory.service"
        echo "${C_BLUE}━━━ Uninstalling systemd unit (needs sudo) ━━━${C_RESET}"
        sudo systemctl disable --now fusion-memory 2>/dev/null || true
        sudo rm -f "$dst"
        sudo systemctl daemon-reload
        echo "${C_GREEN}● uninstalled systemd unit${C_RESET} $dst"
    else
        echo "${C_YELLOW}● nothing to uninstall (no systemd/launchd)${C_RESET}"
    fi
}

case "${1:-}" in
start) cmd_start ;;
stop) cmd_stop ;;
restart) cmd_stop; cmd_start ;;
status) cmd_status ;;
log) shift; cmd_log "$@" ;;
doctor) cmd_doctor ;;
install) shift; cmd_install "$@" ;;
uninstall) shift; cmd_uninstall "$@" ;;
*)
    echo "usage: $0 {start|stop|restart|status|log [-f]|doctor|install|uninstall}" >&2
    exit 1
    ;;
esac
