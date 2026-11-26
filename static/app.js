'use strict';

/* ============================================================
   云电脑控制中心 · 前端逻辑
   关键设计：增量 DOM 更新 —— 轮询只改变化的文本/宽度，
   绝不 innerHTML 全量重建，彻底消除面板闪烁。
   ============================================================ */

const S = {
  token: localStorage.getItem('ctyun_token') || '',
  user: null, accounts: [], status: null,
  currentView: 'dashboard', selectedAccount: '',
  pollTimer: null, es: null, qrTimer: null, qrData: null,
  logs: [], logSig: '', accountSig: '', overviewSig: '', taskSig: '',
  refreshBusy: false,
};

const $ = (s) => document.querySelector(s);
const $$ = (s) => Array.from(document.querySelectorAll(s));
const esc = (v) => String(v ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const icon = (n, cls = '') => `<svg class="${cls}"><use href="#i-${n}"/></svg>`;
const sig = (v) => { try { return JSON.stringify(v); } catch { return String(v); } };
// 账号显示名：占位名（扫码账号）回退手机号；其余附加手机号后 4 位，避免多个账号同名分不清。
function accLabel(a) {
  const name = esc(a?.name || '') || '';
  const user = esc(a?.user || '') || '';
  const base = (name && name !== '扫码账号') ? name : (user || '云电脑账号');
  const last4 = user.replace(/\s/g, '').slice(-4);
  if (!last4 || base.includes(user) || base.includes(last4)) return base;
  return `${base} · ${last4}`;
}

/* ---------- 增量更新工具：值不变则零操作 ---------- */
function setText(id, value) {
  const el = document.getElementById(id);
  if (!el) return;
  const v = String(value ?? '');
  if (el.textContent !== v) el.textContent = v;
}
function setNum(id, value, flash = true) {
  const el = document.getElementById(id);
  if (!el) return;
  const v = Number(value || 0).toLocaleString('zh-CN');
  if (el.textContent === v) return;           // 值未变：不触碰 DOM
  el.textContent = v;
  if (flash) {
    el.classList.remove('flash');
    void el.offsetWidth;                      // 触发重排以重启动画
    el.classList.add('flash');
  }
}
function setWidth(id, pct) {
  const el = document.getElementById(id);
  if (!el) return;
  const i = el.firstElementChild;
  const w = `${Math.max(0, Math.min(100, pct))}%`;
  if (i && i.style.width !== w) i.style.width = w;
}

/* ============ 主题系统 ============ */
const THEME_KEY = 'ctyun_theme', ACCENT_KEY = 'ctyun_accent';
function systemDark() { return window.matchMedia('(prefers-color-scheme: dark)').matches; }
function applyTheme() {
  const mode = localStorage.getItem(THEME_KEY) || 'light';
  const theme = mode === 'auto' ? (systemDark() ? 'dark' : 'light') : mode;
  document.documentElement.setAttribute('data-theme', theme);
  const accent = localStorage.getItem(ACCENT_KEY) || 'teal';
  document.documentElement.setAttribute('data-accent', accent);
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content', theme === 'dark' ? '#151a1d' : '#eef0ec');
  $$('[data-theme-set]').forEach(b => b.classList.toggle('active', b.dataset.themeSet === mode));
  $$('[data-accent-set]').forEach(b => b.classList.toggle('active', b.dataset.accentSet === accent));
}
function initTheme() {
  applyTheme();
  $$('[data-theme-set]').forEach(b => b.addEventListener('click', () => {
    localStorage.setItem(THEME_KEY, b.dataset.themeSet);
    applyTheme();
  }));
  $$('[data-accent-set]').forEach(b => b.addEventListener('click', () => {
    localStorage.setItem(ACCENT_KEY, b.dataset.accentSet);
    applyTheme();
  }));
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
    if ((localStorage.getItem(THEME_KEY) || 'light') === 'auto') applyTheme();
  });
}

/* ============ API ============ */
async function api(path, options = {}) {
  const opts = { ...options, headers: { ...(options.headers || {}) } };
  const reqToken = S.token;                       // 记录发起请求时的 token
  if (reqToken) opts.headers['Authorization'] = `Bearer ${reqToken}`;
  if (opts.body && typeof opts.body !== 'string' && !(opts.body instanceof FormData)) {
    opts.headers['Content-Type'] = 'application/json';
    opts.body = JSON.stringify(opts.body);
  }
  let res;
  try { res = await fetch(path, opts); }
  catch (e) { throw new Error('网络连接失败: ' + e.message); }
  if (res.status === 401) {
    // 只有当 token 未被换新时才登出：避免启动时的旧 /me 请求延迟返回，
    // 把已经用新 token 登录成功的会话误踢回登录页（竞态）。
    if (S.token === reqToken) logout(true);
    throw new Error('登录已过期，请重新登录');
  }
  const text = await res.text();
  let data; try { data = text ? JSON.parse(text) : null; } catch { data = text; }
  if (!res.ok) throw new Error((data && (data.error || data.message)) || `请求失败 (${res.status})`);
  return data;
}

function toast(message, type = 'info') {
  const el = document.createElement('div');
  el.className = `toast ${type}`;
  const ic = type === 'success' ? 'check' : type === 'error' ? 'close' : type === 'warning' ? 'bell' : 'bolt';
  el.innerHTML = `${icon(ic)}<span>${esc(message)}</span>`;
  $('#toast-stack').appendChild(el);
  setTimeout(() => { el.classList.add('out'); setTimeout(() => el.remove(), 300); }, 3000);
}

/* ============ 主题化确认/输入弹窗（替代原生 confirm/prompt） ============ */
let _confirmResolve = null;
function closeConfirm(result) {
  $('#confirm-modal').classList.add('hidden');
  if (_confirmResolve) { const r = _confirmResolve; _confirmResolve = null; r(result); }
}
/** uiConfirm(title, desc, okText) -> Promise<boolean> */
function uiConfirm(title, desc, okText = '确定') {
  return new Promise((resolve) => {
    _confirmResolve = resolve;
    setText('confirm-title', title || '确认操作');
    setText('confirm-desc', desc || '');
    $('#confirm-input-wrap').style.display = 'none';
    $('#confirm-ok').textContent = okText;
    $('#confirm-modal').classList.remove('hidden');
  });
}
/** uiPrompt(title, desc, label, defaultValue) -> Promise<string|null> */
function uiPrompt(title, desc, label, defaultValue = '') {
  return new Promise((resolve) => {
    _confirmResolve = resolve;
    setText('confirm-title', title || '输入');
    setText('confirm-desc', desc || '');
    setText('confirm-input-label', label || '输入');
    $('#confirm-input').value = defaultValue;
    $('#confirm-input-wrap').style.display = '';
    $('#confirm-ok').textContent = '确定';
    $('#confirm-modal').classList.remove('hidden');
    setTimeout(() => $('#confirm-input')?.focus(), 50);
  });
}
$('#confirm-ok')?.addEventListener('click', () => {
  const inputing = $('#confirm-input-wrap').style.display !== 'none';
  closeConfirm(inputing ? ($('#confirm-input').value || null) : true);
});
$('#confirm-cancel')?.addEventListener('click', () => closeConfirm(null));

/* ============ Boot / Auth ============ */
async function boot() {
  initTheme();
  // 先查询鉴权模式：内网关闭鉴权时免登录，直接以管理员身份进入
  try {
    const cfg = await fetch('/api/auth/config').then(r => r.ok ? r.json() : null);
    if (cfg && cfg.auth_required === false) {
      S.user = { id: 'u_admin', username: 'admin', role: 'admin', maxQuota: 999 };
      showApp();
      return;
    }
  } catch (_) { /* 查询失败则按需要登录处理 */ }
  if (S.token) {
    try { S.user = await api('/api/auth/me'); showApp(); return; } catch (_) { /* fall through */ }
  }
  finishBootToLogin();
}
function finishBootToLogin() {
  $('#boot').classList.add('done');
  $('#login-view').classList.remove('hidden');
}
function showApp() {
  $('#boot').classList.add('done');
  $('#login-view').classList.add('hidden');
  $('#app-view').classList.remove('hidden');
  setText('user-name', S.user?.username || '用户');
  setText('user-role', S.user?.role === 'admin' ? '管理员' : '普通用户');
  if (S.user?.role !== 'admin') $$('.admin-only').forEach(e => e.classList.add('hidden'));
  initApp();
}

$('#login-form').addEventListener('submit', async (e) => {
  e.preventDefault();
  const btn = $('#login-submit'); btn.classList.add('loading');
  try {
    const data = await api('/api/auth/login', { method: 'POST', body: {
      username: $('#login-user').value.trim(), password: $('#login-pass').value,
      remember: $('#login-remember').checked,
    }});
    S.token = data.token; S.user = data.user;
    localStorage.setItem('ctyun_token', S.token);
    showApp(); toast('登录成功，欢迎回来', 'success');
  } catch (err) { toast(err.message, 'error'); }
  finally { btn.classList.remove('loading'); }
});

function logout(silent = false) {
  api('/api/auth/logout', { method: 'POST' }).catch(() => {});
  S.token = ''; S.user = null;
  localStorage.removeItem('ctyun_token');
  if (S.es) { S.es.close(); S.es = null; }
  if (S.pollTimer) clearInterval(S.pollTimer);
  if (S.qrTimer) { clearInterval(S.qrTimer); S.qrTimer = null; }
  $('#app-view').classList.add('hidden');
  $('#login-view').classList.remove('hidden');
  if (!silent) toast('已退出登录', 'info');
}

/* ============ Navigation ============ */
const viewMeta = {
  dashboard: ['总览面板', '实时监控所有云电脑的运行状态'],
  accounts: ['云电脑', '管理天翼云电脑账号与实例'],
  rewards: ['积分商城', '查看积分余额并兑换升配包、数据盘等商品'],
  tasks: ['任务中心', '执行签到、挂机、兑换等自动化任务'],
  logs: ['实时日志', '服务器推送的运行事件流'],
  settings: ['系统设置', '调度、通知、外观与用户权限'],
};
function switchView(name) {
  S.currentView = name;
  $$('.nav-item').forEach(b => b.classList.toggle('active', b.dataset.view === name));
  $$('.dock-item').forEach(b => b.classList.toggle('active', b.dataset.goto === name));
  $$('.view').forEach(v => v.classList.toggle('active', v.id === `view-${name}`));
  setText('page-title', viewMeta[name][0]);
  setText('page-subtitle', viewMeta[name][1]);
  closeSidebar();
  loadView(name);
}
$$('.nav-item').forEach(b => b.addEventListener('click', () => switchView(b.dataset.view)));
$$('[data-goto]').forEach(b => b.addEventListener('click', () => switchView(b.dataset.goto)));

