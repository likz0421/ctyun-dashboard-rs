#!/bin/sh
# 天翼云电脑 · LuCI 菜单独立修复脚本
# 不依赖 install.sh，直接内嵌 ucode/JS 三件套，绕过下载失败与脚本中断问题。
# 用法: sh -c "$(wget -4 -qO- https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/openwrt/fix_luci.sh)"
#
# 背景：新版 OpenWrt（apk / SNAPSHOT）的 LuCI 是 ucode/JS 架构，且常常不装 Lua 运行时。
#       传统 luasrc/*.lua 控制器在这种设备上无效，菜单必须由 menu.d/*.json + JS view 注册。

line() { echo "--------------------------------------------"; }
log()  { echo "[ctyun] $*"; }
err()  { echo "[ctyun][错误] $*" >&2; }
warn() { echo "[ctyun][警告] $*" >&2; }

BASE="https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/openwrt"
download() {
  url="$1" out="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -4 -fsSL --connect-timeout 10 --max-time 60 -o "$out" "$url" 2>/dev/null
  else
    wget -4 -q --no-check-certificate --timeout=15 -O "$out" "$url" 2>/dev/null
  fi
}

line
log "开始修复 LuCI 菜单（ucode/JS 架构）..."

# ------------------------------------------------------------
# 1. 现代 LuCI 菜单注册（menu.d）
#    depends 去掉，避免 ACL 未被 rpcd 加载时菜单被隐藏
# ------------------------------------------------------------
mkdir -p /usr/share/luci/menu.d
cat > /usr/share/luci/menu.d/luci-app-ctyun-dashboard.json <<'CTYUN_EOF'
{
  "admin/services/ctyun_dashboard": {
    "title": "天翼云电脑",
    "order": 60,
    "action": {
      "type": "view",
      "path": "ctyun-dashboard/status"
    }
  }
}
CTYUN_EOF

# ------------------------------------------------------------
# 2. rpcd ACL（放行 uci 与 ubus 访问，保证视图读端口正常）
# ------------------------------------------------------------
mkdir -p /usr/share/rpcd/acl.d
cat > /usr/share/rpcd/acl.d/luci-app-ctyun-dashboard.json <<'CTYUN_EOF'
{
  "luci-app-ctyun-dashboard": {
    "description": "Grant access to ctyun-dashboard LuCI page",
    "read": {
      "ubus": { "uci": ["*"] },
      "uci": ["ctyun_dashboard", "network"]
    },
    "write": {
      "ubus": { "uci": ["*"] },
      "uci": ["ctyun_dashboard"]
    }
  }
}
CTYUN_EOF

# ------------------------------------------------------------
# 3. JS 视图：LuCI 原生控件界面（~700 行，内嵌写入，不依赖下载）
#    完全弃用 iframe：用 LuCI 的 section/value/btn/table/tabs 风格重写全部功能。
# ------------------------------------------------------------
mkdir -p /www/luci-static/resources/view/ctyun-dashboard
cat > /www/luci-static/resources/view/ctyun-dashboard/status.js <<'CTYUN_VIEW_JS'
'use strict';
'require view';
'require uci';

/*
 * 天翼云电脑控制中心 · LuCI 原生视图（完全用 LuCI 控件风格构建，非 iframe）
 * 直接 fetch 后端 API（Bearer token），后端需开启 CORS。
 * 只用原生 DOM API，不依赖 E()/H()/_() 等辅助函数，兼容所有 LuCI 版本。
 */
/* ============================================================
   全局状态（必须在 return 之前初始化：return 之后的 var 赋值不会执行）
   ============================================================ */
var CTX = {
	api: '', token: '', user: null, tab: 'dashboard',
	accounts: [], status: null, rewards: [], selected: '',
	poll: null, logPoll: null
};
var TABS = [
	['dashboard', '总览'], ['accounts', '云电脑'], ['tasks', '任务中心'],
	['rewards', '积分商城'], ['logs', '实时日志'], ['settings', '系统设置']
];

return view.extend({
	load: function () {
		return uci.load('ctyun_dashboard');
	},

	render: function () {
		var root = document.createElement('div');
		root.className = 'ctyun-app';
		root.appendChild(buildStyle());

		var port = uci.get('ctyun_dashboard', 'main', 'port') || '8571';
		var host = window.location.hostname || location.hostname;
		CTX.api = 'http://' + host + ':' + port;

		var main = document.createElement('div');
		main.id = 'ctyun-main';
		root.appendChild(main);

		// 异步启动：先判断登录态
		boot(main);
		return root;
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});

/* ============================================================
   基础工具
   ============================================================ */
function el(tag, attrs, kids) {
	var e = document.createElement(tag);
	if (attrs) for (var k in attrs) {
		if (!Object.prototype.hasOwnProperty.call(attrs, k)) continue;
		var v = attrs[k];
		if (v === null || v === undefined) continue;
		if (k === 'class') e.className = v;
		else if (k === 'style') e.setAttribute('style', v);
		else if (k === 'html') e.innerHTML = v;
		else if (k.indexOf('on') === 0 && typeof v === 'function') e.addEventListener(k.slice(2), v);
		else if (v === true) e.setAttribute(k, k);
		else if (v !== false) e.setAttribute(k, v);
	}
	if (kids) {
		if (!Array.isArray(kids)) kids = [kids];
		for (var i = 0; i < kids.length; i++) {
			var c = kids[i];
			if (c === null || c === undefined || c === false) continue;
			e.appendChild(typeof c === 'string' || typeof c === 'number'
				? document.createTextNode(String(c)) : c);
		}
	}
	return e;
}
function clearNode(n) { while (n.firstChild) n.removeChild(n.firstChild); }
function esc(v) { return String(v == null ? '' : v).replace(/[&<>"']/g, function (c) {
	return ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c];
}); }
// 账号显示名：占位名回退手机号，并附加手机号后 4 位，避免多个账号同名分不清。
function ctyunAccLabel(a) {
	var name = String(a && a.name || '');
	var user = String(a && a.user || '');
	var base = (name && name !== '扫码账号') ? name : (user || '云电脑账号');
	var last4 = user.replace(/\s/g, '').slice(-4);
	if (!last4 || base.indexOf(user) >= 0 || base.indexOf(last4) >= 0) return esc(base);
	return esc(base) + ' · ' + esc(last4);
}
function token() {
	if (CTX.token) return CTX.token;
	try { return localStorage.getItem('ctyun_token') || ''; } catch (e) { return ''; }
}
function saveToken(t) {
	CTX.token = t;
	try { if (t) localStorage.setItem('ctyun_token', t); else localStorage.removeItem('ctyun_token'); } catch (e) {}
}
function num(v) { return Number(v || 0).toLocaleString('zh-CN'); }

/* ============================================================
   API 封装
   ============================================================ */
async function api(path, opts) {
	opts = opts || {};
	opts.headers = opts.headers || {};
	var tk = token();
	if (tk) opts.headers['Authorization'] = 'Bearer ' + tk;
	if (opts.body && typeof opts.body !== 'string') {
		opts.headers['Content-Type'] = 'application/json';
		opts.body = JSON.stringify(opts.body);
	}
	var res;
	try { res = await fetch(CTX.api + path, opts); }
	catch (e) { throw new Error('无法连接后端服务（' + CTX.api + '），请确认服务已启动: ' + e.message); }
	var text = await res.text();
	var data; try { data = text ? JSON.parse(text) : null; } catch (e) { data = text; }
	if (!res.ok) {
		var emsg = (data && (data.error || data.message)) || ('请求失败 (' + res.status + ')');
		// 401 且确实已登录过才提示会话过期；登录接口自身返回的 401 是用户名/密码错误，原样显示。
		if (res.status === 401 && !opts.__login) { saveToken(''); emsg = '登录已过期，请重新登录'; }
		throw new Error(emsg);
	}
	return data;
}

/* ============================================================
   提示条（LuCI alert 风格）
   ============================================================ */
function notify(msg, type) {
	type = type || 'info';
	var host = document.getElementById('ctyun-notify');
	if (!host) {
		host = el('div', { id: 'ctyun-notify', style: 'position:fixed;top:8px;right:8px;z-index:9999;max-width:420px' });
		document.body.appendChild(host);
	}
	var cls = type === 'error' ? 'alert-danger' : type === 'success' ? 'alert-success' : 'alert-info';
	var box = el('div', { 'class': 'alert ' + cls, style: 'margin-bottom:6px;padding:8px 12px' }, msg);
	host.appendChild(box);
	setTimeout(function () { if (box.parentNode) box.parentNode.removeChild(box); }, 4000);
}

