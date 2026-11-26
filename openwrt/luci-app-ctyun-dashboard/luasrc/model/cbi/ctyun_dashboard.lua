-- 天翼云电脑控制中心 · LuCI 配置页
local m, s, o

m = Map("ctyun_dashboard", _("天翼云电脑控制中心"),
	_("服务开关与端口在这里配置；账号、保活任务、电源、积分、日志等日常操作请进入「运行状态」页。"))

-- 基本设置
s = m:section(TypedSection, "main", _("基本设置"))
s.anonymous = true

o = s:option(Flag, "enabled", _("启用服务"))
o.default = 1
o.rmempty = false

o = s:option(Value, "port", _("监听端口"))
o.default = 8571
o.datatype = "port"
o.rmempty = false
o.description = _("修改后保存会自动重启服务")

o = s:option(Value, "data_dir", _("数据目录"))
o.default = "/var/lib/ctyun-dashboard"
o.rmempty = false
o.description = _("存放配置、凭据与日志，卸载时默认保留")

-- 运行状态展示（只读）
s = m:section(TypedSection, "_status", _("服务状态"))
s.anonymous = true

o = s:option(DummyValue, "_svc", _("后台服务"))
o.cfgvalue = function()
	local running = luci.sys.call("pgrep -f '/usr/bin/ctyun-dashboard' >/dev/null 2>&1") == 0
	local port = luci.sys.exec("uci get ctyun_dashboard.main.port 2>/dev/null"):gsub("%s+", "")
	if port == "" then port = "8571" end
	local health = ""
	if running then
		local ok = luci.sys.call("curl -fsS --max-time 3 http://127.0.0.1:" .. port .. "/health >/dev/null 2>&1") == 0
		health = ok and " · " .. _("健康检查正常") or " · " .. _("健康检查未响应")
	end
	return running and (_("● 运行中") .. health) or _("○ 未运行")
end

o = s:option(DummyValue, "_ver", _("版本"))
o.cfgvalue = function()
	local f = io.open("/usr/bin/ctyun-dashboard", "r")
	return f and "Rust Edition" or _("未安装")
end

-- 快捷入口
s = m:section(TypedSection, "_links", _("快捷入口"))
s.anonymous = true

o = s:option(DummyValue, "_ops", _("日常操作台"))
o.cfgvalue = function()
	return "<a href='" .. luci.dispatcher.build_url("admin/services/ctyun_dashboard/status") .. "'>" ..
		_("打开运行状态页（账号/任务/电源/积分/日志/设置）") .. "</a>"
end
o.rawhtml = true

o = s:option(DummyValue, "_panel", _("独立控制面板"))
o.cfgvalue = function()
	local ip = luci.sys.exec("uci get network.lan.ipaddr 2>/dev/null"):gsub("%s+", "")
	local port = luci.sys.exec("uci get ctyun_dashboard.main.port 2>/dev/null"):gsub("%s+", "")
	if port == "" then port = "8571" end
	return "<a href='http://" .. ip .. ":" .. port .. "' target='_blank'>" ..
		_("打开完整面板 (扫码登录/积分商城/远程桌面/短信绑定)") .. "</a>"
end
o.rawhtml = true

-- 保存后重启服务，让端口/开关改动立即生效
function m.on_commit(self)
	luci.sys.call("/etc/init.d/ctyun-dashboard restart >/dev/null 2>&1 &")
end

return m