/* ============ 移动端侧栏开合（修复：之前 menu 按钮未绑定） ============ */
function openSidebar() {
  $('#sidebar')?.classList.add('open');
  $('#sidebar-scrim')?.classList.add('show');
  document.body.classList.add('sidebar-open'); // 侧栏打开时隐藏 dock，避免遮挡侧栏底部
}
function closeSidebar() {
  $('#sidebar')?.classList.remove('open');
  $('#sidebar-scrim')?.classList.remove('show');
  document.body.classList.remove('sidebar-open');
}
$('#mobile-menu')?.addEventListener('click', () => {
  $('#sidebar').classList.contains('open') ? closeSidebar() : openSidebar();
});
$('#sidebar-scrim')?.addEventListener('click', closeSidebar);
$('#logout-btn').addEventListener('click', () => logout());
$('#refresh-btn').addEventListener('click', async () => { await refreshAll(true); toast('数据已刷新', 'success'); });
setInterval(() => setText('clock', new Date().toLocaleTimeString('zh-CN', { hour12: false })), 1000);
setText('clock', new Date().toLocaleTimeString('zh-CN', { hour12: false }));
// focus 监听只注册一次（全局），避免多次登录叠加导致重复请求
window.addEventListener('focus', () => { if (S.token && !document.hidden) refreshAll(false); });

function initApp() {
  switchView('dashboard');
  refreshAll();
  // 总览页版本卡片：管理员可见，绑定事件并静默检查一次
  if (S.user?.role === 'admin') {
    $('#dash-ver-check')?.addEventListener('click', () => checkUpgrade(true));
    $('#dash-ver-upgrade')?.addEventListener('click', doUpgrade);
    checkUpgrade(false);
  }
  connectSSE();
  // 单一轮询：一次拿 status + accounts，增量更新；4 秒。先清旧定时器防叠加。
  if (S.pollTimer) clearInterval(S.pollTimer);
  S.pollTimer = setInterval(() => { if (!document.hidden) refreshAll(false); }, 4000);
}

async function loadView(name) {
  try {
    if (name === 'dashboard') await refreshDashboard();
    else if (name === 'accounts') await loadAccounts();
    else if (name === 'rewards') { await loadAccounts(); await loadRewards(); }
    else if (name === 'tasks') { await loadAccounts(); renderTaskAccountSelect(); await loadCron(); }
    else if (name === 'logs') await loadLogs();
    else if (name === 'settings') await loadSettings();
  } catch (e) { toast(e.message, 'error'); }
}

/* ============ 统一刷新（合并请求、增量渲染） ============ */
async function refreshAll(force = false) {
  if (S.refreshBusy) return;
  S.refreshBusy = true;
  try {
    const needAccounts = force || S.currentView === 'dashboard' || S.currentView === 'accounts' || S.currentView === 'tasks';
    const [status, accounts] = await Promise.all([
      api('/api/status'),
      needAccounts ? api('/api/accounts') : Promise.resolve(S.accounts),
    ]);
    S.status = status;
    if (needAccounts) S.accounts = accounts;
    renderKPIs(status);
    if (S.currentView === 'dashboard') {
      renderMonitor(accounts, status);
      renderTasks(status);
      renderOverview(accounts);
    }
    if (S.currentView === 'accounts') renderAccounts(accounts);
    if (S.currentView === 'tasks' && force) renderTaskAccountSelect();
  } catch (e) { /* 轮询失败静默，避免打扰 */ }
  finally { S.refreshBusy = false; }
}
async function refreshDashboard() { await refreshAll(true); }

/* ============ KPI：增量更新，值不变零操作 ============ */
function renderKPIs(s) {
  if (!s) return;
  setNum('val-accounts', s.accountsTotal);
  setNum('val-online', s.onlineKeepAlive);
  setNum('val-sign', s.signedToday);
  setNum('val-points', s.totalEarnedPoints);
}

/* ============ Monitor：增量 ============ */
function renderMonitor(accounts, status) {
  const list = Array.isArray(accounts) ? accounts : [];
  const online = list.filter(a => a.stats?.keep_alive_status === 'online').length;
  const totalVms = list.reduce((n, a) => n + (a.desktops?.length || 0), 0);
  const errors = list.filter(a => a.session_expired || a.stats?.last_error).length;
  const pct = list.length ? Math.round(online / list.length * 100) : 0;
  const taskPct = list.length ? Math.round((status.signedToday || 0) * 100 / list.length) : 0;
  const healthPct = list.length ? Math.round((list.length - errors) * 100 / list.length) : 100;
  setWidth('meter-online', pct); setText('pct-online', `${pct}%`);
  setWidth('meter-task', Math.min(100, taskPct)); setText('pct-task', `${status.signedToday || 0}/${list.length}`);
  setWidth('meter-health', healthPct); setText('pct-health', errors ? `异常 ${errors}` : '正常');
  setText('stat-vms', totalVms); setText('stat-conn', status.onlineKeepAlive || 0); setText('stat-err', errors);
}

/* ============ Tasks：直接改静态行，不重建 ============ */
function renderTasks(status) {
  const details = status.pointsDetails || [];
  // 汇总每个任务的完成状态与真实进度（current/total，秒），用于进度条。
  const allTasks = details.flatMap(a => a.tasks || []);
  const pick = (pred) => allTasks.find(pred);
  const done = {
    sign: (status.signedToday || 0) > 0 || allTasks.some(t => t.name.includes('登录AI云电脑') && t.completed),
    ai: allTasks.some(t => t.name.includes('AI对话') && t.completed),
    hang: allTasks.some(t => (t.name.includes('使用1小时') || t.name.includes('挂机')) && t.completed),
  };
  // 挂机进度：取所有账号里进度最大的那个「使用1小时」任务
  const hangTask = pick(t => (t.name.includes('使用1小时') || t.name.includes('挂机')));
  const pctOf = (t) => {
    if (!t) return 0;
    const m = String(t.progress || '').match(/^(\d+)\/(\d+)$/);
    if (!m) return 0;
    const cur = Number(m[1]), total = Number(m[2]);
    return total > 0 ? Math.min(100, Math.round(cur / total * 100)) : 0;
  };
  $$('#task-body .task-item').forEach(row => {
    const key = row.dataset.task;
    const d = !!done[key];
    const check = row.querySelector('.task-check');
    const small = row.querySelector('.task-meta-top small');
    const bar = row.querySelector('.task-progress i');
    if (check) check.classList.toggle('done', d);
    if (bar) {
      // 挂机显示真实进度；其余任务保持二态
      const w = d ? '100%' : (key === 'hang' ? pctOf(hangTask) + '%' : '0%');
      if (bar.style.width !== w) bar.style.width = w;
    }
    if (small) {
      let label = d ? '已完成' : '待执行';
      if (!d && key === 'hang') {
        const m = String(hangTask?.progress || '').match(/^(\d+)\/(\d+)$/);
        if (m) label = `进行中 ${Math.round(Number(m[1]) / 60)}/${Math.round(Number(m[2]) / 60)} 分钟`;
      }
      if (small.textContent !== label) small.textContent = label;
    }
  });
}

/* ============ 积分明细弹窗 ============ */
$('#kpi-points').addEventListener('click', openPointsDetailModal);
function openPointsDetailModal() {
  const details = S.status?.pointsDetails || [];
  const list = $('#points-detail-list');
  setText('points-modal-sum', S.status?.totalEarnedPoints || 0);
  if (!details.length) {
    list.innerHTML = '<div class="points-empty">暂无云电脑今日积分明细</div>';
  } else {
    list.innerHTML = details.map(acc => {
      const rows = (acc.tasks || []).map(t => {
        const d = t.completed || t.points > 0;
        // 进度 current/total 对"使用1小时"是秒，转成 X/Y 分钟更直观；其它任务原样显示。
        let prog = esc(t.progress || '0/1');
        const pm = String(t.progress || '').match(/^(\d+)\/(\d+)$/);
        if (!d && pm && Number(pm[2]) >= 600 && (t.name.includes('使用1小时') || t.name.includes('挂机'))) {
          prog = `${Math.round(Number(pm[1]) / 60)}/${Math.round(Number(pm[2]) / 60)} 分钟`;
        }
        // 进度百分比：用于未完成任务的细进度条（挂机可直观看到在走）。
        let pct = 0;
        if (!d && pm && Number(pm[2]) > 0) pct = Math.min(100, Math.round(Number(pm[1]) / Number(pm[2]) * 100));
        const pts = Number(t.points) || 0;
        return `<div class="points-row ${d ? 'is-done' : 'is-pending'}">
          <span class="points-row-icon">${icon(d ? 'check' : 'clock')}</span>
          <div class="points-row-main">
            <div class="points-row-top"><strong class="points-row-name">${esc(t.name)}</strong>
              <span class="points-badge ${d ? 'done' : ''}">${d ? '+' + pts + ' 积分' : '+' + pts + ' 待领'}</span></div>
            ${d
              ? `<div class="points-row-sub"><span class="points-row-time">${esc(t.completedAt || '今日已达成')}</span></div>`
              : `<div class="points-row-prog"><i style="width:${pct}%"></i></div><div class="points-row-sub"><span class="points-row-pending">进行中 · ${prog}</span></div>`}
          </div>
        </div>`;
      }).join('');
      const today = Number(acc.todayPoints) || 0;
      const total = Number(acc.totalPoints) || 0;
      return `<div class="points-acc">
        <div class="points-acc-head">
          <div class="points-acc-id"><span class="points-acc-ic">${icon('monitor')}</span><strong>${esc(acc.accountName)}</strong></div>
          <div class="points-acc-nums"><span class="pan-today">今日 <b>+${today}</b></span><span class="pan-total">总积分 <b>${total}</b></span></div>
        </div>
        <div class="points-acc-body">${rows}</div></div>`;
    }).join('');
  }
  // 默认回到"今日任务"页，并渲染余额历史
  switchPtTab('today');
  renderPointsHistory();
  $('#points-modal').classList.remove('hidden');
}

/* 积分弹窗内部 tab（独立于全局 .tab，避免影响登录/账号弹窗） */
function switchPtTab(name) {
  $$('.ptab').forEach(t => t.classList.toggle('active', t.dataset.ptab === name));
  $$('.ptab-pane').forEach(p => p.classList.toggle('active', p.id === `ptab-${name}`));
  if (name === 'history') renderPointsHistory();
}
$$('.ptab').forEach(t => t.addEventListener('click', () => switchPtTab(t.dataset.ptab)));

