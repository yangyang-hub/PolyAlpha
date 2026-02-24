#!/usr/bin/env bash
set -euo pipefail

# ── PolyAlpha Docker 部署脚本 ──
# 用法:
#   ./deploy.sh          # 拉取更新，二进制变更则重建容器
#   ./deploy.sh pull      # 仅拉取，不重启
#   ./deploy.sh restart   # 强制重建并重启
#   ./deploy.sh stop      # 停止容器
#   ./deploy.sh status    # 查看状态
#   ./deploy.sh logs      # 查看日志（docker logs -f）

# ── 配置 ──
REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$REPO_DIR/bin/polyalpha"
HASH_FILE="$REPO_DIR/.polyalpha.sha256"
LOCK_FILE="$REPO_DIR/deploy.lock"
ENV_FILE="$REPO_DIR/.env"
COMPOSE_FILE="$REPO_DIR/docker/docker-compose.yml"
GIT_BRANCH="master"
CONTAINER_NAME="polyalpha-bot"

# ── cron 兼容: PATH + 颜色 ──
export PATH="/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"
if [[ -t 1 ]]; then
    RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
else
    RED=''; GREEN=''; YELLOW=''; NC=''
fi

log()  { echo -e "${GREEN}[deploy]${NC} $*"; }
warn() { echo -e "${YELLOW}[deploy]${NC} $*"; }
err()  { echo -e "${RED}[deploy]${NC} $*" >&2; }

# ── 进程锁: 防止 cron 并发 ──
exec 9>"$LOCK_FILE"
if ! flock -n 9; then
    err "另一个 deploy 正在运行，退出"
    exit 0
fi

# ── 检查 .env ──
check_env() {
    if [[ ! -f "$ENV_FILE" ]]; then
        err ".env 文件不存在: $ENV_FILE"
        err "请创建 .env 并设置: POLYMARKET_PRIVATE_KEY=0x..."
        exit 1
    fi
    local perms
    perms=$(stat -c %a "$ENV_FILE" 2>/dev/null || stat -f %Lp "$ENV_FILE" 2>/dev/null)
    if [[ "$perms" != "600" && "$perms" != "400" ]]; then
        warn ".env 权限为 $perms，收紧为 600"
        chmod 600 "$ENV_FILE"
    fi
}

# ── 检测二进制是否变更 ──
binary_changed() {
    if [[ ! -f "$BIN" ]]; then
        err "二进制文件不存在: $BIN"
        exit 1
    fi

    local new_hash
    new_hash=$(sha256sum "$BIN" | awk '{print $1}')

    if [[ -f "$HASH_FILE" ]]; then
        local old_hash
        old_hash=$(cat "$HASH_FILE")
        if [[ "$new_hash" == "$old_hash" ]]; then
            return 1  # 未变更
        fi
    fi

    # 保存新 hash
    echo "$new_hash" > "$HASH_FILE"
    return 0  # 有变更
}

# ── Git 拉取（始终成功返回，用 GIT_UPDATED 标记是否有更新）──
GIT_UPDATED=false

do_pull() {
    log "拉取最新代码 ($GIT_BRANCH)..."
    cd "$REPO_DIR"

    # 防止 cron/其他用户执行时 git dubious ownership 报错
    git config --global --add safe.directory "$REPO_DIR" 2>/dev/null || true

    # 保存本地配置变更
    local stashed=false
    if ! git diff --quiet config/ 2>/dev/null; then
        warn "暂存本地 config/ 修改..."
        if git stash push -m "deploy-auto-stash" -- config/ 2>/dev/null; then
            stashed=true
        fi
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
        return
    fi

    git reset --hard "origin/$GIT_BRANCH"
    log "更新完成: ${local_hash:0:7} → ${remote_hash:0:7}"

    if $stashed; then
        git stash pop 2>/dev/null || warn "配置恢复冲突，请手动处理: git stash pop"
    fi

    GIT_UPDATED=true
}

# ── Docker 操作 ──
do_stop() {
    if docker ps --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$"; then
        log "停止容器 $CONTAINER_NAME..."
        docker compose -f "$COMPOSE_FILE" down --timeout 15
        log "已停止"
    else
        log "容器未运行"
    fi
}

do_build_and_start() {
    check_env

    if [[ ! -f "$BIN" ]]; then
        err "二进制不存在: $BIN"
        exit 1
    fi

    log "构建 Docker 镜像..."
    docker compose -f "$COMPOSE_FILE" build --no-cache

    log "启动容器..."
    docker compose -f "$COMPOSE_FILE" up -d

    # 等待最多 10 秒确认容器存活
    local ok=false
    for _ in $(seq 1 10); do
        sleep 1
        if docker ps --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$"; then
            ok=true
            break
        fi
    done

    if $ok; then
        log "启动成功: $(docker ps --filter "name=$CONTAINER_NAME" --format '{{.Status}}')"
    else
        err "启动失败，查看日志: docker compose -f $COMPOSE_FILE logs"
        exit 1
    fi
}

do_status() {
    if docker ps --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$"; then
        log "运行中"
        docker ps --filter "name=$CONTAINER_NAME" --format "  ID: {{.ID}}  状态: {{.Status}}  端口: {{.Ports}}"
        echo ""
        log "最近日志:"
        docker logs "$CONTAINER_NAME" --tail 5 2>&1 | sed 's/^/  /'
    else
        warn "未运行"
    fi
}

do_logs() {
    docker logs "$CONTAINER_NAME" -f 2>&1
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
        do_build_and_start
        ;;
    restart)
        do_stop
        do_build_and_start
        ;;
    status)
        do_status
        ;;
    logs)
        do_logs
        ;;
    deploy|"")
        do_pull

        if binary_changed; then
            log "检测到 bin/polyalpha 变更，重建容器..."
            do_stop
            do_build_and_start
        else
            # 二进制未变更，但容器可能没在运行
            if ! docker ps --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$"; then
                warn "容器未运行，启动中..."
                do_build_and_start
            else
                log "二进制未变更，无需操作"
            fi
        fi
        ;;
    *)
        echo "用法: $0 {deploy|pull|start|stop|restart|status|logs}"
        exit 1
        ;;
esac
