#!/bin/bash
# 天翼云电脑控制中心 · Linux 一键卸载脚本（systemd）
# 用法（root 执行）: bash uninstall.sh
#   KEEP_DATA=0 bash uninstall.sh   # 连数据目录一起删除

set +e

log() { echo "[ctyun-uninstall] $*"; }

[ "$(id -u)" -eq 0 ] || { echo "请以 root 执行" >&2; exit 1; }

SERVICE="ctyun-dashboard"
UNIT="/etc/systemd/system/ctyun-dashboard.service"
INSTALL_DIR="/opt/ctyun-dashboard"
CONF_DIR="/etc/ctyun-dashboard"
DATA_DIR="/var/lib/ctyun-dashboard"
USER_NAME="ctyun-dashboard"

log "停止并禁用服务 ..."
systemctl stop "$SERVICE" >/dev/null 2>&1 || true
systemctl disable "$SERVICE" >/dev/null 2>&1 || true

log "删除 systemd 单元与程序 ..."
rm -f "$UNIT"
rm -rf "$INSTALL_DIR"
rm -rf "$CONF_DIR"
systemctl daemon-reload >/dev/null 2>&1 || true

# 数据目录默认保留（含加密凭据），KEEP_DATA=0 时彻底删除
if [ "${KEEP_DATA:-1}" = "0" ]; then
  log "删除数据目录 ${DATA_DIR} ..."
  rm -rf "$DATA_DIR"
else
  log "数据目录 ${DATA_DIR} 已保留（含加密凭据；彻底清除请设 KEEP_DATA=0 或手动 rm -rf）"
fi

if id "$USER_NAME" >/dev/null 2>&1; then
  userdel "$USER_NAME" >/dev/null 2>&1 || true
  log "已删除系统用户 ${USER_NAME}"
fi

log "============================================"
log " 卸载完成"
log "============================================"