/* 渲染某账号的积分余额变动历史（倒序，最近在上） */
function renderPointsHistory() {
  const sel = $('#hist-account');
  const list = $('#points-history-list');
  if (!sel || !list) return;
  const accs = (S.accounts || []).filter(a => a.stats && Array.isArray(a.stats.points_history) && a.stats.points_history.length);
  if (!accs.length) {
    sel.innerHTML = '';
    sel.classList.add('hidden');
    list.innerHTML = '<div class="points-empty">暂无余额变动记录。积分每次发生增减时会自动记录（每 90 秒同步一次）。</div>';
    return;
  }
  sel.classList.remove('hidden');
  const keep = sel.value;
  sel.innerHTML = accs.map(a => `<option value="${esc(a.id)}">${accLabel(a)} · ${a.stats.points || 0} 积分</option>`).join('');
  if (keep && accs.some(a => a.id === keep)) sel.value = keep;
  const chosen = accs.find(a => a.id === sel.value) || accs[0];
  // 图表按时间正序（旧→新），明细倒序（新→旧）
  const chrono = [...chosen.stats.points_history];
  const descending = [...chrono].reverse();
  const cur = Number(chosen.stats.points || 0);
  const chart = buildPointsChart(chrono);
  const rows = descending.map(e => {
    const delta = Number(e.delta || 0);
    const cls = delta > 0 ? 'up' : (delta < 0 ? 'down' : 'flat');
    const sign = delta > 0 ? '+' : '';
    return `<div class="ph-row">
      <div class="ph-main"><span class="ph-dot ${cls}">${delta > 0 ? '↑' : (delta < 0 ? '↓' : '·')}</span>
        <strong>${esc(e.reason || '余额变动')}</strong>
        <span class="ph-delta ${cls}">${sign}${delta}</span></div>
      <div class="ph-meta"><span>${esc(e.ts)}</span><code>余额 ${e.points}</code></div>
    </div>`;
  }).join('');
  // 明细默认折叠（details 无 open），趋势图始终可见
  list.innerHTML = `<div class="ph-tip">共 ${chrono.length} 条记录（最多 200 条）· 当前余额 <b>${cur}</b></div>` +
    chart +
    `<details class="ph-detail-fold"><summary>${icon('chevron')}<span>展开变动明细</span><small>${chrono.length} 条</small></summary>
      <div class="ph-detail-body">${rows}</div></details>`;
}
$('#hist-account')?.addEventListener('change', renderPointsHistory);

/* 生成积分余额峰峦面积图（纯 SVG，无外部依赖） */
function buildPointsChart(chron) {
  const data = (chron || []).map(e => ({ v: Number(e.points || 0), ts: e.ts || '' }));
  if (data.length < 2) {
    return `<div class="ph-chart-empty">再积累几次积分变动后，这里会显示余额走势图</div>`;
  }
  const W = 320, H = 96, PAD = 8;
  const vals = data.map(d => d.v);
  let lo = Math.min(...vals), hi = Math.max(...vals);
  if (hi === lo) { hi = lo + 1; }         // 全平线时留出纵向空间
  const span = hi - lo;
  const stepX = (W - PAD * 2) / (data.length - 1);
  const pts = data.map((d, i) => ({
    x: PAD + i * stepX,
    y: PAD + (H - PAD * 2) * (1 - (d.v - lo) / span),
    v: d.v, ts: d.ts
  }));
  // 平滑折线（Catmull-Rom 简化为二次中点）
  let line = `M ${pts[0].x.toFixed(1)} ${pts[0].y.toFixed(1)}`;
  for (let i = 1; i < pts.length; i++) {
    const prev = pts[i - 1], cur = pts[i];
    const mx = (prev.x + cur.x) / 2;
    line += ` Q ${prev.x.toFixed(1)} ${prev.y.toFixed(1)} ${mx.toFixed(1)} ${((prev.y + cur.y) / 2).toFixed(1)}`;
  }
  line += ` T ${pts[pts.length - 1].x.toFixed(1)} ${pts[pts.length - 1].y.toFixed(1)}`;
  const area = `${line} L ${pts[pts.length - 1].x.toFixed(1)} ${H} L ${pts[0].x.toFixed(1)} ${H} Z`;
  const dots = pts.map(p => `<circle cx="${p.x.toFixed(1)}" cy="${p.y.toFixed(1)}" r="2.4"><title>${esc(p.ts)} · ${p.v} 积分</title></circle>`).join('');
  return `<div class="ph-chart">
    <div class="ph-chart-head"><span>积分余额走势</span><small>最低 ${lo} · 最高 ${hi}</small></div>
    <svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" aria-label="积分余额趋势图">
      <defs><linearGradient id="phGrad" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0%" stop-color="var(--mist)" stop-opacity=".38"/>
        <stop offset="100%" stop-color="var(--mist)" stop-opacity="0"/>
      </linearGradient></defs>
      <path class="ph-area" d="${area}"/>
      <path class="ph-line" d="${line}"/>
      <g class="ph-dots">${dots}</g>
    </svg></div>`;
}

/* ============ Account cards（签名缓存，数据不变不重建） ============ */
function vmStatus(d) {
  // 真实电源状态优先（useStatusText / useStatus==25=运行中，对齐上游）。
  const txt = d?.use_status_text || '';
  const code = d?.use_status;
  if (txt.includes('运行') || code === 25) return ['online', '运行中'];
  if (txt.includes('休眠') || txt.includes('睡眠')) return ['standby', '休眠'];
  if (txt.includes('关机') || txt.includes('停止')) return ['offline', '已关机'];
  if (txt) return ['offline', txt];           // 未知文本按原样显示
  // 没有电源状态时才退回保活状态（仅作参考，不代表电源）
  const s = d?.live_metrics?.status;
  if (s === 'online') return ['online', '保活中'];
  if (s === 'error') return ['error', '异常'];
  return ['offline', '状态未知'];
}
function desktopCard(d, accId, accErr = '') {
  const st = vmStatus(d);
  const lm = d.live_metrics || {};
  const statusText = st[1];
  // 只有异常状态才显示错误条；成功结果（如"脉冲周期完成"）不该被当成错误标红。
  const failed = lm.status === 'error';
  const err = failed ? (lm.last_heartbeat_result || accErr || '保活异常') : '';
  const specs = [d.cpu, d.memory, d.os].filter(Boolean);
  const specIcons = ['cpu', 'layers', 'monitor'];
  const specHtml = specs.map((v, i) => `<span class="vm-spec">${icon(specIcons[i])}<em>${esc(v)}</em></span>`).join('');
  const meta = [];
  if (d.image_name) meta.push(`<span title="镜像">${icon('layers')}<em>${esc(d.image_name)}</em></span>`);
  if (d.flavor_name) meta.push(`<span title="规格">${icon('server')}<em>${esc(d.flavor_name)}</em></span>`);
  const pulseTxt = lm.countdown ? `下次保活 ${lm.countdown}s` :
    (d.last_pulse_at ? `脉冲 ${esc(d.last_pulse_at)}` : '等待首次脉冲');
  return `<div class="vm-card">
    <div class="vm-head">
      <span class="vm-state ${st[0]}"><span class="status-dot ${st[0]}"></span>${esc(statusText)}</span>
      <button class="vm-launch" data-act="launch-vm" data-id="${esc(accId)}" data-vm="${esc(d.desktop_id)}" title="远程连接这台云电脑">${icon('arrow-up-right')}</button>
    </div>
    <strong class="vm-name">${esc(d.desktop_name)}</strong>
    <div class="vm-specs">${specHtml}</div>
    ${meta.length ? `<div class="vm-meta">${meta.join('')}</div>` : ''}
    <div class="vm-foot"><span>${icon('activity')}<em>${pulseTxt}</em></span><span>累计 ${lm.success_count || 0} 次</span></div>
    ${err ? `<div class="vm-error" title="${esc(err)}">${icon('bell')}<span>${esc(err)}</span></div>` : ''}
  </div>`;
}
function accountCard(a, full = true) {
  const badges = vmStatus({ live_metrics: { status: a.stats?.keep_alive_status } });
  const vms = (a.desktops || []).slice(0, full ? 6 : 3).map(d => desktopCard(d, a.id, a.stats?.last_error || '')).join('')
    || '<div class="vm-empty">暂无云电脑实例</div>';
  const featureList = [a.features?.keep_alive && '保活', a.features?.auto_sign && '签到',
    a.features?.ai_chat && 'AI', a.features?.cloud_hang && '挂机', a.features?.auto_redeem && '兑换'].filter(Boolean);
  const featureTags = featureList.length
    ? featureList.map(f => `<span class="ftag">${f}</span>`).join('')
    : '<span class="ftag none">未启用</span>';
  return `<div class="account-card" data-account="${esc(a.id)}">
    <div class="card-top"><div class="card-icon">${icon('monitor')}</div>
      <div class="card-title"><strong>${esc(a.name || a.user)}</strong><small>${esc(a.user)} · ${(a.desktops || []).length} 台云电脑</small></div>
      <span class="card-badge ${a.session_expired ? 'error' : a.enabled ? badges[0] : 'offline'}">${a.session_expired ? '凭据失效' : a.enabled ? badges[1] : '已停用'}</span></div>
    <div class="card-info">
      <div class="info-cell"><small>保活周期</small><strong>${a.keepalive_interval || 30} 秒</strong></div>
      <div class="info-cell"><small>账户积分</small><strong>${a.stats?.points || 0}</strong></div>
      <div class="info-cell"><small>今日轮次</small><strong>${a.stats?.today_success_count || 0}</strong></div>
      <div class="info-cell info-features"><small>已启用任务</small><div class="ftags">${featureTags}</div></div></div>
    <div class="vm-grid">${vms}</div>
    ${full ? `<div class="card-actions">
      <button class="btn btn-ghost btn-xs" data-act="refresh" data-id="${esc(a.id)}">${icon('refresh')}刷新</button>
      <button class="btn btn-ghost btn-xs" data-act="power" data-id="${esc(a.id)}">${icon('power')}电源</button>
      <button class="btn btn-ghost btn-xs" data-act="settings" data-id="${esc(a.id)}">${icon('settings')}设置</button>
      <button class="btn btn-ghost btn-xs" data-act="desktop" data-id="${esc(a.id)}">${icon('monitor')}远程</button>
      <button class="btn btn-ghost btn-xs" data-act="edit" data-id="${esc(a.id)}">${icon('edit')}编辑</button>
      <button class="btn btn-ghost btn-xs" data-act="delete" data-id="${esc(a.id)}">${icon('trash')}</button></div>` : ''}
  </div>`;
}
function renderAccounts(accounts) {
  const list = Array.isArray(accounts) ? accounts : S.accounts;
  S.accounts = list;
  setText('account-count', `共 ${list.length} 个账号`);
  const s = sig(list.map(a => [a.id, a.name, a.user, a.enabled, a.session_expired, a.keepalive_interval,
    a.stats?.points, a.stats?.today_success_count, a.stats?.keep_alive_status, a.stats?.last_error, a.features,
    (a.desktops || []).map(d => [d.desktop_id, d.desktop_name, d.live_metrics?.status, d.spec_str,
      d.cpu, d.memory, d.os, d.image_name, d.flavor_name, d.use_status_text,
      d.live_metrics?.countdown, d.live_metrics?.success_count, d.last_pulse_at,
      d.live_metrics?.last_heartbeat_result])]));
  if (s === S.accountSig) return;             // 数据未变：跳过重建，无闪烁
  S.accountSig = s;
  const grid = $('#account-grid');
  if (!list.length) {
    grid.innerHTML = `<div class="empty-hint">还没有天翼云账号<br><br><button class="btn btn-primary btn-sm" id="empty-add">${icon('plus')}添加第一个账号</button></div>`;
    $('#empty-add')?.addEventListener('click', () => openAccountModal());
    return;
  }
  grid.innerHTML = list.map(a => accountCard(a, true)).join('');
  bindCardActions(grid);
  renderTaskAccountSelect();
}
function renderOverview(accounts) {
  const list = Array.isArray(accounts) ? accounts : S.accounts;
  const s = sig((list || []).slice(0, 4).map(a => [a.id, a.name, a.user, a.enabled, a.session_expired,
    a.stats?.points, a.stats?.today_success_count, a.stats?.keep_alive_status,
    (a.desktops || []).map(d => [d.desktop_id, d.desktop_name, d.live_metrics?.status, d.cpu, d.memory, d.os])]));
  if (s === S.overviewSig) return;
  S.overviewSig = s;
  const grid = $('#overview-grid');
  if (!list || !list.length) { grid.innerHTML = '<div class="empty-hint">暂无云电脑账号，点击「添加账号」开始</div>'; return; }
  grid.innerHTML = list.slice(0, 4).map(a => accountCard(a, false)).join('');
  bindCardActions(grid);   // 概览卡片重建后必须重新绑定远程/刷新等按钮事件
}
function bindCardActions(root) {
  root.querySelectorAll('[data-act]').forEach(btn => {
    btn.addEventListener('click', async () => {
      const id = btn.dataset.id, act = btn.dataset.act;
      const acc = S.accounts.find(a => a.id === id);
      if (act === 'delete') {
        if (!await uiConfirm('删除账号', `确定删除账号「${acc?.name || acc?.user}」吗？此操作不可撤销。`, '删除')) return;
        try { await api(`/api/accounts/${id}`, { method: 'DELETE' }); toast('账号已删除', 'success'); await loadAccounts(); }
        catch (e) { toast(e.message, 'error'); }
      } else if (act === 'refresh') {
        btn.classList.add('busy');
        try { await api(`/api/accounts/${id}/desktops`); toast('云电脑列表已刷新', 'success'); await loadAccounts(); }
        catch (e) { toast(e.message, 'error'); } finally { btn.classList.remove('busy'); }
      } else if (act === 'edit') openAccountModal(acc);
      else if (act === 'power') openPowerModal(acc);
      else if (act === 'settings') openVmSettingsModal(acc);
      else if (act === 'desktop') await launchDesktop(acc);
      else if (act === 'launch-vm') await launchDesktop(acc, btn.dataset.vm);
    });
  });
}
async function loadAccounts() {
  S.accounts = await api('/api/accounts');
  renderAccounts(S.accounts);
  return S.accounts;
}

