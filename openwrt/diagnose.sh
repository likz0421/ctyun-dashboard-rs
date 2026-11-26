#!/bin/sh
# 天翼云电脑控制中心 · OpenWrt 诊断脚本
# 用法: sh -c "$(wget -4 -qO- https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/openwrt/diagnose.sh)"
# 把全部输出贴给开发者即可精准定位，不会修改系统任何东西。

line() { echo "--------------------------------------------"; }
sec()  { line; echo "[$1]"; line; }

sec "1. 系统信息"
echo "架构      : $(uname -m)"
echo "型号      : $(tr -d '\000' </proc/device-tree/model 2>/dev/null || echo N/A)"
(. /etc/openwrt_release 2>/dev/null; echo "OpenWrt   : ${DISTRIB_RELEASE} ${DISTRIB_DESCRIPTION}")
echo "包管理器  : $(command -v apk >/dev/null && echo apk || (command -v opkg >/dev/null && echo opkg || echo none))"

sec "2. LuCI 类型（关键）"
if [ -d /usr/share/luci/menu.d ]; then
  echo "现代 LuCI (ucode/JS): 是  -> /usr/share/luci/menu.d 存在"
  echo "menu.d 里的应用数: $(ls /usr/share/luci/menu.d/*.json 2>/dev/null | wc -l)"
else
  echo "现代 LuCI (ucode/JS): 否"
fi
if [ -d /usr/lib/lua/luci/controller ]; then
  echo "传统 Lua LuCI       : 是  -> /usr/lib/lua/luci/controller 存在"
  echo "controller 文件数   : $(ls /usr/lib/lua/luci/controller/*.lua 2>/dev/null | wc -l)"
else
  echo "传统 Lua LuCI       : 否"
fi
echo "uhttpd 状态         : $(/etc/init.d/uhttpd status 2>/dev/null || echo 'unknown')"
echo "rpcd 状态           : $(/etc/init.d/rpcd status 2>/dev/null || echo 'unknown')"

sec "3. 本插件文件是否齐全"
check() { [ -e "$2" ] && echo "  [OK]   $1 -> $2" || echo "  [缺失] $1 -> $2"; }
check "传统controller" /usr/lib/lua/luci/controller/ctyun_dashboard.lua
check "传统cbi"       /usr/lib/lua/luci/model/cbi/ctyun_dashboard.lua
check "传统view"      /usr/lib/lua/luci/view/ctyun_dashboard/status.htm
check "ucode菜单"     /usr/share/luci/menu.d/luci-app-ctyun-dashboard.json
check "ucode ACL"     /usr/share/rpcd/acl.d/luci-app-ctyun-dashboard.json
check "ucode视图JS"   /www/luci-static/resources/view/ctyun-dashboard/status.js
check "主程序"        /usr/bin/ctyun-dashboard
check "init脚本"      /etc/init.d/ctyun-dashboard
check "ctl工具"       /usr/bin/ctyun-dashboard-ctl
check "UCI配置"       /etc/config/ctyun_dashboard

sec "4. UCI 配置"
cat /etc/config/ctyun_dashboard 2>/dev/null || echo "  (无配置文件)"

sec "5. 服务运行状态"
PORT=$(uci get ctyun_dashboard.main.port 2>/dev/null || echo 8571)
echo "UCI 端口: $PORT"
echo "进程:"
ps -w 2>/dev/null | grep '[c]tyun-dashboard' || echo "  (无进程)"
echo "监听:"
netstat -tln 2>/dev/null | grep -E ":(${PORT}|8571) " || echo "  (无监听)"
echo "健康检查 (${PORT}):"
curl -sS --max-time 3 "http://127.0.0.1:${PORT}/health" 2>&1 | head -c 300 || echo

sec "6. 服务日志 (logread)"
logread 2>/dev/null | grep -i ctyun | tail -20 || echo "  (无日志)"
echo "--- init 脚本直接 start 的输出 ---"
/etc/init.d/ctyun-dashboard start 2>&1 | head -20 || echo "  (start 失败)"
sleep 2
echo "--- start 后进程 ---"
ps -w 2>/dev/null | grep '[c]tyun-dashboard' || echo "  (仍无进程)"

sec "7. 主程序能否直接运行（仅当服务未占用端口时测试）"
if ps -w 2>/dev/null | grep -q '[c]tyun-dashboard'; then
  echo "服务进程已在运行，跳过独立启动测试（此时手动启动报 AddrInUse 属正常现象，非 bug）。"
else
  echo "服务未运行，尝试独立启动 3 秒:"
  CTYUN_DATA_DIR=/var/lib/ctyun-dashboard CTYUN_PORT=8571 timeout 3 /usr/bin/ctyun-dashboard 2>&1 | head -15 || echo "  (进程退出，见上方错误)"
fi

line
echo "诊断完成，请把以上全部输出发给开发者。"