/* 主题化确认/输入弹窗：跟随 LuCI 深浅主题，替代浏览器原生 confirm/prompt */
function uiConfirm(title, desc, okText, inputLabel, defaultValue) {
	return new Promise(function (resolve) {
		var isPrompt = typeof inputLabel === 'string';
		var mask = el('div', { 'class': 'ctyun-confirm-mask' });
		var box = el('div', { 'class': 'ctyun-confirm-box cbi-section' });
		box.appendChild(el('h3', { style: 'margin:0 0 6px;font-size:16px' }, title || '确认操作'));
		if (desc) box.appendChild(el('p', { style: 'margin:0 0 12px;font-size:12px;opacity:.75;white-space:pre-line;line-height:1.6' }, desc));
		var input = null;
		if (isPrompt) {
			input = el('input', { 'class': 'cbi-input', type: 'text', value: defaultValue || '', style: 'width:100%;box-sizing:border-box;margin-bottom:12px' });
			box.appendChild(el('label', { style: 'display:block;font-size:11px;margin-bottom:4px;opacity:.8' }, inputLabel));
			box.appendChild(input);
		}
		var done = function (val) {
			if (mask.parentNode) mask.parentNode.removeChild(mask);
			resolve(val);
		};
		var okBtn = el('button', { 'class': 'btn cbi-button cbi-button-apply', type: 'button' }, okText || '确定');
		var cancelBtn = el('button', { 'class': 'btn cbi-button', type: 'button', style: 'margin-left:8px' }, '取消');
		okBtn.addEventListener('click', function () { done(isPrompt ? (input && input.value ? input.value : null) : true); });
		cancelBtn.addEventListener('click', function () { done(null); });
		mask.addEventListener('click', function (e) { if (e.target === mask) done(null); });
		box.appendChild(el('div', { style: 'margin-top:6px' }, [okBtn, cancelBtn]));
		mask.appendChild(box);
		document.body.appendChild(mask);
		if (input) setTimeout(function () { input.focus(); }, 50);
	});
}

/* ============================================================
   登录页
   ============================================================ */
function renderLogin(main) {
	clearNode(main);
	stopPolls();
	var card = el('div', { 'class': 'cbi-section', style: 'max-width:420px;margin:40px auto' });
	card.appendChild(el('h3', { 'class': 'cbi-section-title' }, '云电脑控制中心 · 登录'));
	var inner = el('div', { style: 'padding:16px' });

	var u = el('input', { 'class': 'cbi-input', type: 'text', id: 'cty-u', placeholder: '用户名', autocomplete: 'username' });
	var p = el('input', { 'class': 'cbi-input', type: 'password', id: 'cty-p', placeholder: '密码', autocomplete: 'current-password' });
	inner.appendChild(field('用户名', u));
	inner.appendChild(field('密码', p));
	var btn = el('button', { 'class': 'btn cbi-button cbi-button-apply', type: 'button' }, '登录');
	btn.addEventListener('click', async function () {
		btn.disabled = true;
		try {
			var d = await api('/api/auth/login', { method: 'POST', __login: true, body: {
				username: u.value.trim(), password: p.value, remember: true
			} });
			saveToken(d.token || (d && d.data && d.data.token) || '');
			var me;
			try { me = await api('/api/auth/me'); } catch (e) { me = { username: u.value.trim(), role: 'user' }; }
			CTX.user = me;
			notify('登录成功', 'success');
			renderShell(main);
			refreshTab();
		} catch (e) { notify(e.message, 'error'); }
		finally { btn.disabled = false; }
	});
	p.addEventListener('keydown', function (e) { if (e.key === 'Enter') btn.click(); });
	inner.appendChild(el('div', { style: 'margin-top:12px' }, btn));
	card.appendChild(inner);
	main.appendChild(card);
}

function field(label, input) {
	return el('div', { 'class': 'cbi-value' }, [
		el('label', { 'class': 'cbi-value-title' }, label),
		el('div', { 'class': 'cbi-value-field' }, input)
	]);
}

/* ============================================================
   主框架：顶部栏 + Tab 导航 + 内容区
   ============================================================ */
function renderShell(main) {
	clearNode(main);
	stopPolls();
	var bar = el('div', { 'class': 'cbi-section', style: 'padding:10px 14px;margin-bottom:10px;display:flex;align-items:center;gap:12px;flex-wrap:wrap' });
	bar.appendChild(el('strong', { style: 'font-size:16px' }, '☁ 天翼云电脑控制中心'));
	bar.appendChild(el('span', { style: 'flex:1' }));
	var who = el('span', { style: 'color:#666;font-size:13px' },
		((CTX.user && CTX.user.username) || '用户') + (CTX.user && CTX.user.role === 'admin' ? '（管理员）' : ''));
	bar.appendChild(who);
	var out = el('button', { 'class': 'btn cbi-button', type: 'button' }, '退出');
	out.addEventListener('click', async function () {
		try { await api('/api/auth/logout', { method: 'POST' }); } catch (e) {}
		saveToken(''); CTX.user = null; renderLogin(main);
	});
	bar.appendChild(out);
	main.appendChild(bar);

	var nav = el('div', { 'class': 'tabs', style: 'margin-bottom:12px' });
	var ul = el('ul', { 'class': 'nav nav-tabs', style: 'display:flex;gap:4px;flex-wrap:wrap;list-style:none;margin:0;padding:0' });
	TABS.forEach(function (t) {
		var li = el('li', { 'data-tab': t[0] });
		var a = el('a', { href: '#', 'class': 'btn cbi-button' + (CTX.tab === t[0] ? ' cbi-button-action active' : '') }, t[1]);
		a.addEventListener('click', function (e) {
			e.preventDefault();
			CTX.tab = t[0];
			Array.prototype.forEach.call(ul.querySelectorAll('a'), function (x) { x.className = 'btn cbi-button'; });
			a.className = 'btn cbi-button cbi-button-action active';
			refreshTab();
		});
		li.appendChild(a); ul.appendChild(li);
	});
	nav.appendChild(ul);
	main.appendChild(nav);

	var content = el('div', { id: 'ctyun-content' });
	main.appendChild(content);
}

function refreshTab() {
	var c = document.getElementById('ctyun-content');
	if (!c) return;
	stopPolls();
	switch (CTX.tab) {
		case 'dashboard': viewDashboard(c); break;
		case 'accounts': viewAccounts(c); break;
		case 'tasks': viewTasks(c); break;
		case 'rewards': viewRewards(c); break;
		case 'logs': viewLogs(c); break;
		case 'settings': viewSettings(c); break;
	}
}

function stopPolls() {
	if (CTX.poll) { clearInterval(CTX.poll); CTX.poll = null; }
	if (CTX.logPoll) { clearInterval(CTX.logPoll); CTX.logPoll = null; }
	if (CTX.qrPoll) { clearInterval(CTX.qrPoll); CTX.qrPoll = null; }
}

async function boot(main) {
	// 先查询鉴权模式：内网关闭鉴权时免登录，直接以管理员身份进入
	try {
		var cfg = await fetch(CTX.api + '/api/auth/config').then(function (r) { return r.ok ? r.json() : null; });
		if (cfg && cfg.auth_required === false) {
			CTX.user = { username: 'admin', role: 'admin' };
			renderShell(main);
			refreshTab();
			return;
		}
	} catch (e) {}
	var tk = token();
	if (tk) {
		try {
			CTX.user = await api('/api/auth/me');
			renderShell(main);
			refreshTab();
			return;
		} catch (e) { saveToken(''); }
	}
	renderLogin(main);
}

/* ============================================================
   总览
   ============================================================ */
async function viewDashboard(c) {
	clearNode(c);
	var sec = el('div', { 'class': 'cbi-section' });
	sec.appendChild(el('h3', { 'class': 'cbi-section-title' }, '运行总览'));
	var cards = el('div', { id: 'cty-cards', style: 'display:flex;gap:12px;flex-wrap:wrap;padding:12px' });
	sec.appendChild(cards);
	var tblHost = el('div', { id: 'cty-tasktbl', style: 'padding:0 12px 12px' });
	sec.appendChild(tblHost);
	// 版本与更新：紧凑小徽标条（能看见即可，不喧宾夺主）
	if (CTX.user && CTX.user.role === 'admin') {
		var verBox = el('div', { id: 'cty-verbox', style: 'margin:0 12px 12px;display:inline-flex;align-items:center;gap:8px;padding:5px 10px;border:1px solid rgba(128,128,128,.25);border-radius:999px;background:rgba(128,128,128,.06);flex-wrap:wrap' });
		var verBadge = el('span', { id: 'cty-ver-badge', style: 'font-size:10px;font-weight:700;padding:3px 8px;border-radius:999px;background:rgba(106,138,163,.18);color:#3f5f7d;white-space:nowrap' }, '版本');
		var verNum = el('strong', { id: 'cty-ver-num', style: 'font-size:11px;font-weight:700' }, 'v—');
		var verStatus = el('span', { id: 'cty-ver-status', style: 'font-size:10px;color:#888;white-space:nowrap' }, '正在检查更新…');
		var verCheck = el('button', { 'class': 'btn cbi-button', type: 'button', style: 'height:24px;padding:0 9px;font-size:10px' }, '检查更新');
		var verDo = el('button', { 'class': 'btn cbi-button cbi-button-apply', type: 'button', style: 'height:24px;padding:0 9px;font-size:10px;display:none' }, '立即升级');
		verBox.appendChild(verBadge);
		verBox.appendChild(verNum);
		verBox.appendChild(verStatus);
		verBox.appendChild(el('div', {}, [verCheck, verDo]));
		sec.insertBefore(verBox, cards);
		ctyunBindUpgrade(verStatus, verDo, verCheck, null, verBadge);
	}
	c.appendChild(sec);

	async function load() {
		try {
			var s = await api('/api/status');
			CTX.status = s;
			clearNode(cards);
			card(cards, '账号总数', s.accountsTotal, '#40916c');
			card(cards, '在线保活', s.onlineKeepAlive, '#2a9d8f');
			card(cards, '今日签到', s.signedToday, '#e9c46a');
			card(cards, '今日积分', num(s.totalEarnedPoints), '#e76f51');
			renderTaskTable(tblHost, s.pointsDetails || [], s.currentTime);
		} catch (e) { notify(e.message, 'error'); }
	}
	load();
	CTX.poll = setInterval(load, 10000);
}