/* ============ 通用：账号/桌面下拉填充 ============ */
function fillAccountSelect(sel, accounts, keepSelected = true) {
  if (!sel) return;
  const prev = keepSelected ? sel.value : '';
  sel.innerHTML = (accounts || []).map(a => `<option value="${esc(a.id)}">${accLabel(a)}</option>`).join('');
  if (prev && accounts.some(a => a.id === prev)) sel.value = prev;
}
function fillDesktopSelect(sel, account) {
  if (!sel) return;
  const vms = account?.desktops || [];
  sel.innerHTML = vms.length
    ? vms.map(d => `<option value="${esc(d.desktop_id)}">${esc(d.desktop_name)} · ${esc(d.spec_str || '')}</option>`).join('')
    : '<option value="">（无云电脑实例）</option>';
}
function findAccount(id) { return (S.accounts || []).find(a => a.id === id); }

/* ============ 远程桌面 ============ */
async function launchDesktop(acc, desktopId) {
  if (!acc) { toast('请先选择账号', 'warning'); return; }
  const vm = (desktopId && acc.desktops?.find(d => d.desktop_id === desktopId)) || acc.desktops?.[0];
  if (!vm) { toast('该账号暂无云电脑实例', 'warning'); return; }
  // 天翼云同一账号的远程桌面是单会话：另一个标签页还开着时再开，会把那个挤掉线。
  // 用同源 localStorage 活跃标记做互斥提示（心跳每 5s 续期，30s 无续期视为已关闭）。
  const rk = 'ctyun_remote_' + acc.id;
  let alive = false;
  try {
    const mark = JSON.parse(localStorage.getItem(rk) || 'null');
    alive = !!mark && (Date.now() - mark.ts < 30000);
  } catch (_) {}
  if (alive && !await uiConfirm('远程桌面已在运行', '该账号的远程桌面已在另一个标签页运行，再开一个会把那个挤掉线。确定要继续吗？', '继续打开')) return;
  try {
    const r = await api(`/api/accounts/${acc.id}/web-launch?desktopId=${encodeURIComponent(vm.desktop_id)}`);
    let openUrl = null;
    if (r.directViewUrl) {
      // 新标签页无法携带 Authorization header，用 URL token 通过后端鉴权（后端已支持 ?token=）。
      const u = new URL(r.directViewUrl, location.origin);
      u.searchParams.set('token', S.token || '');
      openUrl = u.toString();
    } else if (r.targetUrl) openUrl = r.targetUrl;
    if (openUrl) {
      try { localStorage.setItem(rk, JSON.stringify({ ts: Date.now() })); } catch (_) {}
      window.open(openUrl, '_blank', 'noopener');
    } else toast('已生成远程桌面会话', 'success');
  } catch (e) { toast(e.message, 'error'); }
}

/* ============ 电源管理弹窗 ============ */
function openPowerModal(acc) {
  $('#power-account').value = acc?.id || (S.accounts[0]?.id || '');
  const a = findAccount($('#power-account').value);
  fillDesktopSelect($('#power-desktop'), a);
  $('#power-modal').classList.remove('hidden');
}
$('#power-account').addEventListener('change', () => fillDesktopSelect($('#power-desktop'), findAccount($('#power-account').value)));
$$('.power-btn').forEach(btn => btn.addEventListener('click', async () => {
  const accId = $('#power-account').value, desktopId = $('#power-desktop').value;
  if (!accId) { toast('请先选择账号', 'warning'); return; }
  if (!desktopId) { toast('该账号暂无云电脑实例', 'warning'); return; }
  const action = btn.dataset.power;
  if (action === 'shutdown' && !await uiConfirm('关闭云电脑', '确定要关闭这台云电脑吗？未保存的工作可能丢失。', '确认关闭')) return;
  btn.classList.add('busy');
  try {
    const r = await api(`/api/accounts/${accId}/power/${action}`, { method: 'POST', body: { desktopId } });
    toast(r.message || '电源指令已下发', r.success === false ? 'error' : 'success');
  } catch (e) { toast(e.message, 'error'); } finally { btn.classList.remove('busy'); }
}));

/* ============ 云电脑逐台设置弹窗 ============ */
const FEATURE_LABELS = [
  ['keep_alive', '常态保活'], ['auto_sign', '自动签到'], ['ai_chat', 'AI 对话'],
  ['cloud_hang', '挂机任务'], ['auto_redeem', '自动兑换'], ['auto_boot', '自动开机'],
];
function openVmSettingsModal(acc) {
  fillAccountSelect($('#vm-account'), S.accounts || [], false);  // 先填充账号下拉
  $('#vm-account').value = acc?.id || (S.accounts[0]?.id || '');
  renderVmSettings(findAccount($('#vm-account').value));
  $('#vm-settings-modal').classList.remove('hidden');
}
function renderVmSettings(acc) {
  const box = $('#vm-settings-list');
  const vms = acc?.desktops || [];
  box.innerHTML = vms.length ? vms.map(d => `<div class="vm-item" data-vm="${esc(d.desktop_id)}">
    <div class="vm-item-head"><strong>${icon('monitor')} ${esc(d.desktop_name)}</strong><small>${esc(d.spec_str || '')}</small></div>
    <div class="vm-toggles">
      <label class="vm-toggle"><input type="checkbox" data-f="keepaliveEnabled" ${d.keepalive_enabled !== false ? 'checked' : ''}><span class="toggle-ui"></span><span>独立保活</span></label>
      <label class="vm-toggle"><input type="checkbox" data-f="taskEnabled" ${d.task_enabled !== false ? 'checked' : ''}><span class="toggle-ui"></span><span>积分任务</span></label>
      <label class="vm-toggle"><input type="checkbox" data-f="autoBootEnabled" ${d.auto_boot_enabled !== false ? 'checked' : ''}><span class="toggle-ui"></span><span>自动开机</span></label>
    </div>
    <div class="vm-interval">脉冲间隔 <input type="number" data-f="keepaliveInterval" value="${d.keepalive_interval || 30}" min="10" max="3300"> 秒</div>
  </div>`).join('') : '<div class="empty-hint">该账号暂无云电脑实例</div>';
  $('#feature-grid').innerHTML = FEATURE_LABELS.map(([k, label]) =>
    `<label class="feature-toggle"><input type="checkbox" data-feature="${k}" ${acc?.features?.[k] ? 'checked' : ''}><span class="check-ui"></span><span>${label}</span></label>`).join('');
  const dc = acc?.display_config || {};
  $('#disp-width').value = dc.width || 2560;
  $('#disp-height').value = dc.height || 1440;
  $('#disp-scale').value = dc.scale || 150;
}
$('#vm-account').addEventListener('change', () => renderVmSettings(findAccount($('#vm-account').value)));
$('#vm-save').addEventListener('click', async () => {
  const accId = $('#vm-account').value;
  if (!accId) { toast('请先选择账号', 'warning'); return; }
  const acc = findAccount(accId);
  const btn = $('#vm-save'); btn.classList.add('loading');
  try {
    // 逐台开关
    for (const item of $$('#vm-settings-list .vm-item')) {
      const vmId = item.dataset.vm;
      for (const input of item.querySelectorAll('[data-f]')) {
        const feature = input.dataset.f;
        const value = feature === 'keepaliveInterval' ? Number(input.value) : input.checked;
        await api(`/api/accounts/${accId}/vm-feature`, { method: 'PUT', body: { desktopId: vmId, feature, value } });
      }
    }
    // 账号功能开关 + 分辨率
    const features = {};
    $$('#feature-grid [data-feature]').forEach(i => features[i.dataset.feature] = i.checked);
    const payload = {
      features,
      displayConfig: { width: Number($('#disp-width').value), height: Number($('#disp-height').value), scale: Number($('#disp-scale').value) },
    };
    await api(`/api/accounts/${accId}`, { method: 'PUT', body: payload });
    toast('云电脑设置已保存', 'success');
    $('#vm-settings-modal').classList.add('hidden');
    S.accountSig = ''; await loadAccounts();
  } catch (e) { toast(e.message, 'error'); } finally { btn.classList.remove('loading'); }
});

