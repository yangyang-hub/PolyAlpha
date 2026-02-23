#!/usr/bin/env bash
set -euo pipefail

# ── PolyAlpha 云服务器部署脚本 ──
# 用法:
#   ./deploy.sh          # 拉取更新 + 重启
#   ./deploy.sh pull      # 仅拉取，不重启
#   ./deploy.sh restart   # 仅重启，不拉取
#   ./deploy.sh stop      # 停止
#   ./deploy.sh status    # 查看状态
#   ./deploy.sh logs      # 查看日志（tail -f）

# ── 配置 ──
REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$REPO_DIR/bin/polyalpha"
PID_FILE="$REPO_DIR/polyalpha.pid"
LOG_FILE="$REPO_DIR/polyalpha.log"
ENV_FILE="$REPO_DIR/.env"
GIT_BRANCH="master"

# ── 颜色 ──
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[deploy]${NC} $*"; }
warn() { echo -e "${YELLOW}[deploy]${NC} $*"; }
err()  { echo -e "${RED}[deploy]${NC} $*" >&2; }

# ── 检查 .env ──
check_env() {
    if [[ ! -f "$ENV_FILE" ]]; then
        err ".env 文件不存在: $ENV_FILE"
        err "请创建 .env 并设置: POLYMARKET_PRIVATE_KEY=0x..."
        exit 1
    fi
}

# ── 获取 PID ──
get_pid() {
    if [[ -f "$PID_FILE" ]]; then
        local pid
        pid=$(cat "$PID_FILE")
        if kill -0 "$pid" 2>/dev/null; then
            echo "$pid"
            return 0
        fi
        rm -f "$PID_FILE"
    fi
    return 1
}

# ── Git 拉取 ──
do_pull() {
    log "拉取最新代码 ($GIT_BRANCH)..."
    cd "$REPO_DIR"

    # 保存本地配置变更
    local stashed=false
    if ! git diff --quiet config/ .env 2>/dev/null; then
        warn "暂存本地配置修改..."
        git stash push -m "deploy-auto-stash" -- config/ 2>/dev/null && stashed=true
    fi

    git fetch origin "$GIT_BRANCH"
    local local_hash remote_hash
    local_hash=$(git rev-parse HEAD)
    remote_hash=$(git rev-parse "origin/$GIT_BRANCH")

    if [[ "$local_hash" == "$remote_hash" ]]; then
        log "已是最新版本: ${local_hash:0:7}"
        if $stashed; then
            git stash pop 2>/dev/null || true
        fi
        return 1  # 无更新
    fi

    git pull origin "$GIT_BRANCH"
    log "更新完成: ${local_hash:0:7} → ${remote_hash:0:7}"

    # 恢复本地配置
    if $stashed; then
        git stash pop 2>/dev/null || warn "配置恢复冲突，请手动处理: git stash pop"
    fi

    # 检查二进制
    if [[ ! -x "$BIN" ]]; then
        err "二进制文件不存在或无执行权限: $BIN"
        exit 1
    fi

    chmod +x "$BIN"
    log "二进制就绪: $(ls -lh "$BIN" | awk '{print $5}')"
    return 0  # 有更新
}

# ── 停止 ──
do_stop() {
    local pid
    if pid=$(get_pid); then
        log "停止 polyalpha (PID: $pid)..."
        kill "$pid"
        # 等待优雅退出（最多 15 秒）
        local waited=0
        while kill -0 "$pid" 2>/dev/null && (( waited < 15 )); do
            sleep 1
            (( waited++ ))
        done
        if kill -0 "$pid" 2>/dev/null; then
            warn "优雅退出超时，强制终止..."
            kill -9 "$pid" 2>/dev/null || true
        fi
        rm -f "$PID_FILE"
        log "已停止"
    else
        log "未运行"
    fi
}

# ── 启动 ──
do_start() {
    check_env

    if pid=$(get_pid); then
        err "已在运行 (PID: $pid)，请先 stop"
        exit 1
    fi

    if [[ ! -x "$BIN" ]]; then
        err "二进制不存在: $BIN"
        exit 1
    fi

    log "启动 polyalpha..."
    cd "$REPO_DIR"
    nohup "$BIN" >> "$LOG_FILE" 2>&1 &
    local pid=$!
    echo "$pid" > "$PID_FILE"
    sleep 1

    if kill -0 "$pid" 2>/dev/null; then
        log "启动成功 (PID: $pid)"
        log "日志: $LOG_FILE"
    else
        err "启动失败，查看日志: tail -50 $LOG_FILE"
        rm -f "$PID_FILE"
        exit 1
    fi
}

# ── 状态 ──
do_status() {
    local pid
    if pid=$(get_pid); then
        log "运行中 (PID: $pid)"
        # 显示资源占用
        ps -p "$pid" -o pid,vsz,rss,%cpu,etime --no-headers 2>/dev/null | \
            awk '{printf "  PID: %s  VSZ: %.0fMB  RSS: %.0fMB  CPU: %s  运行时间: %s\n", $1, $2/1024, $3/1024, $4, $5}'
        # 显示最近几行日志
        echo ""
        log "最近日志:"
        tail -5 "$LOG_FILE" 2>/dev/null | sed 's/^/  /'
    else
        warn "未运行"
    fi
}

# ── 日志 ──
do_logs() {
    if [[ -f "$LOG_FILE" ]]; then
        tail -f "$LOG_FILE"
    else
        err "日志文件不存在: $LOG_FILE"
    fi
}

# ── 主流程 ──
case "${1:-deploy}" in
    pull)
        do_pull
        ;;
    stop)
        do_stop
        ;;
    start)
        do_start
        ;;
    restart)
        do_stop
        do_start
        ;;
    status)
        do_status
        ;;
    logs)
        do_logs
        ;;
    deploy|"")
        if do_pull; then
            # 有更新，重启
            do_stop
            do_start
        else
            # 无更新
            if ! get_pid >/dev/null; then
                warn "进程未运行，启动中..."
                do_start
            else
                log "无需操作"
            fi
        fi
        ;;
    *)
        echo "用法: $0 {deploy|pull|start|stop|restart|status|logs}"
        exit 1
        ;;
esac