/* 升级检查/执行：供总览卡片与设置页共用 */
function ctyunBindUpgrade(statusEl, doEl, checkEl, infoEl, badgeEl) {
	function setBadge(text, bg, fg) {
		if (!badgeEl) return;
		badgeEl.textContent = text;
		badgeEl.style.background = bg;
		badgeEl.style.color = fg;
	}
	var aboutVer = document.getElementById('cty-about-ver');
	function render(r, manual) {
		var cur = r.current_version || '?';
		var lat = (r.latest_version || '').replace(/^v/, '') || '?';
		var numEl = document.getElementById('cty-ver-num');
		if (numEl) numEl.textContent = 'v' + cur;
		if (aboutVer) aboutVer.textContent = 'v' + cur;
		var showDo = false, text = '', cls = '#888';
		if (!r.reachable) { text = '暂时无法连接更新服务器'; showDo = false; setBadge('离线', '#f2f2f2', '#888'); }
		else if (r.update_available) { text = '发现新版本 v' + lat + '，点击升级'; showDo = true; cls = '#b45309'; setBadge('可升级', 'rgba(180,83,9,.15)', '#b45309'); }
		else {
			var byMd5 = r.method === 'md5' && r.local_md5 && r.remote_md5;
			text = byMd5 ? '已是最新版本（校验值一致）' : '已是最新版本';
			showDo = false; cls = '#2a9d8f'; setBadge('最新', 'rgba(47,125,91,.14)', '#2f7d5b');
		}
		if (statusEl) { statusEl.textContent = text; statusEl.style.color = cls; }
		if (infoEl) infoEl.textContent = (statusEl ? text : '') || '当前版本 v' + cur + ' · ' + text;
		if (doEl) doEl.style.display = showDo ? '' : 'none';
		if (manual) notify(text, r.reachable ? 'success' : 'warning');
	}
	function check(manual) {
		if (manual && statusEl) statusEl.textContent = '正在检查更新…';
		api('/api/upgrade/check').then(function (r) { render(r, manual); })
			.catch(function (e) { if (statusEl) statusEl.textContent = '检查更新失败'; if (manual) notify(e.message, 'error'); });
	}
	if (checkEl) checkEl.addEventListener('click', function () { check(true); });
	if (doEl) doEl.addEventListener('click', async function () {
		if (!await uiConfirm('确认在线升级', '账号与数据全部保留，升级后服务自动重启，请稍后刷新页面。', '立即升级')) return;
		doEl.disabled = true;
		if (statusEl) statusEl.textContent = '正在下载并安装新版本…';
		if (infoEl) infoEl.textContent = '正在下载并安装新版本…';
		api('/api/upgrade/do', { method: 'POST', body: {} }).then(function (r) {
			notify(r.message || '升级完成，正在重启', 'success');
			if (statusEl) statusEl.textContent = '升级完成，服务重启中…';
			if (infoEl) infoEl.textContent = '升级完成，服务重启中…';
			if (doEl) doEl.style.display = 'none';
			var n = 0;
			var tick = function () {
				n++;
				fetch('/health', { cache: 'no-store' }).then(function (resp) { if (resp.ok) { location.reload(); return; } }).catch(function () {});
				if (n < 60) setTimeout(tick, 1500); else notify('请稍后手动刷新页面', 'warning');
			};
			setTimeout(tick, 2500);
		}).catch(function (e) { doEl.disabled = false; if (statusEl) statusEl.textContent = '升级失败，请重试'; if (infoEl) infoEl.textContent = '升级失败，请重试'; notify(e.message, 'error'); });
	});
	check(false);
}

function card(host, label, value, color) {
	host.appendChild(el('div', { style: 'flex:1;min-width:120px;border:1px solid #ddd;border-radius:8px;padding:12px;text-align:center' }, [
		el('div', { style: 'font-size:26px;font-weight:600;color:' + color }, String(value)),
		el('div', { style: 'font-size:12px;color:#888;margin-top:4px' }, label)
	]));
}

function renderTaskTable(host, details, now) {
	clearNode(host);
	host.appendChild(el('h4', { style: 'margin:4px 0 8px' }, '今日任务完成情况'));
	if (!details.length) { host.appendChild(el('p', { style: 'color:#888' }, '暂无账号，请先在“云电脑”页添加账号。')); return; }
	var tbl = el('table', { 'class': 'table cbi-section-table', style: 'width:100%;border-collapse:collapse' });
	var head = el('tr', {}, [
		th('账号'), th('今日积分'), th('积分余额'), th('任务进度')
	]);
	tbl.appendChild(el('thead', {}, head));
	var body = el('tbody');
	details.forEach(function (d) {
		var tasks = el('div', { style: 'display:flex;flex-direction:column;gap:7px' });
		(d.tasks || []).forEach(function (t) {
			var prog = t.progress || '0/1';
			var pm = String(prog).match(/^(\d+)\/(\d+)$/);
			if (!t.completed && pm && Number(pm[2]) >= 600 && /使用1小时|挂机/.test(t.name || '')) {
				prog = Math.round(Number(pm[1]) / 60) + '/' + Math.round(Number(pm[2]) / 60) + ' 分钟';
			}
			var done = !!t.completed, pts = Number(t.points) || 0;
			var row = el('div', { style: 'display:flex;align-items:flex-start;gap:8px;padding:7px 9px;border-radius:8px;background:rgba(128,128,128,.07)' });
			row.appendChild(el('span', { style: 'width:18px;height:18px;flex:none;border-radius:50%;display:grid;place-items:center;font-size:11px;font-weight:700;color:' + (done ? '#2a9d8f' : '#999') + ';background:' + (done ? 'rgba(42,157,143,.15)' : 'rgba(128,128,128,.12)') }, done ? '✓' : '·'));
			var main = el('div', { style: 'flex:1;min-width:0' });
			var top = el('div', { style: 'display:flex;align-items:center;justify-content:space-between;gap:8px' });
			top.appendChild(el('strong', { style: 'font-size:12px;font-weight:600' }, esc(t.name || '')));
			top.appendChild(el('span', { style: 'font-size:10px;font-weight:700;padding:2px 7px;border-radius:20px;white-space:nowrap;color:' + (done ? '#2a9d8f' : '#999') + ';background:' + (done ? 'rgba(42,157,143,.14)' : 'rgba(128,128,128,.12)') }, done ? '+' + pts + ' 积分' : '待领 +' + pts));
			main.appendChild(top);
			if (done) {
				main.appendChild(el('div', { style: 'font-size:10px;color:#999;margin-top:3px' }, '达成 ' + esc(t.completedAt || '今日已达成')));
			} else {
				var pm2 = String(t.progress || '').match(/^(\d+)\/(\d+)$/);
				var pct = pm2 && Number(pm2[2]) > 0 ? Math.min(100, Math.round(Number(pm2[1]) / Number(pm2[2]) * 100)) : 0;
				main.appendChild(el('div', { style: 'height:3px;border-radius:3px;background:rgba(128,128,128,.18);margin-top:6px;overflow:hidden' }, [
					el('i', { style: 'display:block;height:100%;border-radius:3px;background:#5f86a8;width:' + pct + '%' })
				]));
				main.appendChild(el('div', { style: 'font-size:10px;color:#b45309;margin-top:3px' }, '进行中 · ' + prog));
			}
			row.appendChild(main);
			tasks.appendChild(row);
		});
		body.appendChild(el('tr', {}, [
			td(esc(d.accountName || '')), td(String(d.todayPoints || 0)), td(String(d.totalPoints || 0)), el('td', {}, tasks)
		]));
	});
	tbl.appendChild(body);
	host.appendChild(tbl);
	if (now) host.appendChild(el('div', { style: 'font-size:11px;color:#999;margin-top:6px' }, '更新于 ' + now));
}
function th(t) { return el('th', { style: 'text-align:left;padding:6px;border-bottom:1px solid #ddd' }, t); }
function td(t) { return el('td', { style: 'padding:6px;border-bottom:1px solid #eee' }, t); }