/* ============ Account modal ============ */
function openAccountModal(acc = null) {
  $('#account-modal').classList.remove('hidden');
  setText('account-modal-title', acc ? '编辑天翼云账号' : '添加天翼云账号');
  $('#acc-name').value = acc?.name || '';
  $('#acc-user').value = acc?.user || '';
  $('#acc-password').value = acc?.password || '';
  $('#acc-device').value = acc?.device_code || '';
  $('#acc-captcha').value = '';
  $('#save-account-btn').dataset.editId = acc?.id || '';
}
function closeModal(id) {
  if (id === 'confirm-modal') { closeConfirm(null); return; }
  const el = document.getElementById(id);
  if (el) el.classList.add('hidden');
  if (id === 'account-modal' && S.qrTimer) { clearInterval(S.qrTimer); S.qrTimer = null; }
}
$$('[data-close]').forEach(b => b.addEventListener('click', () => closeModal(b.dataset.close)));
$$('.modal-backdrop').forEach(b => b.addEventListener('click', e => { if (e.target === b) closeModal(b.id); }));
$$('.tab').forEach(tab => tab.addEventListener('click', () => {
  $$('.tab').forEach(t => t.classList.toggle('active', t === tab));
  $$('.tab-pane').forEach(p => p.classList.toggle('active', p.id === `tab-${tab.dataset.tab}`));
}));
$('#add-account-btn').addEventListener('click', () => openAccountModal());
$('#qr-btn').addEventListener('click', () => {
  openAccountModal();
  $$('.tab').forEach(t => t.classList.toggle('active', t.dataset.tab === 'qr'));
  $$('.tab-pane').forEach(p => p.classList.toggle('active', p.id === 'tab-qr'));
});
$('#gen-device-btn').addEventListener('click', async () => {
  try { $('#acc-device').value = (await api('/api/device/generate', { method: 'POST' })).deviceCode; }
  catch (e) { toast(e.message, 'error'); }
});
$('#get-captcha-btn').addEventListener('click', async () => {
  const phone = $('#acc-user').value.trim();
  if (!phone) { toast('请先填写手机号', 'warning'); return; }
  try {
    const deviceCode = $('#acc-device').value.trim() || (await api('/api/device/generate', { method: 'POST' })).deviceCode;
    $('#acc-device').value = deviceCode;
    const r = await api(`/api/captcha/${encodeURIComponent(phone)}?deviceCode=${encodeURIComponent(deviceCode)}`);
    $('#captcha-img').src = r.captchaImage; $('#captcha-img').hidden = false;
    $('#acc-challenge-id').value = r.challengeId; $('#acc-challenge-code').value = r.challengeCode;
    toast('验证码已获取', 'success');
  } catch (e) { toast(e.message, 'error'); }
});
$('#captcha-img').addEventListener('click', () => $('#get-captcha-btn').click());
$('#gen-qr-btn').addEventListener('click', async () => {
  try {
    const r = await api('/api/account/qrcode/generate', { method: 'POST', body: { deviceCode: $('#acc-device').value } });
    S.qrData = r;
    $('#qr-image').innerHTML = r.qrImage ? `<img src="${r.qrImage}" alt="登录二维码">`
      : `<a href="${esc(r.qrUrl)}" target="_blank" rel="noopener">打开授权页面</a>`;
    setText('qr-hint', '请使用天翼云电脑 App 扫码授权，授权后点击下方保存');
    clearInterval(S.qrTimer);
    S.qrTimer = setInterval(async () => {
      try {
        const st = await api(`/api/account/qrcode/status?qrCodeId=${encodeURIComponent(r.qrCodeId)}&deviceCode=${encodeURIComponent(r.deviceCode)}&accountName=${encodeURIComponent($('#acc-name').value || '')}`);
        if (st.codeStatus === 'authorize' && st.account) {
          clearInterval(S.qrTimer); toast('扫码授权成功，账号已添加', 'success');
          $('#account-modal').classList.add('hidden'); loadAccounts();
        }
      } catch (_) { /* keep polling */ }
    }, 2500);
  } catch (e) { toast(e.message, 'error'); }
});
$('#save-account-btn').addEventListener('click', async () => {
  const editId = $('#save-account-btn').dataset.editId;
  const payload = {
    name: $('#acc-name').value.trim(), user: $('#acc-user').value.trim(), password: $('#acc-password').value,
    deviceCode: $('#acc-device').value.trim(), captchaCode: $('#acc-captcha').value.trim(),
    challengeId: $('#acc-challenge-id').value, challengeCode: $('#acc-challenge-code').value,
  };
  if (!payload.user) { toast('请填写手机号', 'warning'); return; }
  const btn = $('#save-account-btn'); btn.classList.add('loading');
  try {
    if (editId) await api(`/api/accounts/${editId}`, { method: 'PUT', body: payload });
    else await api('/api/accounts', { method: 'POST', body: payload });
    toast(editId ? '账号已更新' : '账号已添加', 'success');
    $('#account-modal').classList.add('hidden'); loadAccounts();
  } catch (e) { toast(e.message, 'error'); } finally { btn.classList.remove('loading'); }
});

/* ============ 积分商城 ============ */
const PRODUCT_TYPE_LABELS = {
  cpcai: 'AI 权益', pointscomputer: '云电脑试用', pointstplupgrade: '规格升配', pointsdiskupgrade: '数据盘',
};
let S_rewards = [], S_rewardPoints = 0;
async function loadRewards() {
  try {
    S_rewards = await api('/api/rewards');
    if (!Array.isArray(S_rewards)) { S_rewards = []; }
    // 余额：优先用当前账号 stats.points，否则由 rewards 接口已刷新进 config
    const acc = findAccount(S.selectedAccount) || S.accounts[0];
    S_rewardPoints = acc?.stats?.points || 0;
    setText('wallet-points', S_rewardPoints.toLocaleString('zh-CN'));
    setText('wallet-account', acc ? (acc.name || acc.user) : '暂无账号');
    renderRewards();
  } catch (e) {
    $('#rewards-grid').innerHTML = `<div class="empty-hint">积分商城加载失败：${esc(e.message)}</div>`;
  }
}
function rewardNeedsDesktop(p) {
  return p.prodType === 'pointstplupgrade' || p.prodType === 'pointsdiskupgrade';
}
function renderRewards() {
  const grid = $('#rewards-grid');
  if (!S_rewards.length) { grid.innerHTML = '<div class="empty-hint">暂无可兑换商品（请先添加并登录天翼云账号）</div>'; return; }
  grid.innerHTML = S_rewards.map((p, i) => `<div class="reward-card">
    <div class="reward-head"><span class="reward-name">${esc(p.prodName || p.name)}</span><span class="reward-type">${esc(PRODUCT_TYPE_LABELS[p.prodType] || p.prodType || '商品')}</span></div>
    <p class="reward-desc">${esc(p.description || '')}</p>
    <div class="reward-foot"><span class="reward-cost"><b>${Number(p.costPoints || 0).toLocaleString('zh-CN')}</b><small>积分</small></span>
      <button class="reward-buy" data-buy="${i}">兑换</button></div>
  </div>`).join('');
  grid.querySelectorAll('[data-buy]').forEach(b => b.addEventListener('click', () => openManualRedeem(S_rewards[Number(b.dataset.buy)])));
}
$('#wallet-refresh').addEventListener('click', async () => { await loadRewards(); toast('积分已刷新', 'success'); });

/* 手动兑换 */
function openManualRedeem(product = null) {
  const accounts = S.accounts || [];
  if (!accounts.length) { toast('请先添加天翼云账号', 'warning'); return; }
  fillAccountSelect($('#manual-account'), accounts);
  // 若从卡片点击，尽量选中当前账号
  if (S.selectedAccount && accounts.some(a => a.id === S.selectedAccount)) $('#manual-account').value = S.selectedAccount;
  const sel = $('#manual-product');
  sel.innerHTML = S_rewards.map((p, i) => `<option value="${i}">${esc(p.prodName || p.name)}（${p.costPoints} 积分）</option>`).join('');
  if (product) {
    const idx = S_rewards.findIndex(p => (p.prodId || p.id) === (product.prodId || product.id));
    if (idx >= 0) sel.value = String(idx);
  }
  syncManualForm();
  $('#manual-redeem-modal').classList.remove('hidden');
}
function syncManualForm() {
  const idx = Number($('#manual-product').value || 0);
  const p = S_rewards[idx];
  const acc = findAccount($('#manual-account').value);
  setText('manual-user-points', (acc?.stats?.points || 0).toLocaleString('zh-CN'));
  setText('manual-account-name', acc ? (acc.name || acc.user) : '');
  setText('manual-product-desc', p?.description || '');
  const needDesk = p ? rewardNeedsDesktop(p) : false;
  $('#manual-desktop-field').classList.toggle('hidden', !needDesk);
  fillDesktopSelect($('#manual-desktop'), acc);
  updateManualCost();
}
function updateManualCost() {
  const p = S_rewards[Number($('#manual-product').value || 0)];
  const times = Math.max(1, Number($('#manual-times').value || 1));
  setText('manual-cost', ((p?.costPoints || 0) * times).toLocaleString('zh-CN'));
}
$('#manual-account').addEventListener('change', syncManualForm);
$('#manual-product').addEventListener('change', syncManualForm);
$('#manual-times').addEventListener('input', updateManualCost);
$('#manual-redeem-btn').addEventListener('click', () => openManualRedeem());
$('#manual-order-submit').addEventListener('click', async () => {
  const accId = $('#manual-account').value;
  const p = S_rewards[Number($('#manual-product').value || 0)];
  if (!accId || !p) { toast('请选择账号与商品', 'warning'); return; }
  const prodId = p.prodId || p.id;
  const desktopId = $('#manual-desktop').value;
  if (rewardNeedsDesktop(p) && !desktopId) { toast('该商品必须选择绑定目标云电脑', 'warning'); return; }
  const times = Math.max(1, Number($('#manual-times').value || 1));
  const btn = $('#manual-order-submit'); btn.classList.add('loading');
  try {
    const r = await api(`/api/accounts/${accId}/order`, { method: 'POST', body: {
      prodId, prodName: p.prodName || p.name, prodType: p.prodType,
      costPoints: p.costPoints, times, desktopId, pointType: 1,
    }});
    toast(r.message || '兑换成功', r.success === false ? 'error' : 'success');
    if (r.success !== false) {
      $('#manual-redeem-modal').classList.add('hidden');
      await loadRewards(); refreshAll(true);
    }
  } catch (e) { toast(e.message, 'error'); } finally { btn.classList.remove('loading'); }
});

