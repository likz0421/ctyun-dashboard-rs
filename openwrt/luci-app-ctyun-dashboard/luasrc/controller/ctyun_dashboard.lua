-- 天翼云电脑控制中心 LuCI 控制器
-- 注册菜单：服务 -> 天翼云电脑
-- api_proxy 把 LuCI 内的请求完整转发给本地 Rust 服务（全方法 + token 透传 + body 转发）。

local http = require "luci.http"
local fs   = require "nixio.fs"

module("luci.controller.ctyun_dashboard", package.seeall)

function index()
	if not fs.access("/etc/config/ctyun_dashboard") then
		return
	end

	local page
	page = entry({"admin", "services", "ctyun_dashboard"}, cbi("ctyun_dashboard"), _("天翼云电脑"), 60)
	page.dependent = true
	-- 不设 acl_depends：一键脚本直接拷贝文件的方式没有注册 ACL 定义，
	-- 设了会导致菜单被 LuCI 隐藏（只有编成 ipk 时才应启用）。

	page = entry({"admin", "services", "ctyun_dashboard", "status"}, template("ctyun_dashboard/status"), _("运行状态"), 10)
	page.leaf = true

	-- 通用 API 代理：前端通过相对路径调用，避免跨域；支持全部业务接口
	page = entry({"admin", "services", "ctyun_dashboard", "api"}, call("api_proxy"), nil)
	page.leaf = true
end

-- 读取 UCI 端口
local function get_port()
	local f = io.open("/etc/config/ctyun_dashboard", "r")
	if f then
		local c = f:read("*a"); f:close()
		local p = c:match("option%s+port%s+'([0-9]+)'") or c:match('option%s+port%s+"([0-9]+)"')
		if p then return tonumber(p) or 8571 end
	end
	return 8571
end

-- 危险接口前缀（会转发任意外部请求 / 大文件流式代理），LuCI 内不开放
local BLOCKED = {
	"/api/ctyun-proxy",
	"/ctyun-static",
	"/workers",
	"/desktop-view",
}

-- 业务接口白名单前缀；path 以任一前缀开头即放行
local ALLOW_PREFIX = {
	"/api/status", "/api/accounts", "/api/auth", "/api/rewards", "/api/logs",
	"/api/settings", "/api/user", "/api/captcha", "/api/account", "/api/device",
	"/api/scheduler", "/api/config", "/api/keeper", "/health",
}

local function is_allowed(path)
	for _, b in ipairs(BLOCKED) do
		if path == b or path:find(b .. "/", 1, true) == 1 then return false end
	end
	for _, pre in ipairs(ALLOW_PREFIX) do
		if path == pre or path:find(pre .. "/", 1, true) == 1 or path:find(pre .. "?", 1, true) == 1 then
			return true
		end
	end
	return false
end

-- 只保留 URL 安全字符，防命令注入
local function safe_url(s)
	return s and s:gsub("[^%w%/%.%-%_%?%=&:@]", "") or ""
end

-- 执行 curl 并返回响应体（body 写临时文件，避免 shell 注入）
local function request(method, url, body, token)
	local tmp_body
	local args = { "curl", "-sS", "--max-time", "15", "-X", method, "-H", "Accept: application/json" }
	if token and token ~= "" then
		args[#args + 1] = "-H"
		args[#args + 1] = "Authorization: Bearer " .. safe_url(token)
	end
	if body and body ~= "" then
		tmp_body = "/tmp/ctyun-luci-body-" .. tostring(os.time()) .. "-" .. tostring(math.random(10000, 99999))
		local bf = io.open(tmp_body, "w")
		if bf then bf:write(body); bf:close() end
		args[#args + 1] = "-H"
		args[#args + 1] = "Content-Type: application/json"
		args[#args + 1] = "--data-binary"
		args[#args + 1] = "@" .. tmp_body
	end
	args[#args + 1] = safe_url(url)

	local handle = io.popen(table.concat(args, " "))
	local out = handle and handle:read("*a") or ""
	if handle then handle:close() end
	if tmp_body then os.remove(tmp_body) end
	return out
end

function api_proxy()
	local path   = safe_url(http.formvalue("path") or "")
	-- 前端统一用 POST 访问 LuCI，用 method 参数指定真实方法；无则回退到 HTTP 方法
	local method = (http.formvalue("method") or http.getenv("REQUEST_METHOD") or "GET"):upper()
	if method ~= "GET" and method ~= "POST" and method ~= "PUT" and method ~= "DELETE" then
		method = "GET"
	end
	local body   = http.formvalue("body") or ""
	local token  = http.formvalue("token") or ""

	-- 也接受标准 Authorization 头里的 token
	local auth_hdr = http.getenv("HTTP_AUTHORIZATION") or ""
	if token == "" and auth_hdr:find("Bearer ", 1, true) == 1 then
		token = auth_hdr:sub(8)
	end

	if path == "" or not is_allowed(path) then
		http.status(403, "Forbidden")
		http.write('{"error":"forbidden","path":"' .. path .. '"}')
		return
	end

	local url = "http://127.0.0.1:" .. get_port() .. path
	-- token 作为 query 追加（后端 extract_token 支持 ?token=）
	if token ~= "" then
		url = url .. (path:find("?", 1, true) and "&" or "?") .. "token=" .. safe_url(token)
	end

	local out = request(method, url, body, token)
	if out == "" then out = "{}" end

	http.header("Content-Type", "application/json; charset=utf-8")
	http.header("Cache-Control", "no-store")
	http.write(out)
end