/* ============================================================
   云电脑（账号 + 实例 + 电源 + 保活开关）
   ============================================================ */
async function viewAccounts(c) {
	clearNode(c);
	var sec = el('div', { 'class': 'cbi-section' });
	var head = el('div', { style: 'display:flex;align-items:center;gap:8px;padding:10px 12px' });
	head.appendChild(el('h3', { 'class': 'cbi-section-title', style: 'margin:0;flex:1' }, '云电脑账号与实例'));
	var add = el('button', { 'class': 'btn cbi-button cbi-button-add', type: 'button' }, '+ 添加账号');
	add.addEventListener('click', function () { dlgAddAccount(c); });
	head.appendChild(add);
	head.appendChild(el('button', {
		'class': 'btn cbi-button', type: 'button',
		onclick: function () { viewAccounts(c); }
	}, '刷新'));
	sec.appendChild(head);
	var host = el('div', { id: 'cty-acchost', style: 'padding:0 12px 12px' });
	sec.appendChild(host);
	c.appendChild(sec);

	try { CTX.accounts = await api('/api/accounts'); }
	catch (e) { notify(e.message, 'error'); CTX.accounts = []; }
	renderAccountList(host);
}

function renderAccountList(host) {
	clearNode(host);
	if (!CTX.accounts.length) { host.appendChild(el('p', { style: 'color:#888' }, '暂无账号。点击“添加账号”开始。')); return; }
	CTX.accounts.forEach(function (a) {
		var box = el('div', { style: 'border:1px solid #ddd;border-radius:8px;margin-bottom:10px;padding:10px 12px' });
		var head = el('div', { style: 'display:flex;align-items:center;gap:8px;flex-wrap:wrap' });
		head.appendChild(el('strong', {}, esc(a.name || a.user || '未命名账号')));
		head.appendChild(el('span', { style: 'font-size:12px;color:#888' }, '手机 ' + esc(a.user || '-')));
		head.appendChild(el('span', { style: 'font-size:12px;color:' + (a.enabled ? '#2a9d8f' : '#999') }, a.enabled ? '● 已启用' : '○ 已停用'));
		head.appendChild(el('span', { style: 'font-size:12px;color:' + (a.sessionExpired ? '#e76f51' : '#2a9d8f') }, a.sessionExpired ? '会话过期' : '会话正常'));
		head.appendChild(el('span', { style: 'flex:1' }));
		var en = el('input', { type: 'checkbox' }); en.checked = !!a.enabled;
		en.addEventListener('change', async function () {
			try { await api('/api/accounts/' + a.id, { method: 'PUT', body: { enabled: en.checked } }); notify('已更新启用状态', 'success'); }
			catch (e) { notify(e.message, 'error'); }
		});
		head.appendChild(el('label', { style: 'font-size:12px' }, [en, ' 启用']));
		var del = el('button', { 'class': 'btn cbi-button cbi-button-remove', type: 'button' }, '删除');
		del.addEventListener('click', async function () {
			if (!await uiConfirm('删除账号', '确定删除账号「' + (a.name || a.user) + '」？此操作不可撤销。', '删除')) return;
			try { await api('/api/accounts/' + a.id, { method: 'DELETE' }); notify('已删除', 'success'); viewAccounts(document.getElementById('ctyun-content')); }
			catch (e) { notify(e.message, 'error'); }
		});
		head.appendChild(del);
		box.appendChild(head);

		var vmHead = el('div', { style: 'margin-top:8px;font-size:12px;color:#666' });
		vmHead.appendChild(el('button', {
			'class': 'btn cbi-button', type: 'button',
			onclick: async function () {
				try { await api('/api/accounts/' + a.id + '/desktops'); notify('云电脑列表已刷新', 'success'); viewAccounts(document.getElementById('ctyun-content')); }
				catch (e) { notify(e.message, 'error'); }
			}
		}, '刷新实例'));
		box.appendChild(vmHead);

		var vms = a.desktops || [];
		if (!vms.length) box.appendChild(el('div', { style: 'color:#999;font-size:12px;margin-top:6px' }, '未获取到云电脑实例，请点“刷新实例”。'));
		vms.forEach(function (vm) {
			var row = el('div', { style: 'display:flex;align-items:center;gap:8px;flex-wrap:wrap;border-top:1px solid #eee;padding:8px 0;font-size:13px' });
			row.appendChild(el('span', {}, esc(vm.desktopName || vm.desktop_id || '实例')));
			row.appendChild(el('span', { style: 'color:#888;font-size:12px' }, esc(vm.flavorName || vm.specStr || '')));
			row.appendChild(el('span', { style: 'color:#888;font-size:12px' }, esc(vm.useStatusText || '')));
			row.appendChild(el('span', { style: 'flex:1' }));
			['boot', 'shutdown', 'reboot'].forEach(function (act) {
				var b = el('button', { 'class': 'btn cbi-button', type: 'button' }, act === 'boot' ? '开机' : act === 'shutdown' ? '关机' : '重启');
				b.addEventListener('click', async function () {
					try {
						var r = await api('/api/accounts/' + a.id + '/power/' + act, { method: 'POST', body: { desktopId: vm.desktop_id } });
						notify(r.message || r.error || '已下发', r.error ? 'error' : 'success');
					} catch (e) { notify(e.message, 'error'); }
				});
				row.appendChild(b);
			});
			box.appendChild(row);
		});
		host.appendChild(box);
	});
}

function dlgAddAccount(c) {
	var modal = el('div', { style: 'border:1px solid #ddd;border-radius:8px;padding:12px;margin-bottom:12px;background:#fafafa' });
	modal.appendChild(el('h4', { style: 'margin:0 0 8px' }, '添加天翼云账号'));
	var name = el('input', { 'class': 'cbi-input', type: 'text', placeholder: '备注名称（可选）' });
	var user = el('input', { 'class': 'cbi-input', type: 'text', placeholder: '手机号（必填）' });
	var pass = el('input', { 'class': 'cbi-input', type: 'password', placeholder: '密码' });
	var dev = el('input', { 'class': 'cbi-input', type: 'text', placeholder: '设备编号（可选，留空自动生成）' });
	modal.appendChild(field('备注名称', name));
	modal.appendChild(field('手机号', user));
	modal.appendChild(field('密码', pass));
	modal.appendChild(field('设备编号', dev));
	var save = el('button', { 'class': 'btn cbi-button cbi-button-apply', type: 'button' }, '保存');
	save.addEventListener('click', async function () {
		if (!user.value.trim()) { notify('手机号不能为空', 'error'); return; }
		save.disabled = true;
		try {
			await api('/api/accounts', { method: 'POST', body: {
				name: name.value.trim(), user: user.value.trim(), password: pass.value, deviceCode: dev.value.trim()
			} });
			notify('账号已添加', 'success');
			viewAccounts(c);
		} catch (e) { notify(e.message, 'error'); }
		finally { save.disabled = false; }
	});
	modal.appendChild(el('div', { style: 'margin-top:10px' }, [save,
		el('button', { 'class': 'btn cbi-button', type: 'button', style: 'margin-left:8px', onclick: function () { modal.parentNode.removeChild(modal); } }, '取消')
	]));

	// 扫码添加账号（复用后端 generate + status 轮询，与独立面板一致）
	var qrsec = el('div', { style: 'border:1px solid #ddd;border-radius:8px;padding:12px;margin-top:10px;background:#fff' });
	qrsec.appendChild(el('h4', { style: 'margin:0 0 8px' }, '扫码添加账号（天翼云电脑 App 授权）'));
	var qrHint = el('div', { style: 'font-size:12px;color:#888;margin-bottom:8px' }, '填好备注后点“生成二维码”，用天翼云电脑 App 扫码授权，授权成功会自动添加。');
	qrsec.appendChild(qrHint);
	var qrImg = el('div', { style: 'min-height:40px;margin:8px 0' });
	qrsec.appendChild(qrImg);
	var qrDev = el('input', { 'class': 'cbi-input', type: 'text', placeholder: '设备编号（可选，留空自动生成）' });
	qrsec.appendChild(field('设备编号', qrDev));
	var qrBtn = el('button', { 'class': 'btn cbi-button cbi-button-add', type: 'button' }, '生成二维码');
	qrsec.appendChild(qrBtn);
	qrBtn.addEventListener('click', async function () {
		qrBtn.disabled = true;
		try {
			var r = await api('/api/account/qrcode/generate', { method: 'POST', body: { deviceCode: qrDev.value.trim() } });
			if (r.success === false) throw new Error(r.error || '二维码生成失败');
			qrDev.value = r.deviceCode || qrDev.value;
			clearNode(qrImg);
			if (r.qrImage) qrImg.appendChild(el('img', { src: r.qrImage, alt: '授权二维码', style: 'width:180px;height:180px' }));
			else qrImg.appendChild(el('a', { href: r.qrUrl, target: '_blank' }, '打开授权页面'));
			qrHint.textContent = '已生成，请用天翼云电脑 App 扫码授权，等待自动添加…';
			if (CTX.qrPoll) clearInterval(CTX.qrPoll);
			var tries = 0;
			CTX.qrPoll = setInterval(async function () {
				if (++tries > 120) { clearInterval(CTX.qrPoll); qrHint.textContent = '扫码超时，请重新生成二维码'; return; }
				try {
					var st = await api('/api/account/qrcode/status?qrCodeId=' + encodeURIComponent(r.qrCodeId) +
						'&deviceCode=' + encodeURIComponent(r.deviceCode) +
						'&accountName=' + encodeURIComponent(name.value.trim() || ''));
					if (st.codeStatus === 'authorize' || st.codeStatus === 'success') {
						clearInterval(CTX.qrPoll);
						qrHint.textContent = '授权成功，账号已添加';
						notify('扫码授权成功，账号已添加', 'success');
						viewAccounts(c);
					}
				} catch (e) { /* 轮询中忽略瞬时错误 */ }
			}, 2500);
		} catch (e) { notify(e.message, 'error'); }
		finally { qrBtn.disabled = false; }
	});
	modal.appendChild(qrsec);

	var anchor = document.getElementById('ctyun-content').firstChild;
	anchor.parentNode.insertBefore(modal, anchor);
}