/* 自动兑换配置 */
function openRedeemConfig() {
  const accounts = S.accounts || [];
  if (!accounts.length) { toast('请先添加天翼云账号', 'warning'); return; }
  fillAccountSelect($('#rc-account'), accounts);
  $('#rc-product').innerHTML = S_rewards.map((p, i) => `<option value="${i}">${esc(p.prodName || p.name)}（${p.costPoints} 积分）</option>`).join('');
  syncRedeemForm();
  $('#redeem-config-modal').classList.remove('hidden');
}
function syncRedeemForm() {
  const acc = findAccount($('#rc-account').value);
  const rc = acc?.redeem_config || {};
  $('#rc-enabled').checked = !!rc.enabled;
  fillDesktopSelect($('#rc-desktop'), acc);
  if (rc.desktop_id) $('#rc-desktop').value = rc.desktop_id;
  const idx = S_rewards.findIndex(p => (p.prodId || p.id) === rc.prod_id);
  if (idx >= 0) $('#rc-product').value = String(idx);
  $('#rc-schedule').value = rc.schedule_type || 'monthly_days';
  $('#rc-monthly').value = (rc.monthly_days || []).join(',');
  $('#rc-interval').value = rc.interval_days || 7;
  $('#rc-max-times').value = rc.max_redeem_times || 1;
  syncScheduleFields();
}
function syncScheduleFields() {
  const t = $('#rc-schedule').value;
  $('#rc-monthly-group').classList.toggle('hidden', t !== 'monthly_days');
  $('#rc-interval-group').classList.toggle('hidden', t !== 'interval_days');
}
$('#redeem-config-btn').addEventListener('click', openRedeemConfig);
$('#rc-account').addEventListener('change', syncRedeemForm);
$('#rc-schedule').addEventListener('change', syncScheduleFields);
$('#rc-save').addEventListener('click', async () => {
  const accId = $('#rc-account').value;
  const p = S_rewards[Number($('#rc-product').value || 0)];
  if (!accId) { toast('请先选择账号', 'warning'); return; }
  const scheduleType = $('#rc-schedule').value;
  const monthly = $('#rc-monthly').value.split(',').map(s => parseInt(s.trim())).filter(n => !isNaN(n));
  const rc = {
    enabled: $('#rc-enabled').checked,
    desktop_id: $('#rc-desktop').value,
    desktop_name: $('#rc-desktop').selectedOptions[0]?.text || '',
    prod_id: p ? (p.prodId || p.id) : 0,
    prod_name: p ? (p.prodName || p.name) : '',
    prod_type: p?.prodType || '',
    cost_points: p?.costPoints || 0,
    max_redeem_times: Number($('#rc-max-times').value || 0),
    schedule_type: scheduleType,
    monthly_days: scheduleType === 'monthly_days' ? (monthly.length ? monthly : [-1]) : [],
    interval_days: Number($('#rc-interval').value || 7),
  };
  const btn = $('#rc-save'); btn.classList.add('loading');
  try {
    await api(`/api/accounts/${accId}`, { method: 'PUT', body: { redeemConfig: rc } });
    toast('自动兑换设置已保存', 'success');
    $('#redeem-config-modal').classList.add('hidden');
    S.accountSig = ''; await loadAccounts();
  } catch (e) { toast(e.message, 'error'); } finally { btn.classList.remove('loading'); }
});

/* ============ 短信绑定 ============ */
function openSmsModal() {
  fillAccountSelect($('#sms-account'), S.accounts || []);
  $('#sms-captcha-img').hidden = true; $('#sms-captcha').value = ''; $('#sms-code').value = '';
  $('#sms-modal').classList.remove('hidden');
}
$('#sms-bind-btn').addEventListener('click', openSmsModal);
$('#sms-get-captcha').addEventListener('click', async () => {
  const id = $('#sms-account').value;
  if (!id) { toast('请先选择账号', 'warning'); return; }
  try {
    const r = await api(`/api/accounts/${id}/sms-captcha`);
    $('#sms-captcha-img').src = r.captchaImage; $('#sms-captcha-img').hidden = false;
    toast('图形验证码已获取', 'success');
  } catch (e) { toast(e.message, 'error'); }
});
$('#sms-send').addEventListener('click', async () => {
  const id = $('#sms-account').value, code = $('#sms-captcha').value.trim();
  if (!code) { toast('请输入图形验证码', 'warning'); return; }
  try {
    const r = await api(`/api/accounts/${id}/send-sms`, { method: 'POST', body: { captchaCode: code } });
    toast(r.message || '短信验证码已发送', r.success === false ? 'error' : 'success');
  } catch (e) { toast(e.message, 'error'); }
});
$('#sms-bind').addEventListener('click', async () => {
  const id = $('#sms-account').value, code = $('#sms-code').value.trim();
  if (!code) { toast('请输入短信验证码', 'warning'); return; }
  try {
    const r = await api(`/api/accounts/${id}/bind-sms`, { method: 'POST', body: { verificationCode: code } });
    toast(r.message || '设备绑定成功', r.success === false ? 'error' : 'success');
    if (r.success !== false) { $('#sms-modal').classList.add('hidden'); S.accountSig = ''; await loadAccounts(); }
  } catch (e) { toast(e.message, 'error'); }
});

/* ============ 通知设置 ============ */
const NOTIFY_CHANNELS = [['dingtalk', '钉钉'], ['wecom', '企业微信'], ['feishu', '飞书'], ['telegram', 'Telegram'], ['serverchan', 'Server酱'], ['bark', 'Bark'], ['custom', '自定义 Webhook']];
async function renderNotifySettings() {
  let n;
  try { n = await api('/api/user/notify'); } catch { n = {}; }
  $('#notify-grid').innerHTML = `
    <label class="switch-row"><span><strong>启用通知</strong><small>推送任务达成、积分变动、登录失效告警</small></span>
      <input type="checkbox" id="notify-enabled" ${n.enabled ? 'checked' : ''}><span class="switch-ui"></span></label>
    <div class="setting-item"><label>通知渠道</label><select id="notify-channel" class="select">${NOTIFY_CHANNELS.map(([v, l]) => `<option value="${v}" ${n.channel === v ? 'selected' : ''}>${l}</option>`).join('')}</select></div>
    <div class="setting-item"><label>Webhook 地址</label><input id="notify-url" value="${esc(n.webhook_url || '')}" placeholder="https://..."></div>
    <div class="settings-actions"><button class="btn btn-ghost btn-sm" id="notify-test">${icon('bell')}发送测试</button>
      <button class="btn btn-primary btn-sm" id="notify-save">${icon('check')}保存通知设置</button></div>`;
  $('#notify-save').addEventListener('click', async () => {
    const payload = { enabled: $('#notify-enabled').checked, channel: $('#notify-channel').value,
      webhook_url: $('#notify-url').value.trim(), secret: n.secret || '',
      custom_title_template: n.custom_title_template || '', custom_content_template: n.custom_content_template || '' };
    try { await api('/api/user/notify', { method: 'PUT', body: payload }); toast('通知设置已保存', 'success'); }
    catch (e) { toast(e.message, 'error'); }
  });
  $('#notify-test').addEventListener('click', async () => {
    try { const r = await api('/api/user/notify/test', { method: 'POST' }); toast(r.message || '测试消息已发送', r.success === false ? 'error' : 'success'); }
    catch (e) { toast(e.message, 'error'); }
  });
}

/* ============ 账户安全 ============ */
$('#change-username-btn').addEventListener('click', () => { $('#new-username').value = S.user?.username || ''; $('#username-modal').classList.remove('hidden'); });
$('#username-submit').addEventListener('click', async () => {
  const username = $('#new-username').value.trim();
  if (!username) { toast('请输入新用户名', 'warning'); return; }
  try {
    await api('/api/auth/change-username', { method: 'POST', body: { username } });
    S.user.username = username; setText('user-name', username);
    toast('用户名已修改', 'success'); $('#username-modal').classList.add('hidden');
  } catch (e) { toast(e.message, 'error'); }
});
$('#change-password-btn').addEventListener('click', () => {
  $('#old-password').value = ''; $('#new-password').value = ''; $('#confirm-password').value = '';
  $('#password-modal').classList.remove('hidden');
});
$('#password-submit').addEventListener('click', async () => {
  const oldPassword = $('#old-password').value, newPassword = $('#new-password').value, confirm = $('#confirm-password').value;
  if (newPassword !== confirm) { toast('两次输入的新密码不一致', 'warning'); return; }
  if (newPassword.length < 6) { toast('新密码至少 6 位', 'warning'); return; }
  try {
    await api('/api/auth/change-password', { method: 'POST', body: { oldPassword, newPassword } });
    toast('密码已修改，请用新密码登录', 'success'); $('#password-modal').classList.add('hidden');
  } catch (e) { toast(e.message, 'error'); }
});

