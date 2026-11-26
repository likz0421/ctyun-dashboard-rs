#!/bin/bash
# 天翼云电脑控制中心 · Linux 一键安装脚本（systemd）
# 用法（root 执行）:
#   bash install.sh                       # 默认端口 8571
#   CTYUN_PORT=9000 bash install.sh       # 指定端口
#   CTYUN_BIN=/path/to/bin bash install.sh# 指定本地二进制
#
# 二进制来源优先级：CTYUN_BIN > 脚本同目录 ctyun-dashboard-rs >
#                  源码仓库 target/release > GitHub Release（musl 静态，自动选架构）

set +e

log()  { echo "[ctyun] $*"; }
err()  { echo "[ctyun][错误] $*" >&2; }
warn() { echo "[ctyun][警告] $*" >&2; }
line() { echo "--------------------------------------------"; }
die()  { err "$*"; exit 1; }

REPO="likz0421/ctyun-dashboard-rs"
RELEASE="https://github.com/${REPO}/releases/latest/download"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

INSTALL_DIR="/opt/ctyun-dashboard"
BIN="${INSTALL_DIR}/ctyun-dashboard"
DATA_DIR="/var/lib/ctyun-dashboard"
CONF_DIR="/etc/ctyun-dashboard"
ENV_FILE="${CONF_DIR}/ctyun-dashboard.env"
UNIT="/etc/systemd/system/ctyun-dashboard.service"
SERVICE="ctyun-dashboard"
USER_NAME="ctyun-dashboard"
PORT="${CTYUN_PORT:-8571}"
PORT="${PORT:-8571}"

line
log "开始安装 天翼云电脑控制中心（Linux / systemd）"

# ---------- 1. 环境检查 ----------
[ "$(id -u)" -eq 0 ] || die "请以 root 执行（sudo bash install.sh）"
command -v systemctl >/dev/null 2>&1 || die "未检测到 systemd（systemctl 不存在），本脚本仅支持 systemd 系统"
command -v curl >/dev/null 2>&1 || die "未检测到 curl，请先安装"

# 架构识别（统一使用 musl 静态版，glibc 系统同样可运行）
ARCH_RAW="$(uname -m 2>/dev/null | tr '[:upper:]' '[:lower:]')"
case "$ARCH_RAW" in
  x86_64|amd64)   ARCH="x86_64" ;;
  aarch64|arm64)  ARCH="aarch64" ;;
  armv7l|armv7|armhf) ARCH="armv7" ;;
  *) ARCH="" ;;
esac
log "架构        : ${ARCH_RAW:-未知}"
log "安装目录    : ${INSTALL_DIR}"
log "数据目录    : ${DATA_DIR}"
log "配置目录    : ${CONF_DIR}"

# ---------- 2. 获取二进制 ----------
BIN_SRC=""
if [ -n "$CTYUN_BIN" ] && [ -x "$CTYUN_BIN" ]; then
  BIN_SRC="$CTYUN_BIN"
  log "二进制来源  : 环境变量 CTYUN_BIN"
elif [ -x "${SCRIPT_DIR}/ctyun-dashboard-rs" ]; then
  BIN_SRC="${SCRIPT_DIR}/ctyun-dashboard-rs"
  log "二进制来源  : 脚本同目录"
elif [ -x "${SCRIPT_DIR}/../target/release/ctyun-dashboard-rs" ]; then
  BIN_SRC="${SCRIPT_DIR}/../target/release/ctyun-dashboard-rs"
  log "二进制来源  : 源码编译产物"
fi

if [ -z "$BIN_SRC" ]; then
  [ -n "$ARCH" ] || die "不支持的架构 ${ARCH_RAW}，请自行编译或指定 CTYUN_BIN"
  FNAME="ctyun-dashboard-${ARCH}-musl"
  TMP="$(mktemp -d)"
  log "从 GitHub Release 下载 ${FNAME}（国内自动走镜像回退）..."

  # 下载源：官方 Release 优先，失败依次走国内镜像；也尊重系统已有代理环境变量
  BIN_URLS="