/* ============================================================
   任务中心
   ============================================================ */
async function viewTasks(c) {
	clearNode(c);
	var sec = el('div', { 'class': 'cbi-section' });
	sec.appendChild(el('h3', { 'class': 'cbi-section-title' }, '任务中心'));
	var host = el('div', { style: 'padding:12px' });
	sec.appendChild(host);
	c.appendChild(sec);
	try { CTX.accounts = await api('/api/accounts'); }
	catch (e) { notify(e.message, 'error'); CTX.accounts = []; }
	if (!CTX.accounts.length) { host.appendChild(el('p', { style: 'color:#888' }, '暂无账号。')); return; }

	var sel = el('select', { 'class': 'cbi-input' });
	CTX.accounts.forEach(function (a) {
		var o = el('option', { value: a.id }, ctyunAccLabel(a));
		if (a.id === CTX.selected) o.selected = true;
		sel.appendChild(o);
	});
	if (!CTX.selected && CTX.accounts.length) CTX.selected = CTX.accounts[0].id;
	sel.value = CTX.selected;
	sel.addEventListener('change', function () { CTX.selected = sel.value; });
	host.appendChild(field('目标账号', sel));

	var grid = el('div', { style: 'display:flex;gap:10px;flex-wrap:wrap;margin-top:12px' });
	[['sign', '立即签到', 'cbi-button-action'],
	 ['hang', '挂机 1 小时', ''],
	 ['aiChat', 'AI 对话', ''],
	 ['redeem', '执行兑换', '']].forEach(function (t) {
		var b = el('button', { 'class': 'btn cbi-button ' + t[2], type: 'button' }, t[1]);
		b.addEventListener('click', async function () {
			if (!CTX.selected) { notify('请先选择账号', 'error'); return; }
			b.disabled = true;
			try {
				var r = await api('/api/accounts/' + CTX.selected + '/run/' + t[0], { method: 'POST' });
				notify(r.message || r.error || '任务已下发', r.error ? 'error' : 'success');
			} catch (e) { notify(e.message, 'error'); }
			finally { b.disabled = false; }
		});
		grid.appendChild(b);
	});
	host.appendChild(grid);
	host.appendChild(el('p', { style: 'color:#888;font-size:12px;margin-top:12px' }, '签到/挂机为异步任务，进度请到“总览”页或“实时日志”查看。'));
}

/* ============================================================
   积分商城
   ============================================================ */
async function viewRewards(c) {
	clearNode(c);
	var sec = el('div', { 'class': 'cbi-section' });
	sec.appendChild(el('h3', { 'class': 'cbi-section-title' }, '积分商城'));
	var host = el('div', { style: 'padding:12px' });
	sec.appendChild(host);
	c.appendChild(sec);

	try { CTX.accounts = await api('/api/accounts'); }
	catch (e) { notify(e.message, 'error'); CTX.accounts = []; }
	if (!CTX.accounts.length) { host.appendChild(el('p', { style: 'color:#888' }, '暂无账号。')); return; }

	var sel = el('select', { 'class': 'cbi-input' });
	CTX.accounts.forEach(function (a) {
		var o = el('option', { value: a.id }, ctyunAccLabel(a));
		if (a.id === CTX.selected) o.selected = true;
		sel.appendChild(o);
	});
	if (!CTX.selected) CTX.selected = CTX.accounts[0].id;
	sel.value = CTX.selected;
	sel.addEventListener('change', function () { CTX.selected = sel.value; viewRewards(c); });
	host.appendChild(field('兑换账号', sel));

	try { CTX.rewards = await api('/api/rewards'); }
	catch (e) { notify(e.message, 'error'); CTX.rewards = []; }
	if (CTX.rewards && CTX.rewards.error) { host.appendChild(el('p', { style: 'color:#e76f51' }, esc(CTX.rewards.error))); CTX.rewards = []; }

	var list = el('div', { style: 'margin-top:12px;display:grid;grid-template-columns:repeat(auto-fill,minmax(320px,1fr));gap:12px' });
	(CTX.rewards || []).forEach(function (r) {
		var card = el('div', { style: 'border:1px solid rgba(128,128,128,.25);border-radius:8px;overflow:hidden;background:rgba(128,128,128,.03)' });
		// 商品信息区
		var info = el('div', { style: 'padding:12px 14px 10px' }, [
			el('div', { style: 'display:flex;align-items:center;justify-content:space-between;gap:8px' }, [
				el('strong', { style: 'font-size:13px' }, esc(r.prodName || '商品')),
				el('span', { style: 'font-size:10px;font-weight:700;padding:2px 8px;border-radius:999px;background:rgba(106,138,163,.16);color:#3f5f7d;white-space:nowrap' }, esc(r.prodType || '商品'))
			]),
			el('div', { style: 'font-size:11px;color:#888;line-height:1.6;margin:6px 0 8px;min-height:18px' }, esc(r.description || '')),
			el('div', { style: 'font-size:16px;font-weight:800;color:#e76f51' }, [
				(r.costPoints || 0).toLocaleString('zh-CN') + ' ',
				el('small', { style: 'font-size:10px;font-weight:500;color:#888' }, '积分')
			])
		]);
		card.appendChild(info);
		// 操作区
		var ops = el('div', { style: 'display:flex;align-items:center;gap:8px;padding:10px 14px;border-top:1px solid rgba(128,128,128,.15);background:rgba(128,128,128,.05);flex-wrap:wrap' });
		var times = el('input', { 'class': 'cbi-input', type: 'number', min: '1', value: '1', style: 'width:58px' });
		ops.appendChild(times);
		var vmSel = el('select', { 'class': 'cbi-input', style: 'flex:1;min-width:120px' });
		vmSel.appendChild(el('option', { value: '' }, '选择云电脑'));
		var acc = CTX.accounts.filter(function (a) { return a.id === CTX.selected; })[0];
		(acc && acc.desktops || []).forEach(function (vm) {
			vmSel.appendChild(el('option', { value: vm.desktop_id }, esc(vm.desktopName || vm.desktop_id)));
		});
		ops.appendChild(vmSel);
		var b = el('button', { 'class': 'btn cbi-button cbi-button-apply', type: 'button' }, '兑换');
		b.addEventListener('click', async function () {
			b.disabled = true;
			try {
				var res = await api('/api/accounts/' + CTX.selected + '/order', { method: 'POST', body: {
					prodId: r.prodId, prodName: r.prodName, prodType: r.prodType,
					costPoints: r.costPoints, times: Number(times.value || 1), desktopId: vmSel.value
				} });
				notify(res.message || res.error || '已提交', res.success === false || res.error ? 'error' : 'success');
			} catch (e) { notify(e.message, 'error'); }
			finally { b.disabled = false; }
		});
		ops.appendChild(b);
		card.appendChild(ops);
		list.appendChild(card);
	});
	if (!(CTX.rewards || []).length) list.appendChild(el('p', { style: 'color:#888' }, '未获取到商品列表。'));
	host.appendChild(list);

	// 积分余额变动历史：峰峦面积图 + 默认折叠的明细
	var histBox = el('div', { style: 'margin-top:18px;border:1px solid #ddd;border-radius:8px;padding:14px' });
	histBox.appendChild(el('h4', { style: 'margin:0 0 10px' }, '积分余额历史'));
	host.appendChild(histBox);
	ctyunRenderHistory(histBox, CTX.accounts, CTX.selected);
}

