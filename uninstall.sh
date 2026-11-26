#!/bin/sh
# 天翼云电脑控制中心 · 统一一键卸载引导
# 自动识别系统（OpenWrt / 普通 Linux），国内网络自动走 GitHub 镜像回退。
#
# 用法（一键）:
#   sh -c "$(wget -4 -qO- https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/uninstall.sh)"
#   sh -c "$(curl -fsSL https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/uninstall.sh)"
#   KEEP_DATA=0 sh -c "$(...)"   # 连数据一起删

REPO="likz0421/ctyun-dashboard-rs"
BRANCH="main"

log() { echo "[ctyun] $*"; }
err() { echo "[ctyun][错误] $*" >&2; }

MIRRORS="
https://raw.githubusercontent.com/${REPO}/${BRANCH}
https://ghfast.top/https://raw.githubusercontent.com/${REPO}/${BRANCH}
https://ghproxy.net/https://raw.githubusercontent.com/${REPO}/${BRANCH}
https://gh-proxy.com/https://raw.githubusercontent.com/${REPO}/${BRANCH}
https://cdn.jsdelivr.net/gh/${REPO}@${BRANCH}
"

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

fetch_into() {
  rel="$1"; out="$2"
  for base in ${MIRRORS}; do
    fetch "${base}/${rel}" > "$out" 2>/dev/null || continue
    [ -s "$out" ] || continue
    head -c 200 "$out" 2>/dev/null | grep -qiE '<html|<!DOCTYPE|404: Not Found|rate limit|too many requests' && continue
    return 0
  done
  return 1
}

if [ -f /etc/openwrt_release ] || [ -f /etc/openwrt_version ]; then
  SYS="openwrt"
elif [ -f /etc/systemd/system.conf ] || [ -x /usr/bin/systemctl ] || [ -x /bin/systemctl ]; then
  SYS="linux"
else
  SYS="linux"
fi

log "识别系统: ${SYS} -> 拉取 ${SYS}/uninstall.sh"
TMP="$(mktemp 2>/dev/null || echo "/tmp/ctyun-uninstall-$$.sh")"
if ! fetch_into "${SYS}/uninstall.sh" "$TMP"; then
  err "下载 ${SYS}/uninstall.sh 失败（已尝试原始源 + 4 个国内镜像）"
  err "可设置代理后重试：export https_proxy=http://主机:端口"
  rm -f "$TMP" 2>/dev/null
  exit 1
fi
KEEP_DATA="${KEEP_DATA:-1}" sh "$TMP" "$@"
RC=$?
rm -f "$TMP" 2>/dev/null
exit $RC