${RELEASE}/${FNAME}
https://ghfast.top/${RELEASE}/${FNAME}
https://ghproxy.net/${RELEASE}/${FNAME}
https://gh-proxy.com/${RELEASE}/${FNAME}
"
  MIN_SIZE=1048576
  DL_OK=0
  for url in ${BIN_URLS}; do
    log "  源: ${url}"
    curl -4 -fL --connect-timeout 12 --max-time 180 -o "${TMP}/${FNAME}" "$url" 2>/dev/null \
      || curl -fL --connect-timeout 12 --max-time 180 -o "${TMP}/${FNAME}" "$url" 2>/dev/null \
      || continue
    sz=$(wc -c < "${TMP}/${FNAME}" 2>/dev/null)
    if [ "${sz:-0}" -lt "$MIN_SIZE" ]; then
      log "    返回内容过小（${sz:-0} 字节），切换下一个源"
      continue
    fi
    DL_OK=1
    break
  done
  [ "$DL_OK" = "1" ] || die "所有下载源均失败。可设置代理后重试：export https_proxy=http://主机:端口；或到 Releases 手动下载 ${FNAME}"

  # 可选 MD5 校验（同样多源回退；拿不到校验文件就跳过，不阻断安装）
  for murl in "${RELEASE}/${FNAME}.md5" "https://ghfast.top/${RELEASE}/${FNAME}.md5" "https://ghproxy.net/${RELEASE}/${FNAME}.md5"; do
    curl -4 -fsL --connect-timeout 10 --max-time 30 -o "${TMP}/${FNAME}.md5" "$murl" 2>/dev/null && break
  done
  if [ -s "${TMP}/${FNAME}.md5" ]; then
    EXPECT="$(awk '{print $1}' "${TMP}/${FNAME}.md5" | head -1)"
    ACTUAL="$(md5sum "${TMP}/${FNAME}" | awk '{print $1}')"
    if [ -n "$EXPECT" ] && [ "$EXPECT" != "$ACTUAL" ]; then
      die "MD5 校验失败（期望 ${EXPECT}，实际 ${ACTUAL}）"
    fi
    log "MD5 校验    : 通过"
  else
    warn "未取到 MD5 校验文件，跳过校验（二进制来自可信源）"
  fi
  BIN_SRC="${TMP}/${FNAME}"
fi

# ---------- 2.5 重装预处理：释放被自身旧实例占用的端口 ----------
# 升级/重装场景下，占用目标端口（如 8571）的往往就是本项目的旧实例——可能是
# systemd 服务，也可能是早期手动启动、不受 systemd 管理的裸进程。若不停掉，
# 下面的端口检查会误判为"被其他程序占用"而错误顺延到 8572。
# 这里先停 systemd 服务，再按端口精确定位并清理所有 ctyun 残留进程；
# 若端口被真正的其他程序占用，仍由下面的逻辑顺延。

# 先停 systemd 服务
if [ -f "$UNIT" ] || systemctl is-enabled "$SERVICE" >/dev/null 2>&1; then
  if systemctl is-active --quiet "$SERVICE" 2>/dev/null; then
    log "检测到旧服务正在运行，先停止以释放端口 ${PORT} ..."
    systemctl stop "$SERVICE" >/dev/null 2>&1 || warn "停止旧服务失败"
    sleep 2
  fi
fi

# 再按目标端口找出残留的 ctyun 进程（覆盖手动启动/旧二进制等非 systemd 实例）
port_pids() {
  P="$1"; pids=""
  if command -v ss >/dev/null 2>&1; then
    pids="$(ss -ltnp 2>/dev/null | grep -oE ":${P}[[:space:]].*pid=[0-9]+" | grep -oE 'pid=[0-9]+' | cut -d= -f2 | sort -u)"
  elif command -v lsof >/dev/null 2>&1; then
    pids="$(lsof -tiTCP:"${P}" -sTCP:LISTEN 2>/dev/null)"
  fi
  echo "$pids"
}
for ppid in $(port_pids "$PORT"); do
  cmd="$(tr '\0' ' ' < "/proc/${ppid}/cmdline" 2>/dev/null)"
  case "$cmd" in
    *ctyun-dashboard*)
      log "端口 ${PORT} 被旧 ctyun 进程 (PID ${ppid}) 占用，清理以复用原端口 ..."
      kill "$ppid" 2>/dev/null || kill -9 "$ppid" 2>/dev/null || true
      ;;
    *)
      warn "端口 ${PORT} 被其他程序占用 (PID ${ppid}: ${cmd})，不做清理"
      ;;
  esac
done
[ -n "$(port_pids "$PORT")" ] && sleep 2