/* 绘制积分余额峰峦面积图（纯 SVG 字符串），带默认折叠明细 */
function ctyunHistorySVG(hist) {
	var data = (hist || []).map(function (e) { return { v: Number(e.points || 0), ts: e.ts || '' }; });
	if (data.length < 2) return '<div style="padding:16px;text-align:center;color:#999;font-size:12px">再积累几次积分变动后显示余额走势图</div>';
	var W = 320, H = 90, PAD = 8;
	var vals = data.map(function (d) { return d.v; });
	var lo = Math.min.apply(null, vals), hi = Math.max.apply(null, vals);
	if (hi === lo) hi = lo + 1;
	var span = hi - lo;
	var step = (W - PAD * 2) / (data.length - 1);
	var pts = data.map(function (d, i) {
		return { x: PAD + i * step, y: PAD + (H - PAD * 2) * (1 - (d.v - lo) / span), v: d.v, ts: d.ts };
	});
	var line = 'M ' + pts[0].x.toFixed(1) + ' ' + pts[0].y.toFixed(1);
	for (var i = 1; i < pts.length; i++) {
		var p = pts[i - 1], q = pts[i], mx = (p.x + q.x) / 2;
		line += ' Q ' + p.x.toFixed(1) + ' ' + p.y.toFixed(1) + ' ' + mx.toFixed(1) + ' ' + ((p.y + q.y) / 2).toFixed(1);
	}
	line += ' T ' + pts[pts.length - 1].x.toFixed(1) + ' ' + pts[pts.length - 1].y.toFixed(1);
	var area = line + ' L ' + pts[pts.length - 1].x.toFixed(1) + ' ' + H + ' L ' + pts[0].x.toFixed(1) + ' ' + H + ' Z';
	var dots = pts.map(function (p) {
		return '<circle cx="' + p.x.toFixed(1) + '" cy="' + p.y.toFixed(1) + '" r="2.4" fill="#fff" stroke="#40916c" stroke-width="1.6"><title>' + esc(p.ts) + ' · ' + p.v + ' 积分</title></circle>';
	}).join('');
	return '<div style="font-size:12px;color:#666;margin:2px 0 6px">积分余额走势 <span style="float:right;color:#999;font-size:11px">最低 ' + lo + ' · 最高 ' + hi + '</span></div>'
		+ '<svg viewBox="0 0 ' + W + ' ' + H + '" preserveAspectRatio="none" style="display:block;width:100%;height:90px">'
		+ '<defs><linearGradient id="ctyPhGrad" x1="0" y1="0" x2="0" y2="1"><stop offset="0%" stop-color="#40916c" stop-opacity=".35"/><stop offset="100%" stop-color="#40916c" stop-opacity="0"/></linearGradient></defs>'
		+ '<path d="' + area + '" fill="url(#ctyPhGrad)"/><path d="' + line + '" fill="none" stroke="#2a9d8f" stroke-width="2" stroke-linejoin="round"/><g>' + dots + '</g></svg>';
}
function ctyunRenderHistory(box, accounts, selected) {
	clearNode(box);
	box.appendChild(el('h4', { style: 'margin:0 0 10px' }, '积分余额历史'));
	var withHist = (accounts || []).filter(function (a) { return a.stats && a.stats.points_history && a.stats.points_history.length; });
	if (!withHist.length) { box.appendChild(el('p', { style: 'color:#999;font-size:12px' }, '暂无余额变动记录，积分每次增减时自动记录（每 90 秒同步）。')); return; }
	var chosen = withHist.filter(function (a) { return a.id === selected; })[0] || withHist[0];
	var chrono = chosen.stats.points_history.slice();
	var desc = chrono.slice().reverse();
	var tip = el('div', { style: 'font-size:12px;color:#666;margin-bottom:8px' },
		'共 ' + chrono.length + ' 条记录（最多 200 条）· 当前余额 ' + (chosen.stats.points || 0));
	box.appendChild(tip);
	var chart = el('div');
	chart.innerHTML = ctyunHistorySVG(chrono);
	box.appendChild(chart);
	var fold = el('details', { style: 'margin-top:8px;border:1px solid #eee;border-radius:6px;padding:8px 10px' });
	fold.appendChild(el('summary', { style: 'cursor:pointer;font-size:12px;color:#666' }, '展开变动明细（' + chrono.length + ' 条）'));
	var rows = el('div', { style: 'margin-top:8px;max-height:320px;overflow-y:auto;overscroll-behavior:contain;-webkit-overflow-scrolling:touch;padding-right:6px' });
	desc.forEach(function (e) {
		var d = Number(e.delta || 0);
		var color = d > 0 ? '#2a9d8f' : (d < 0 ? '#e76f51' : '#888');
		rows.appendChild(el('div', { style: 'display:flex;justify-content:space-between;gap:8px;padding:6px 0;border-bottom:1px solid #f2f2f2;font-size:12px' }, [
			el('span', {}, [esc(e.reason || '余额变动') + ' ', el('small', { style: 'color:#aaa' }, esc(e.ts || ''))]),
			el('strong', { style: 'color:' + color }, (d > 0 ? '+' : '') + d + ' · 余额 ' + e.points)
		]));
	});
	fold.appendChild(rows);
	box.appendChild(fold);
}

/* ============================================================
   实时日志（轮询 /api/logs）
   ============================================================ */
async function viewLogs(c) {
	clearNode(c);
	var sec = el('div', { 'class': 'cbi-section' });
	var head = el('div', { style: 'display:flex;gap:8px;padding:10px 12px;align-items:center' });
	head.appendChild(el('h3', { 'class': 'cbi-section-title', style: 'margin:0;flex:1' }, '实时日志'));
	var clearBtn = el('button', { 'class': 'btn cbi-button', type: 'button' }, '清空');
	clearBtn.addEventListener('click', async function () {
		try { await api('/api/logs/clear', { method: 'POST' }); notify('已清空', 'success'); load(); } catch (e) { notify(e.message, 'error'); }
	});
	head.appendChild(clearBtn);
	sec.appendChild(head);
	var host = el('div', { id: 'cty-loghost', style: 'padding:0 12px 12px;max-height:60vh;overflow:auto;font-family:monospace;font-size:12px' });
	sec.appendChild(host);
	c.appendChild(sec);

	var SRC_LABEL = { Task: '任务', Scheduler: '调度', KeepAlive: '保活', Bms: 'BMS', Account: '账号', Auth: '登录', Upgrade: '升级', Notify: '通知', System: '系统', Api: '接口' };
	var LEVEL_LABEL = { success: '成功', info: '信息', warning: '警告', error: '错误' };
	async function load() {
		try {
			var logs = await api('/api/logs');
			clearNode(host);
			(logs || []).slice().reverse().forEach(function (l) {
				var color = l.level === 'error' || l.level === 'warning' ? '#e76f51' : l.level === 'success' ? '#2a9d8f' : '#555';
				var src = SRC_LABEL[l.source] || l.source || '系统';
				var lvl = LEVEL_LABEL[l.level] || '';
				var tagBg = l.level === 'error' ? 'rgba(231,111,81,.14)' : l.level === 'warning' ? 'rgba(180,83,9,.15)' : l.level === 'success' ? 'rgba(42,157,143,.14)' : 'rgba(106,138,163,.16)';
				host.appendChild(el('div', { style: 'display:grid;grid-template-columns:58px 88px minmax(0,1fr);align-items:baseline;gap:10px;padding:6px 0;border-bottom:1px solid rgba(128,128,128,.15)' }, [
					el('span', { style: 'color:#999;font-size:11px;white-space:nowrap' }, (l.timestamp || '').slice(11, 19)),
					el('span', { style: 'color:' + color + ';background:' + tagBg + ';padding:2px 7px;border-radius:6px;font-size:10px;font-weight:700;max-width:100%;text-align:center;white-space:nowrap' }, esc(src) + (lvl ? ' · ' + lvl : '')),
					el('span', { style: 'color:' + color + ';min-width:0;word-break:break-word' }, esc(l.message || '') + ((l.repeat_count || 1) > 1 ? ' ×' + l.repeat_count : ''))
				]));
			});
			if (!(logs || []).length) host.appendChild(el('div', { style: 'color:#999' }, '暂无日志。'));
		} catch (e) { notify(e.message, 'error'); }
	}
	load();
	CTX.logPoll = setInterval(load, 5000);
}

/* ============================================================
   系统设置（调度 / 通知 / 修改密码）
   ============================================================ */