/* ============ Tasks ============ */
function renderTaskAccountSelect() {
  const box = $('#task-account-select');
  if (!S.accounts.length) { box.innerHTML = '<span class="empty-hint">请先添加账号</span>'; return; }
  if (!S.selectedAccount || !S.accounts.find(a => a.id === S.selectedAccount)) S.selectedAccount = S.accounts[0].id;
  const s = sig([S.selectedAccount, S.accounts.map(a => [a.id, a.name, a.user])]);
  if (box.dataset.sig === s) return;
  box.dataset.sig = s;
  box.innerHTML = S.accounts.map(a => `<button class="chip ${a.id === S.selectedAccount ? 'active' : ''}" data-id="${esc(a.id)}">${accLabel(a)}</button>`).join('');
  box.querySelectorAll('.chip').forEach(c => c.addEventListener('click', () => {
    S.selectedAccount = c.dataset.id;
    box.querySelectorAll('.chip').forEach(x => x.classList.toggle('active', x === c));
    box.dataset.sig = '';
  }));
}
$$('.task-action').forEach(btn => btn.addEventListener('click', async () => {
  if (!S.selectedAccount) { toast('请先添加并选择账号', 'warning'); return; }
  btn.classList.add('busy');
  try {
    const r = await api(`/api/accounts/${S.selectedAccount}/run/${btn.dataset.task}`, { method: 'POST' });
    toast(r.message || '任务已下发', 'success');
    setTimeout(() => refreshAll(true), 800);
  } catch (e) { toast(e.message, 'error'); } finally { btn.classList.remove('busy'); }
}));
async function loadCron() {
  if (S.user?.role !== 'admin') return;
  const settings = await api('/api/settings');
  const cron = settings.cron || {};
  const time = cron.execute_time || '01:20';
  const subOn = !!cron.enable_sub_cron;
  const subCron = (k, d) => esc(cron[k] || d);
  $('#cron-body').innerHTML = `
    <div class="cron-plain">
      <div class="cron-plain-head">
        <span class="cron-plain-ico">${icon('clock')}</span>
        <div><strong>${subOn ? '各任务按独立 cron 执行' : `每天 ${esc(time)} 自动执行`}</strong>
          <p>${subOn ? '签到、AI、挂机、兑换分别按各自的 cron 表达式独立触发，每天每项最多执行一次。' : '到点后依次完成签到、AI 对话、挂机与积分兑换，全程无需手动操作。'}</p></div>
      </div>
      <div class="cron-flow"><span>每日签到</span><i></i><span>AI 对话</span><i></i><span>挂机</span><i></i><span>积分兑换</span></div>
      <div class="cron-mode-row">
        <div class="switch-row" style="padding:0;flex:1">
          <span><strong>启用独立子任务调度</strong><small>开启后各任务按下面的 cron 单独执行；关闭则统一在每天一个时间点全部执行</small></span>
          <label><input type="checkbox" id="cron-sub-on" ${subOn ? 'checked' : ''}><span class="switch-ui"></span></label>
        </div>
      </div>
      <div class="cron-edit" id="cron-unified" ${subOn ? 'style="display:none"' : ''}>
        <label>统一执行时间</label>
        <input type="time" id="cron-time" value="${esc(time)}">
        <button class="btn btn-ghost btn-sm" id="cron-trigger">${icon('refresh')}立即执行全部</button>
        <button class="btn btn-primary btn-sm" id="cron-save">${icon('check')}保存</button>
      </div>
      <div class="cron-sub-grid ${subOn ? '' : 'hidden'}" id="cron-sub-grid">
        <div class="cron-sub-head">cron 格式：<code>分 时 日 月 周</code> 或 <code>秒 分 时 日 月 周</code>，<code>*</code> 表示任意</div>
        <label class="field"><span class="field-label">签到 cron</span><input type="text" id="cron-sign" value="${subCron('sign_cron', '0 20 1 * * *')}" placeholder="0 20 1 * * *"></label>
        <label class="field"><span class="field-label">AI 对话 cron</span><input type="text" id="cron-ai" value="${subCron('ai_chat_cron', '0 25 1 * * *')}" placeholder="0 25 1 * * *"></label>
        <label class="field"><span class="field-label">挂机 cron</span><input type="text" id="cron-hang" value="${subCron('cloud_hang_cron', '0 30 1 * * *')}" placeholder="0 30 1 * * *"></label>
        <label class="field"><span class="field-label">积分兑换 cron</span><input type="text" id="cron-redeem" value="${subCron('redeem_cron', '0 35 1 * * *')}" placeholder="0 35 1 * * *"></label>
        <button class="btn btn-primary btn-sm" id="cron-save-sub">${icon('check')}保存调度</button>
      </div>
    </div>`;
  // 切换模式只切 UI，不立即保存
  $('#cron-sub-on')?.addEventListener('change', (e) => {
    const on = e.target.checked;
    if ($('#cron-unified')) $('#cron-unified').style.display = on ? 'none' : '';
    $('#cron-sub-grid')?.classList.toggle('hidden', !on);
  });
  // 统一时间保存
  $('#cron-save')?.addEventListener('click', async () => {
    const t = $('#cron-time')?.value;
    if (!/^\d{1,2}:\d{2}$/.test(t || '')) { toast('请选择有效的执行时间', 'warning'); return; }
    const hh = String(t.split(':')[0]).padStart(2, '0');
    const normalized = `${hh}:${t.split(':')[1]}`;
    try {
      await api('/api/settings', { method: 'PUT', body: { ...settings, cron: { ...cron, execute_time: normalized, enable_sub_cron: false } } });
      toast(`已设为每天 ${normalized} 自动执行`, 'success');
      loadCron();
    } catch (e) { toast(e.message, 'error'); }
  });
  // 独立 cron 保存
  $('#cron-save-sub')?.addEventListener('click', async () => {
    const val = (id) => ($(id)?.value || '').trim();
    const next = { ...cron, enable_sub_cron: true,
      sign_cron: val('#cron-sign') || '0 20 1 * * *',
      ai_chat_cron: val('#cron-ai') || '0 25 1 * * *',
      cloud_hang_cron: val('#cron-hang') || '0 30 1 * * *',
      redeem_cron: val('#cron-redeem') || '0 35 1 * * *' };
    try {
      await api('/api/settings', { method: 'PUT', body: { ...settings, cron: next } });
      toast('独立调度已保存', 'success');
      loadCron();
    } catch (e) { toast(e.message, 'error'); }
  });
  // 立即执行全部（真正的任务，不是假刷新）
  $('#cron-trigger')?.addEventListener('click', async () => {
    if (!await uiConfirm('立即执行全部任务？', '将对所有启用任务的账号下发签到、AI、挂机、兑换任务，进度请看实时日志。', '立即执行')) return;
    try {
      const r = await api('/api/scheduler/trigger', { method: 'POST', body: {} });
      toast(r.message || '已触发', 'success');
    } catch (e) { toast(e.message, 'error'); }
  });
}

/* ============ Logs ============ */
async function loadLogs() {
  S.logs = await api('/api/logs'); S.logSig = ''; renderLogs();
}
function renderLogs() {
  const level = $('#log-level').value;
  const filtered = S.logs.filter(l => !level || l.level === level);
  const s = sig(filtered.slice(0, 200).map(l => [l.id, l.timestamp, l.level, l.source, l.message, l.repeat_count]));
  if (s === S.logSig) return;
  S.logSig = s;
  const list = $('#log-list');
  if (!filtered.length) { list.innerHTML = '<div class="empty-hint">暂无日志记录</div>'; return; }
  const SRC_LABEL = { Task:'任务', Scheduler:'调度', KeepAlive:'保活', Bms:'BMS', Account:'账号', Auth:'登录', Upgrade:'升级', Notify:'通知', System:'系统', Api:'接口' };
  const LEVEL_LABEL = { success:'成功', info:'信息', warning:'警告', error:'错误' };
  list.innerHTML = filtered.map(l => {
    const src = SRC_LABEL[l.source] || l.source || '系统';
    const lvl = LEVEL_LABEL[l.level] || '';
    return `<div class="log-row ${esc(l.level)}">
    <span class="log-time">${esc(l.timestamp?.slice(11) || '')}</span>
    <span class="log-source" title="${esc(l.source || '')}">${esc(src)}${lvl ? ' · ' + lvl : ''}</span>
    <span class="log-msg">${esc(l.message)}${l.repeat_count > 1 ? ` <em style="color:var(--faint)">×${l.repeat_count}</em>` : ''}</span></div>`;
  }).join('');
}
$('#log-level').addEventListener('change', renderLogs);
$('#clear-logs-btn').addEventListener('click', async () => {
  try { await api('/api/logs/clear', { method: 'POST' }); S.logs = []; renderLogs(); toast('日志已清空', 'success'); }
  catch (e) { toast(e.message, 'error'); }
});
function connectSSE() {
  if (S.es) S.es.close();
  const es = new EventSource(`/api/logs/stream?token=${encodeURIComponent(S.token)}`);
  S.es = es;
  es.onmessage = (e) => {
    try {
      const entry = JSON.parse(e.data);
      S.logs.unshift(entry);
      if (S.logs.length > 400) S.logs.pop();
      if (S.currentView === 'logs') renderLogs();
    } catch (_) { /* ignore malformed */ }
  };
}