# ---------- 3. 端口检查（占用则顺延） ----------
port_in_use() {
  if command -v ss >/dev/null 2>&1; then
    ss -ltn 2>/dev/null | awk '{print $4}' | grep -qE ":${1}$"
  elif command -v netstat >/dev/null 2>&1; then
    netstat -ltn 2>/dev/null | awk '{print $4}' | grep -qE ":${1}$"
  else
    return 1
  fi
}
if port_in_use "$PORT"; then
  warn "端口 ${PORT} 已被占用，尝试顺延 ..."
  NEW_PORT=""
  for p in $(seq $((PORT + 1)) $((PORT + 20))); do
    if ! port_in_use "$p"; then NEW_PORT="$p"; break; fi
  done
  [ -n "$NEW_PORT" ] || die "端口 ${PORT}~$((PORT + 20)) 均被占用，请用 CTYUN_PORT 指定空闲端口"
  PORT="$NEW_PORT"
  log "改用端口    : ${PORT}"
fi

# ---------- 4. 创建用户与目录 ----------
if ! id "$USER_NAME" >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin --home-dir "$DATA_DIR" "$USER_NAME" 2>/dev/null \
    && log "已创建系统用户 ${USER_NAME}" \
    || warn "创建系统用户失败，将使用 root 运行服务"
fi
RUN_USER="$USER_NAME"
if ! id "$USER_NAME" >/dev/null 2>&1; then RUN_USER="root"; fi

mkdir -p "$INSTALL_DIR" "$DATA_DIR" "$CONF_DIR" || die "创建目录失败"

# ---------- 5. 安装二进制 ----------
cp "$BIN_SRC" "$BIN" || die "复制二进制失败"
chmod 755 "$BIN"
# 让服务用户对程序目录有写权限：在线升级时进程需在该目录写入新二进制并
# 原子 rename 覆盖自身（Linux 不能原位改写正在运行的 exe，只能同目录 rename）。
# 服务为低权限专用用户，风险可控；若回退到 root 运行则无需改动。
if [ "$RUN_USER" != "root" ] && id "$RUN_USER" >/dev/null 2>&1; then
  chown -R "${RUN_USER}:${RUN_USER}" "$INSTALL_DIR" 2>/dev/null \
    && log "已授权服务用户可升级程序（在线升级已启用）" \
    || warn "程序目录属主设置失败，在线升级可能不可用（可用一键脚本升级）"
fi

# ---------- 6. 写配置文件 ----------
cat > "$ENV_FILE" <<EOF
# 天翼云电脑控制中心运行配置（由 install.sh 生成）
CTYUN_PORT=${PORT}
CTYUN_DATA_DIR=${DATA_DIR}
RUST_LOG=info
EOF
chmod 640 "$ENV_FILE"

# ---------- 7. 写 systemd 单元 ----------
cat > "$UNIT" <<EOF
[Unit]
Description=天翼云电脑控制中心 (ctyun-dashboard-rs)
Documentation=https://github.com/${REPO}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${RUN_USER}
EnvironmentFile=${ENV_FILE}
WorkingDirectory=${DATA_DIR}
ExecStart=${BIN}
Restart=always
RestartSec=5
LimitNOFILE=65536
NoNewPrivileges=yes
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
EOF
chmod 644 "$UNIT"

# ---------- 8. 目录属主 ----------
if [ "$RUN_USER" != "root" ]; then
  chown -R "${RUN_USER}:${RUN_USER}" "$DATA_DIR" 2>/dev/null || warn "数据目录属主设置失败（不影响运行）"
fi

# ---------- 9. 启动服务 ----------
systemctl daemon-reload
systemctl enable "$SERVICE" >/dev/null 2>&1 || warn "开机自启设置失败"
systemctl restart "$SERVICE" || die "服务启动失败，请执行 journalctl -u ${SERVICE} -e 查看日志"

# ---------- 10. 健康检查 ----------
sleep 2
if curl -4 -fs --max-time 5 "http://127.0.0.1:${PORT}/" >/dev/null 2>&1; then
  HEALTH="通过"
else
  HEALTH="未通过（服务可能仍在启动，请稍后检查）"
fi

line
log "安装完成"
log "面板地址    : http://$(hostname -I 2>/dev/null | awk '{print $1}'):${PORT}"
log "本机地址    : http://127.0.0.1:${PORT}"
log "默认账号    : admin / admin123（首次登录后请立即修改）"
log "健康检查    : ${HEALTH}"
log "服务管理    : systemctl {status|restart|stop} ${SERVICE}"
log "查看日志    : journalctl -u ${SERVICE} -f"
log "卸载        : bash ${SCRIPT_DIR}/uninstall.sh"
line