async function viewSettings(c) {
	clearNode(c);
	var sec = el('div', { 'class': 'cbi-section' });
	sec.appendChild(el('h3', { 'class': 'cbi-section-title' }, '系统设置'));
	var host = el('div', { style: 'padding:12px' });
	sec.appendChild(host);
	c.appendChild(sec);

	// 登录验证开关（管理员）：内网可关闭以便免登录 —— 醒目卡片置于设置页顶部
	var authBox = el('div', { style: 'border:1px solid #b8c9d6;border-radius:8px;padding:14px 16px;margin-bottom:14px;background:rgba(106,138,163,.09)' });
	authBox.appendChild(el('h4', { style: 'margin:0 0 4px;font-size:15px' }, '登录验证'));
	authBox.appendChild(el('p', { style: 'margin:0 0 10px;font-size:11px;color:#888' }, '公网环境务必开启；仅纯内网、不对外暴露时可关闭以便免登录访问'));
	var authEn = el('input', { type: 'checkbox' });
	var authSw = el('label', { 'class': 'ctyun-switch' }, [authEn, el('span', { 'class': 'sw-ui' }), el('span', { 'class': 'sw-txt' }, '开启登录验证（关闭后免登录，仅限内网）')]);
	authBox.appendChild(authSw);
	var authCfg = null;
	try { authCfg = await api('/api/auth/config'); authEn.checked = authCfg.auth_required !== false; } catch (e) {}
	var saveAuth = el('button', { 'class': 'btn cbi-button cbi-button-apply', type: 'button' }, '保存验证设置');
	saveAuth.addEventListener('click', async function () {
		if (!(CTX.user && CTX.user.role === 'admin')) { notify('仅管理员可修改', 'error'); return; }
		saveAuth.disabled = true;
		try {
			var st = await api('/api/settings');
			st.auth_required = authEn.checked;
			await api('/api/settings', { method: 'PUT', body: st });
			notify('已保存' + (authEn.checked ? '' : '（已关闭登录验证，仅限内网使用）'), 'success');
		} catch (e) { notify(e.message, 'error'); }
		finally { saveAuth.disabled = false; }
	});
	authBox.appendChild(el('div', { style: 'margin-top:8px' }, saveAuth));
	host.appendChild(authBox);

	// 调度设置（管理员）
	var sched = el('div', { style: 'border:1px solid #ddd;border-radius:8px;padding:12px;margin-bottom:12px' });
	sched.appendChild(el('h4', { style: 'margin:0 0 8px' }, '定时任务调度'));
	var timeIn = el('input', { 'class': 'cbi-input', type: 'text', placeholder: 'HH:MM', value: '01:20' });
	var enSub = el('input', { type: 'checkbox' });
	var signIn = el('input', { 'class': 'cbi-input', type: 'text' });
	var aiIn = el('input', { 'class': 'cbi-input', type: 'text' });
	var hangIn = el('input', { 'class': 'cbi-input', type: 'text' });
	sched.appendChild(field('每日执行时间', timeIn));
	sched.appendChild(el('label', { 'class': 'ctyun-switch' }, [enSub, el('span', { 'class': 'sw-ui' }), el('span', { 'class': 'sw-txt' }, '启用独立子任务调度（各任务分别按 cron 执行）')]));
	sched.appendChild(field('签到 cron', signIn));
	sched.appendChild(field('AI 对话 cron', aiIn));
	sched.appendChild(field('挂机 cron', hangIn));
	var isAdmin = CTX.user && CTX.user.role === 'admin';
	if (isAdmin) {
		try {
			var st = await api('/api/settings');
			timeIn.value = st.cron && st.cron.execute_time || '01:20';
			enSub.checked = !!(st.cron && st.cron.enable_sub_cron);
			signIn.value = st.cron && st.cron.sign_cron || '';
			aiIn.value = st.cron && st.cron.ai_chat_cron || '';
			hangIn.value = st.cron && st.cron.cloud_hang_cron || '';
		} catch (e) {}
	}
	var saveSched = el('button', { 'class': 'btn cbi-button cbi-button-apply', type: 'button' }, '保存调度');
	saveSched.addEventListener('click', async function () {
		if (!isAdmin) { notify('仅管理员可修改', 'error'); return; }
		try {
			var st = await api('/api/settings');
			st.cron.execute_time = /^\d{1,2}:\d{2}$/.test(timeIn.value.trim()) ? timeIn.value.trim() : '01:20';
			st.cron.enable_sub_cron = enSub.checked;
			st.cron.sign_cron = signIn.value.trim() || st.cron.sign_cron;
			st.cron.ai_chat_cron = aiIn.value.trim() || st.cron.ai_chat_cron;
			st.cron.cloud_hang_cron = hangIn.value.trim() || st.cron.cloud_hang_cron;
			await api('/api/settings', { method: 'PUT', body: st });
			notify('调度已保存', 'success');
		} catch (e) { notify(e.message, 'error'); }
	});
	var triggerBtn = el('button', { 'class': 'btn cbi-button', type: 'button', style: 'margin-left:8px' }, '立即触发任务');
	triggerBtn.addEventListener('click', async function () {
		try { await api('/api/scheduler/trigger', { method: 'POST' }); notify('已触发', 'success'); }
		catch (e) { notify(e.message, 'error'); }
	});
	sched.appendChild(el('div', { style: 'margin-top:10px' }, [saveSched, triggerBtn]));
	host.appendChild(sched);

	// 通知设置
	var nt = el('div', { style: 'border:1px solid #ddd;border-radius:8px;padding:12px;margin-bottom:12px' });
	nt.appendChild(el('h4', { style: 'margin:0 0 8px' }, '消息通知'));
	var nEn = el('input', { type: 'checkbox' });
	var nCh = el('input', { 'class': 'cbi-input', type: 'text', placeholder: '通道（如 webhook）' });
	var nUrl = el('input', { 'class': 'cbi-input', type: 'text', placeholder: 'Webhook URL' });
	nt.appendChild(field('启用通知', nEn));
	nt.appendChild(field('通道', nCh));
	nt.appendChild(field('Webhook URL', nUrl));
	try {
		var n = await api('/api/user/notify');
		nEn.checked = !!n.enabled; nCh.value = n.channel || ''; nUrl.value = n.webhook_url || '';
	} catch (e) {}
	var saveN = el('button', { 'class': 'btn cbi-button cbi-button-apply', type: 'button' }, '保存通知');
	saveN.addEventListener('click', async function () {
		try {
			await api('/api/user/notify', { method: 'PUT', body: { enabled: nEn.checked, channel: nCh.value.trim(), webhook_url: nUrl.value.trim() } });
			notify('通知已保存', 'success');
		} catch (e) { notify(e.message, 'error'); }
	});
	var testBtn = el('button', { 'class': 'btn cbi-button', type: 'button', style: 'margin-left:8px' }, '发送测试');
	testBtn.addEventListener('click', async function () {
		try { var r = await api('/api/user/notify/test', { method: 'POST' }); notify(r.message || r.error || '已发送', r.success === false ? 'error' : 'success'); }
		catch (e) { notify(e.message, 'error'); }
	});
	nt.appendChild(el('div', { style: 'margin-top:10px' }, [saveN, testBtn]));
	host.appendChild(nt);

	// 在线升级（管理员）
	if (isAdmin) {
		var up = el('div', { style: 'border:1px solid #ddd;border-radius:8px;padding:12px;margin-bottom:12px' });
		up.appendChild(el('h4', { style: 'margin:0 0 8px' }, '在线升级'));
		var upInfo = el('div', { style: 'font-size:12px;margin-bottom:8px' }, '正在检查版本…');
		up.appendChild(upInfo);
		up.appendChild(el('div', { style: 'font-size:11px;color:#888;line-height:1.6;margin-bottom:10px' },
			'升级仅更新程序本体，账号、配置与全部数据均会保留；完成后服务自动重启，约需数秒。'));
		var upCheck = el('button', { 'class': 'btn cbi-button', type: 'button' }, '检查更新');
		var upDo = el('button', { 'class': 'btn cbi-button cbi-button-apply', type: 'button', style: 'margin-left:8px;display:none' }, '立即升级');
		up.appendChild(el('div', { style: 'margin-top:6px' }, [upCheck, upDo]));
		host.appendChild(up);
		ctyunBindUpgrade(null, upDo, upCheck, upInfo);
	}

	// 修改密码
	var pw = el('div', { style: 'border:1px solid #ddd;border-radius:8px;padding:12px' });
	pw.appendChild(el('h4', { style: 'margin:0 0 8px' }, '修改密码'));
	var oldP = el('input', { 'class': 'cbi-input', type: 'password' });
	var newP = el('input', { 'class': 'cbi-input', type: 'password' });
	pw.appendChild(field('原密码', oldP));
	pw.appendChild(field('新密码（至少 6 位）', newP));
	var saveP = el('button', { 'class': 'btn cbi-button cbi-button-apply', type: 'button' }, '修改密码');
	saveP.addEventListener('click', async function () {
		try {
			await api('/api/auth/change-password', { method: 'POST', body: { oldPassword: oldP.value, newPassword: newP.value } });
			notify('密码已修改', 'success'); oldP.value = ''; newP.value = '';
		} catch (e) { notify(e.message, 'error'); }
	});
	pw.appendChild(el('div', { style: 'margin-top:10px' }, saveP));
	host.appendChild(pw);

	// 用户管理（管理员）：卡片式列表
	if (isAdmin) {
		var um = el('div', { style: 'border:1px solid #ddd;border-radius:8px;padding:12px;margin-bottom:12px' });
		um.appendChild(el('h4', { style: 'margin:0 0 10px' }, '用户管理'));
		host.appendChild(um);
		ctyunRenderUsers(um);
	}

	// 关于（移至设置页）
	var about = el('div', { style: 'border:1px solid #ddd;border-radius:8px;padding:12px' });
	about.appendChild(el('h4', { style: 'margin:0 0 8px' }, '关于'));
	about.appendChild(el('p', { style: 'font-size:12px;color:#777;line-height:1.7;margin:0 0 8px' }, '基于 Rust · axum · tokio 构建，单文件部署，数据本地加密存储。'));
	about.appendChild(el('span', { id: 'cty-about-ver', style: 'font-size:11px;color:#999;font-weight:600' }, 'v—'));
	host.appendChild(about);
}

