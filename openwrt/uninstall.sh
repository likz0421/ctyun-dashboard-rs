#!/bin/sh
# 天翼云电脑控制中心 · OpenWrt 一键卸载脚本
# 用法: sh -c "$(wget -4 -qO- https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/openwrt/uninstall.sh)"

set -e
log() { echo "[ctyun-uninstall] $*"; }

log "停止并禁用服务 ..."
/etc/init.d/ctyun-dashboard stop  >/dev/null 2>&1 || true
/etc/init.d/ctyun-dashboard disable >/dev/null 2>&1 || true

log "删除程序与配置 ..."
rm -f /usr/bin/ctyun-dashboard
rm -f /usr/bin/ctyun-dashboard-ctl
rm -f /etc/init.d/ctyun-dashboard
rm -rf /etc/ctyun-dashboard

# 询问是否保留数据（非交互环境默认保留）
log "数据目录 /var/lib/ctyun-dashboard 已保留（如需彻底清除请手动删除）"

log "删除 LuCI 插件文件 ..."
rm -f /usr/lib/lua/luci/controller/ctyun_dashboard.lua
rm -f /usr/lib/lua/luci/model/cbi/ctyun_dashboard.lua
rm -rf /usr/lib/lua/luci/view/ctyun_dashboard
rm -rf /tmp/luci-modulecache /tmp/luci-indexcache 2>/dev/null || true

log "删除防火墙规则 ..."
# 只删除名为 ctyun-dashboard 的规则，绝不误删用户其他规则
uci show firewall 2>/dev/null | grep "name='ctyun-dashboard'" | while IFS='=' read -r key val; do
  section="${key%%.name*}"
  uci -q delete "$section" 2>/dev/null || true
done
# fw4/nftables 规则文件
rm -f /etc/nftables.d/30-ctyun-dashboard.nft
uci commit firewall >/dev/null 2>&1 || true
/etc/init.d/firewall restart >/dev/null 2>&1 || true
fw4 reload >/dev/null 2>&1 || true

log "删除 UCI 配置 ..."
uci -q delete ctyun_dashboard 2>/dev/null || true
uci commit ctyun_dashboard >/dev/null 2>&1 || true

log "============================================"
log " 卸载完成！如需彻底清除数据请执行:"
log " rm -rf /var/lib/ctyun-dashboard"
log "============================================"