/* ============ Settings ============ */
async function loadSettings() {
  // 通知设置对所有用户可用
  await renderNotifySettings();
  if (S.user?.role !== 'admin') return;
  const settings = await api('/api/settings');
  $$('.admin-only').forEach(e => e.classList.remove('hidden'));
  // 登录验证：独立醒目卡片，切换即保存（无需再点底部保存按钮）
  const authOn = settings.auth_required !== false;
  const setAuth = $('#set-auth');
  const paintAuth = (on) => {
    if (setAuth) setAuth.checked = on;
    const tag = $('#auth-mode-tag'), desc = $('#auth-mode-desc');
    if (tag) { tag.textContent = on ? '已开启' : '已关闭'; tag.className = 'auth-mode-tag ' + (on ? 'on' : 'off'); }
    if (desc) desc.textContent = on ? '访问控制台需要账号密码验证' : '当前免登录（仅限可信内网，任何人都可管理员操作）';
  };
  paintAuth(authOn);
  if (setAuth && !setAuth._bound) {
    setAuth._bound = true;
    setAuth.addEventListener('change', async () => {
      const want = setAuth.checked;
      paintAuth(want);
      try {
        const cur = await api('/api/settings');
        await api('/api/settings', { method: 'PUT', body: { ...cur, auth_required: want } });
        toast(want ? '已开启登录验证' : '已关闭登录验证（仅限内网使用）', 'success');
      } catch (e) { paintAuth(!want); toast(e.message, 'error'); }
    });
  }
  $('#settings-grid').innerHTML = `
    <details class="set-group" open>
      <summary><span class="sg-ico">${icon('settings')}</span><span class="sg-text"><strong>基本设置</strong><small>系统名称与运行端口</small></span>${icon('chevron')}</summary>
      <div class="set-group-body">
        <div class="setting-item"><label>系统标题</label><input id="set-title" value="${esc(settings.system_title || '')}"></div>
        <div class="setting-item"><label>副标题</label><input id="set-subtitle" value="${esc(settings.system_subtitle || '')}"></div>
        <div class="setting-item"><label>服务端口</label><input id="set-port" type="number" value="${settings.web_port || 8571}"></div>
      </div>
    </details>
    <details class="set-group">
      <summary><span class="sg-ico">${icon('activity')}</span><span class="sg-text"><strong>运行与调度</strong><small>保活脉冲与每日执行时间</small></span>${icon('chevron')}</summary>
      <div class="set-group-body">
        <div class="setting-item"><label>保活周期（秒）</label><input id="set-keepalive" type="number" value="${settings.keep_alive_seconds || 60}"></div>
        <div class="setting-item"><label>脉冲间隔（秒）</label><input id="set-pulse" type="number" value="${settings.pulse_interval_seconds || 30}"></div>
        <div class="setting-item"><label>每日执行时间</label><input id="set-cron" type="time" value="${esc(settings.cron?.execute_time || '01:20')}"></div>
      </div>
    </details>
    <details class="set-group" open>
      <summary><span class="sg-ico">${icon('arrow-up-right')}</span><span class="sg-text"><strong>在线升级</strong><small>检查并更新到最新版本</small></span>${icon('chevron')}</summary>
      <div class="set-group-body">
        <div id="upgrade-box" class="upgrade-box">
          <div class="upgrade-info" id="upgrade-info"><span class="up-loading">正在检查版本…</span></div>
          <div class="upgrade-hint">升级仅更新程序本体，<strong>账号、配置与全部数据均会保留</strong>；升级完成后服务自动重启，约需数秒。</div>
          <div class="upgrade-actions">
            <button class="btn btn-ghost btn-sm" id="upgrade-check">${icon('refresh')}检查更新</button>
            <button class="btn btn-primary btn-sm hidden" id="upgrade-do">${icon('arrow-up-right')}立即升级</button>
          </div>
        </div>
      </div>
    </details>
    <details class="set-group">
      <summary><span class="sg-ico">${icon('shield')}</span><span class="sg-text"><strong>账号与数据</strong><small>配置备份与导出</small></span>${icon('chevron')}</summary>
      <div class="set-group-body">
        <p class="set-hint">导出配置包含加密密钥，请妥善保管，切勿外泄。</p>
      </div>
    </details>
    <div class="settings-actions"><button class="btn btn-ghost btn-sm" id="export-btn">${icon('database')}导出配置</button>
      <button class="btn btn-primary btn-sm" id="save-settings">${icon('check')}保存设置</button></div>`;
  initUpgrade();
  $('#save-settings').addEventListener('click', async () => {
    const rawTime = $('#set-cron')?.value || '01:20';
    const parts = rawTime.split(':');
    const execTime = `${String(parts[0] || '1').padStart(2, '0')}:${parts[1] || '20'}`;
    const payload = {
      web_port: Number($('#set-port').value), keep_alive_seconds: Number($('#set-keepalive').value),
      pulse_interval_seconds: Number($('#set-pulse').value), default_quota: settings.default_quota ?? 2,
      allow_registration: false, system_title: $('#set-title').value,
      system_subtitle: $('#set-subtitle').value,
      auth_required: $('#set-auth') ? $('#set-auth').checked : (settings.auth_required !== false),
      cron: { ...(settings.cron || {}), execute_time: execTime }, notify: settings.notify || {},
    };
    try {
      await api('/api/settings', { method: 'PUT', body: payload });
      settings = { ...settings, ...payload };
      toast('设置已保存' + (payload.auth_required ? '' : '（已关闭登录验证，仅限内网使用）'), 'success');
    }
    catch (e) { toast(e.message, 'error'); }
  });
  $('#export-btn').addEventListener('click', async () => {
    try {
      const r = await api('/api/config/export');
      const a = document.createElement('a');
      a.href = URL.createObjectURL(new Blob([r.config], { type: 'application/json' }));
      a.download = 'ctyun-config.json'; a.click(); URL.revokeObjectURL(a.href);
      toast('配置已导出（请妥善保管主密钥）', 'success');
    } catch (e) { toast(e.message, 'error'); }
  });
  const users = await api('/api/admin/users');
  $('#user-table').innerHTML = users.map(u => {
    const isAdmin = u.role === 'admin';
    const initial = (u.username || '?').trim().charAt(0).toUpperCase();
    const created = u.createdAt ? esc(String(u.createdAt).slice(0, 16)) : '—';
    return `<div class="user-row">
      <span class="avatar" title="${esc(u.username)}">${initial}</span>
      <span class="user-meta"><strong>${esc(u.username)}</strong><small>创建于 ${created}</small></span>
      <span class="user-right">
        <span class="role-tag ${isAdmin ? 'admin' : ''}">${isAdmin ? '管理员' : '普通用户'}</span>
        <span class="user-actions"><button class="mini-btn" data-pwd="${esc(u.id)}" data-uname="${esc(u.username)}">重置密码</button></span>
      </span></div>`;
  }).join('');
  $('#user-table').querySelectorAll('[data-pwd]').forEach(b => b.addEventListener('click', async () => {
    const p = await uiPrompt('重置密码', `为用户「${b.dataset.uname || ''}」设置新密码，至少 6 位。`, '新密码', '');
    if (!p) return;
    try { await api(`/api/admin/users/${b.dataset.pwd}/password`, { method: 'PUT', body: { password: p } }); toast('密码已重置', 'success'); }
    catch (e) { toast(e.message, 'error'); }
  }));
}

/* ============ 在线升级（设置页 + 总览卡片同步） ============ */
let _upgradeInfo = null;
function initUpgrade() {
  // 总览卡片事件在 initApp 中已对管理员绑定，这里只绑定设置页的按钮
  $('#upgrade-check')?.addEventListener('click', () => checkUpgrade(true));
  $('#upgrade-do')?.addEventListener('click', doUpgrade);
  checkUpgrade(false);
}
function setUpgradeBusy(busy) {
  ['#upgrade-do', '#dash-ver-upgrade'].forEach(sel => {
    const b = $(sel); if (!b) return;
    b.classList.toggle('busy', busy); b.disabled = busy;
  });
}
async function checkUpgrade(manual) {
  const info = $('#upgrade-info');
  const vStatus = $('#dash-ver-status');
  const card = $('#dash-version-card');
  const badge = $('#dash-ver-badge');
  if (!info && !vStatus) return;
  const setBadge = (text, cls) => {
    if (!badge) return;
    badge.textContent = text;
    badge.className = 'ub-badge' + (cls ? ' ' + cls : '');
  };
  if (manual) {
    if (info) info.innerHTML = '<span class="up-loading">正在检查版本…</span>';
    if (vStatus) { vStatus.textContent = '正在检查更新…'; vStatus.className = 'ub-status'; }
    setBadge('版本', '');
  }
  try {
    const r = await api('/api/upgrade/check');
    _upgradeInfo = r;
    const cur = r.current_version || '?';
    const lat = (r.latest_version || '').replace(/^v/, '') || '?';
    if ($('#dash-ver-current')) $('#dash-ver-current').textContent = 'v' + cur;
    if ($('#about-version')) $('#about-version').textContent = 'v' + cur;
    card?.classList.remove('hidden'); // 管理员可见：检测完即显示横幅
    const showDo = (on) => {
      ['#upgrade-do', '#dash-ver-upgrade'].forEach(sel => $(sel)?.classList.toggle('hidden', !on));
    };
    if (!r.reachable) {
      if (info) info.innerHTML = `<span class="up-ver">当前版本 v${esc(cur)}</span><span class="up-status up-warn">暂时无法连接更新服务器</span>`;
      if (vStatus) { vStatus.textContent = '暂时无法连接更新服务器'; vStatus.className = 'ub-status'; }
      setBadge('离线', 'off');
      showDo(false);
      if (manual) toast(r.error || '检查更新失败', 'warning');
      return;
    }
    if (r.update_available) {
      if (info) info.innerHTML = `<span class="up-ver">当前 v${esc(cur)} → 最新 v${esc(lat)}</span><span class="up-status up-new">发现新版本，可升级</span>`;
      if (vStatus) { vStatus.textContent = `发现新版本 v${lat}，点击右侧按钮升级`; vStatus.className = 'ub-status has-new'; }
      setBadge('可升级', 'has-new'); // 醒目徽标
      showDo(true);
      if (manual) toast('发现新版本', 'success');
    } else {
      const byMd5 = r.method === 'md5' && r.local_md5 && r.remote_md5;
      const tip = byMd5 ? '已是最新版本（校验值一致）' : '已是最新版本';
      if (info) info.innerHTML = `<span class="up-ver">当前版本 v${esc(cur)}${lat !== '?' ? ` · 最新 v${esc(lat)}` : ''}</span><span class="up-status up-ok">${tip}</span>`;
      if (vStatus) { vStatus.textContent = tip; vStatus.className = 'ub-status is-latest'; }
      setBadge('最新', 'is-latest');
      showDo(false);
      if (manual) toast(tip, 'success');
    }
  } catch (e) {
    if (info) info.innerHTML = '<span class="up-status up-warn">检查更新失败</span>';
    if (vStatus) { vStatus.textContent = '检查更新失败'; vStatus.className = 'ub-status'; }
    setBadge('异常', 'off');
    ['#upgrade-do', '#dash-ver-upgrade'].forEach(sel => $(sel)?.classList.add('hidden'));
    if (manual) toast(e.message, 'error');
  }
}
async function doUpgrade() {
  const info = $('#upgrade-info');
  const vStatus = $('#dash-ver-status');
  if (!await uiConfirm('确认在线升级', '升级仅更新程序本体，账号与数据全部保留。升级完成后服务会自动重启，页面短暂断开，请稍后刷新。', '立即升级')) return;
  setUpgradeBusy(true);
  if (info) info.innerHTML = '<span class="up-loading">正在下载并安装新版本，请勿关闭页面…</span>';
  if (vStatus) { vStatus.textContent = '正在下载并安装新版本…'; vStatus.className = 'ub-status'; }
  try {
    const r = await api('/api/upgrade/do', { method: 'POST', body: {} });
    toast(r.message || '升级已安装，正在重启', 'success');
    if (info) info.innerHTML = '<span class="up-status up-ok">升级完成，服务重启中…</span>';
    if (vStatus) { vStatus.textContent = '升级完成，服务重启中…'; vStatus.className = 'ub-status is-latest'; }
    ['#upgrade-do', '#dash-ver-upgrade'].forEach(sel => $(sel)?.classList.add('hidden'));
    // 服务会自行重启，等待恢复后重新登录并刷新
    waitAndReload();
  } catch (e) {
    setUpgradeBusy(false);
    if (info) info.innerHTML = '<span class="up-status up-warn">升级失败，请重试或使用一键脚本</span>';
    if (vStatus) { vStatus.textContent = '升级失败，请重试'; vStatus.className = 'ub-status'; }
    toast(e.message, 'error');
  }
}
function waitAndReload() {
  let tries = 0;
  const tick = async () => {
    tries++;
    try {
      const ok = await fetch('/health', { cache: 'no-store' });
      if (ok.ok) { location.reload(); return; }
    } catch (_) { /* 服务尚未恢复 */ }
    if (tries < 60) setTimeout(tick, 1500);
    else toast('服务重启较慢，请稍后手动刷新页面', 'warning');
  };
  setTimeout(tick, 2500);
}

/* ============ Start ============ */
boot();