/* 用户管理：卡片式排版 */
async function ctyunRenderUsers(box) {
	var users = [];
	try { users = await api('/api/admin/users'); }
	catch (e) { box.appendChild(el('p', { style: 'color:#999;font-size:12px' }, '加载失败：' + e.message)); return; }
	var list = el('div', { style: 'display:flex;flex-direction:column;gap:10px' });
	(users || []).forEach(function (u) {
		var isAdmin = u.role === 'admin';
		var initial = (u.username || '?').trim().charAt(0).toUpperCase();
		var created = u.createdAt ? String(u.createdAt).slice(0, 16) : '—';
		var row = el('div', { style: 'display:flex;align-items:center;gap:12px;flex-wrap:wrap;border:1px solid #eee;border-radius:8px;padding:12px;background:#fff' }, [
			el('span', { style: 'width:40px;height:40px;border-radius:12px;display:grid;place-items:center;background:rgba(64,145,108,.14);color:#2a9d8f;font-weight:700;flex:none' }, initial),
			el('span', { style: 'flex:1;min-width:120px;display:flex;flex-direction:column' }, [
				el('strong', { style: 'font-size:13px' }, esc(u.username || '')),
				el('small', { style: 'color:#999;font-size:11px' }, '创建于 ' + esc(created))
			]),
			el('span', { style: 'font-size:11px;font-weight:600;padding:4px 10px;border-radius:999px;white-space:nowrap;color:' + (isAdmin ? '#2a6e58' : '#888') + ';background:' + (isAdmin ? 'rgba(64,145,108,.14)' : '#f2f2f2') }, isAdmin ? '管理员' : '普通用户')
		]);
		var reset = el('button', { 'class': 'btn cbi-button', type: 'button', style: 'margin-left:auto' }, '重置密码');
		reset.addEventListener('click', async function () {
			var p = await uiConfirm('重置密码', '为用户「' + (u.username || '') + '」设置新密码，至少 6 位。', '确认重置', '新密码', '');
			if (!p) return;
			reset.disabled = true;
			try { await api('/api/admin/users/' + u.id + '/password', { method: 'PUT', body: { password: p } }); notify('密码已重置', 'success'); }
			catch (e) { notify(e.message, 'error'); }
			finally { reset.disabled = false; }
		});
		row.appendChild(reset);
		list.appendChild(row);
	});
	if (!users.length) list.appendChild(el('p', { style: 'color:#999' }, '暂无用户。'));
	box.appendChild(list);
}

/* ============================================================
   注入样式：让自定义 DOM 融入 LuCI 主题
   ============================================================ */
function buildStyle() {
	var css = ''
		+ '.ctyun-app .cbi-section{margin-bottom:12px}'
		+ '.ctyun-app .cbi-value{display:flex;align-items:center;gap:8px;margin:6px 0}'
		+ '.ctyun-app .cbi-value-title{width:150px;flex:none;text-align:right;color:#666}'
		+ '.ctyun-app .cbi-value-field{flex:1}'
		+ '.ctyun-app .cbi-input{padding:5px 8px;border:1px solid #ccc;border-radius:4px}'
		+ '.ctyun-app table.table th,.ctyun-app table.table td{vertical-align:top}'
		+ '.ctyun-app .btn.active{box-shadow:inset 0 0 0 2px rgba(0,0,0,.15)}'
		+ '.ctyun-app h4{margin-top:4px}'
		/* 醒目开关：跟随 LuCI 主题，替代原生小复选框 */
		+ '.ctyun-switch{display:inline-flex;align-items:center;gap:10px;cursor:pointer;user-select:none}'
		+ '.ctyun-switch input{position:absolute;opacity:0;width:0;height:0}'
		+ '.ctyun-switch .sw-ui{width:44px;height:25px;flex:none;border-radius:999px;background:rgba(128,128,128,.28);position:relative;transition:background .2s,box-shadow .2s;border:1px solid rgba(128,128,128,.3)}'
		+ '.ctyun-switch .sw-ui::after{content:"";position:absolute;top:3px;left:3px;width:17px;height:17px;border-radius:50%;background:#fff;box-shadow:0 1px 3px rgba(0,0,0,.25);transition:transform .2s}'
		+ '.ctyun-switch input:checked + .sw-ui{background:#3f7f6f;border-color:#3f7f6f;box-shadow:0 0 0 3px rgba(63,127,111,.18)}'
		+ '.ctyun-switch input:checked + .sw-ui::after{transform:translateX(19px)}'
		+ '.ctyun-switch input:focus-visible + .sw-ui{outline:2px solid #3f7f6f;outline-offset:2px}'
		+ '.ctyun-switch .sw-txt{font-size:12px;color:var(--text-color,#333)}'
		/* 主题化确认弹窗：用 LuCI 主题变量，深浅色都跟随 */
		+ '.ctyun-confirm-mask{position:fixed;inset:0;z-index:10000;display:flex;align-items:flex-start;justify-content:center;overflow-y:auto;padding:6vh 20px 24px;background:rgba(0,0,0,.45);backdrop-filter:blur(4px)}'
		+ '.ctyun-confirm-box{width:min(100%,420px);margin:auto;max-height:88vh;overflow:auto;padding:20px 22px;border-radius:10px;background:var(--background-color,#fff);color:var(--text-color,#333);border:1px solid var(--border-color,#ddd);box-shadow:0 20px 60px -20px rgba(0,0,0,.5)}';
	var s = document.createElement('style');
	s.setAttribute('type', 'text/css');
	s.appendChild(document.createTextNode(css));
	return s;
}
CTYUN_VIEW_JS
log "LuCI 原生视图已内嵌写入 ($(wc -c < /www/luci-static/resources/view/ctyun-dashboard/status.js) 字节)"

# ------------------------------------------------------------
# 4. 校验文件已写入
# ------------------------------------------------------------
FAIL=0
for f in \
  /usr/share/luci/menu.d/luci-app-ctyun-dashboard.json \
  /usr/share/rpcd/acl.d/luci-app-ctyun-dashboard.json \
  /www/luci-static/resources/view/ctyun-dashboard/status.js; do
  if [ -s "$f" ]; then log "[OK]   $f"; else err "[缺失] $f"; FAIL=1; fi
done

# JSON 语法抽查（有 python3 才做）
if command -v python3 >/dev/null 2>&1; then
  python3 -c "import json;json.load(open('/usr/share/luci/menu.d/luci-app-ctyun-dashboard.json'));json.load(open('/usr/share/rpcd/acl.d/luci-app-ctyun-dashboard.json'))" 2>/dev/null \
    && log "JSON 语法校验通过" || { err "JSON 语法错误"; FAIL=1; }
fi

# ------------------------------------------------------------
# 5. 清缓存 + 重启 rpcd / uhttpd（现代 LuCI 需要重载才扫描到新菜单）
# ------------------------------------------------------------
rm -rf /tmp/luci-*cache /tmp/luci-modulecache /tmp/luci-indexcache 2>/dev/null || true
if [ -x /etc/init.d/rpcd ]; then /etc/init.d/rpcd restart >/dev/null 2>&1 || true; fi
if [ -x /etc/init.d/uhttpd ]; then /etc/init.d/uhttpd restart >/dev/null 2>&1 || true; fi
log "已重启 rpcd / uhttpd 刷新菜单"

line
if [ "$FAIL" = "0" ]; then
  log "修复完成。请硬刷新 LuCI 页面（Ctrl+F5），菜单在：服务 -> 天翼云电脑"
  log "独立面板：http://$(uci get network.lan.ipaddr 2>/dev/null | awk -F/ '{print $1}'):8571"
else
  err "部分文件写入失败，请把以上输出发回开发者"
fi
line
