#!/bin/sh
# 天翼云电脑控制中心 · 统一一键入口（安装 / 卸载 / 状态）
# 自动识别系统（OpenWrt / 普通 Linux）与架构，国内网络自动走 GitHub 镜像回退。
#
# 用法一（交互式菜单，推荐）:
#   sh -c "$(wget -4 -qO- https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/install.sh)"
#   sh -c "$(curl -fsSL https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/install.sh)"
#   -> 终端下弹出菜单：1 安装/重装  2 卸载(保留数据)  3 彻底卸载  4 状态  5 退出
#
# 用法二（参数直跑，非交互，适合自动化）:
#   .../install.sh install          # 安装/重装
#   .../install.sh uninstall        # 卸载（保留数据）
#   .../install.sh uninstall-all    # 彻底卸载（含数据）
#   .../install.sh status           # 查看服务状态
#   管道方式 wget -qO- URL | sh -s install  也支持
#
# 无参数且非交互（管道 | sh）时，默认执行安装（向后兼容旧用法）。

REPO="likz0421/ctyun-dashboard-rs"
BRANCH="main"

log()  { echo "[ctyun] $*"; }
err()  { echo "[ctyun][错误] $*" >&2; }
line() { echo "--------------------------------------------"; }

# ---- 下载源（原始 + 国内镜像，按顺序回退） ----
MIRRORS="
https://raw.githubusercontent.com/${REPO}/${BRANCH}
https://ghfast.top/https://raw.githubusercontent.com/${REPO}/${BRANCH}
https://ghproxy.net/https://raw.githubusercontent.com/${REPO}/${BRANCH}
https://gh-proxy.com/https://raw.githubusercontent.com/${REPO}/${BRANCH}
https://cdn.jsdelivr.net/gh/${REPO}@${BRANCH}
"

# 单 URL 下载到 stdout：wget -4 优先，逐级回退；带超时防止弱网卡死
fetch() {
  url="$1"
  if command -v wget >/dev/null 2>&1; then
    wget -4 -q -T 20 -O - "$url" 2>/dev/null && return 0
    wget -q -T 20 -O - "$url" 2>/dev/null && return 0
  fi
  if command -v curl >/dev/null 2>&1; then
    curl -4 -fsSL --max-time 20 "$url" 2>/dev/null && return 0
    curl -fsSL --max-time 20 "$url" 2>/dev/null && return 0
  fi
  return 1
}

# 下载仓库内相对路径脚本到 out；返回成功用的是哪个镜像
fetch_into() {
  rel="$1"; out="$2"; used=""
  for base in ${MIRRORS}; do
    fetch "${base}/${rel}" > "$out" 2>/dev/null || continue
    [ -s "$out" ] || continue
    # 排除镜像返回的 HTML 错误页 / 限流页
    head -c 200 "$out" 2>/dev/null | grep -qiE '<html|<!DOCTYPE|404: Not Found|rate limit|too many requests' && continue
    used="$base"; return 0
  done
  return 1
}

# ---- 1. 识别系统 / 架构 ----
if [ -f /etc/openwrt_release ] || [ -f /etc/openwrt_version ]; then
  SYS="openwrt"
  SYS_NAME="$(. /etc/openwrt_release 2>/dev/null; echo "${DISTRIB_DESCRIPTION:-OpenWrt}")"
elif [ -f /etc/systemd/system.conf ] || [ -x /usr/bin/systemctl ] || [ -x /bin/systemctl ]; then
  SYS="linux"
  SYS_NAME="Linux (systemd)"
else
  SYS="linux"
  SYS_NAME="Linux"
fi
ARCH="$(uname -m 2>/dev/null)"

# ---- 2. 确定动作：参数 > 交互菜单 > 默认安装 ----
ACTION="${1:-}"
KEEP_DATA="${KEEP_DATA:-1}"

if [ -z "$ACTION" ]; then
  if [ -t 0 ]; then
    line
    log "天翼云电脑控制中心 · 一键管理"
    log "识别系统: ${SYS_NAME}  (${ARCH})"
    line
    echo "  1) 安装 / 重装（保留数据）"
    echo "  2) 卸载（保留账号与数据）"
    echo "  3) 彻底卸载（同时删除数据）"
    echo "  4) 查看服务状态"
    echo "  5) 退出"
    line
    printf "请选择 [1-5]: "
    read -r choice
    case "$choice" in
      1) ACTION="install" ;;
      2) ACTION="uninstall";       KEEP_DATA=1 ;;
      3) ACTION="uninstall";       KEEP_DATA=0 ;;
      4) ACTION="status" ;;
      *) log "已退出"; exit 0 ;;
    esac
  else
    log "非交互模式，默认执行安装"
    ACTION="install"
  fi
fi

# 参数归一化
case "$ACTION" in
  uninstall-all) ACTION="uninstall"; KEEP_DATA=0 ;;
  uninstall)     KEEP_DATA=1 ;;
esac

# ---- 3. 查看状态 ----
if [ "$ACTION" = "status" ]; then
  line
  log "服务状态"
  if [ "$SYS" = "openwrt" ]; then
    /etc/init.d/ctyun-dashboard status 2>&1 || true
    ps 2>/dev/null | grep '[c]tyun-dashboard' || true
    uci show ctyun_dashboard 2>/dev/null || true
  else
    systemctl status ctyun-dashboard --no-pager 2>&1 | head -20 || true
    grep -h CTYUN_PORT /etc/ctyun-dashboard/ctyun-dashboard.env 2>/dev/null || true
  fi
  line
  exit 0
fi

# ---- 4. 拉取对应脚本并执行 ----
TMP="$(mktemp 2>/dev/null || echo "/tmp/ctyun-${ACTION}-$$.sh")"
if ! SRC_BASE="$(fetch_into "${SYS}/${ACTION}.sh" "$TMP")"; then
  err "下载 ${SYS}/${ACTION}.sh 失败（已尝试原始源 + 4 个国内镜像）"
  err "可设置代理后重试：export https_proxy=http://主机:端口"
  rm -f "$TMP" 2>/dev/null
  exit 1
fi
log "脚本来源: ${SRC_BASE}"

# 卸载动作透传 KEEP_DATA；安装动作透传其余参数（脚本自身忽略未知参数）
if [ "$ACTION" = "uninstall" ]; then
  KEEP_DATA="$KEEP_DATA" sh "$TMP" "$@"
else
  sh "$TMP" "$@"
fi
RC=$?
rm -f "$TMP" 2>/dev/null
exit $RC
