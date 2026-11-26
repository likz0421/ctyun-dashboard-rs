#!/usr/bin/env bash
# 一键冒烟测试：前后端核心功能 + 积分系统（完全隔离，临时数据目录 + 独立端口）
# 用法: ./smoke_test.sh [--with-browser]   --with-browser 需要 playwright + chromium
set -u

ROOT="$(cd "$(dirname "$0")" && pwd)"
# 同时支持开发布局(target/release/)与部署布局(根目录单文件)
BIN="$ROOT/target/release/ctyun-dashboard-rs"
[ ! -x "$BIN" ] && BIN="$ROOT/ctyun-dashboard-rs"
PORT=18571
BASE="http://127.0.0.1:$PORT"
TMPDIR="$ROOT/.smoke_test_data"
PASS=0; FAIL=0
WITH_BROWSER=0
[ "${1:-}" = "--with-browser" ] && WITH_BROWSER=1

GREEN='\033[0;32m'; RED='\033[0;31m'; YELLOW='\033[1;33m'; NC='\033[0m'
ok(){ echo -e "${GREEN}[PASS]${NC} $1"; PASS=$((PASS+1)); }
ng(){ echo -e "${RED}[FAIL]${NC} $1"; FAIL=$((FAIL+1)); }
info(){ echo -e "${YELLOW}[INFO]${NC} $1"; }

cleanup(){
  [ -n "${SRV_PID:-}" ] && kill "$SRV_PID" 2>/dev/null
  rm -rf "$TMPDIR"
  echo -e "\n${YELLOW}=== 清理完成（临时数据与进程已删除）===${NC}"
}
trap cleanup EXIT

echo -e "${YELLOW}=== 天翼云控制面板 · 冒烟测试 ===${NC}"

# 0. 构建
if [ ! -x "$BIN" ]; then
  info "二进制不存在，尝试 cargo build..."
  (cd "$ROOT" && cargo build --release) >/dev/null 2>&1 || { echo "构建失败：未找到二进制且无 Rust 环境"; exit 1; }
  BIN="$ROOT/target/release/ctyun-dashboard-rs"
fi
ok "release 二进制就绪: $(du -h "$BIN" | cut -f1)"

# 1. 准备隔离环境（预置端口 18571）
rm -rf "$TMPDIR"; mkdir -p "$TMPDIR"
cat > "$TMPDIR/app_config.json" <<EOF
{"version":"1.0.0","settings":{"web_port":$PORT,"keep_alive_seconds":60,"pulse_interval_seconds":30,"allow_registration":false,"default_quota":2,"cron":{"execute_time":"01:20","enable_sub_cron":false,"sign_cron":"0 20 1 * * *","ai_chat_cron":"0 25 1 * * *","cloud_hang_cron":"0 30 1 * * *","redeem_cron":"0 35 1 * * *"},"notify":{"enabled":false,"channel":"","webhook_url":""},"system_title":"云电脑控制中心","system_subtitle":"CtYun Dashboard · Rust Edition"},"users":[],"accounts":[]}
EOF
CTYUN_DATA_DIR="$TMPDIR" "$BIN" > "$TMPDIR/server.log" 2>&1 &
SRV_PID=$!
for i in $(seq 1 30); do
  curl -s "$BASE/health" >/dev/null 2>&1 && break
  sleep 0.3
done
curl -s "$BASE/health" | grep -q '"status":"ok"' && ok "服务启动 + /health 正常 (端口 $PORT)" || { ng "服务启动失败"; cat "$TMPDIR/server.log"; exit 1; }

# 2. 登录与鉴权
LOGIN=$(curl -s -X POST "$BASE/api/auth/login" -H 'Content-Type: application/json' -d '{"username":"admin","password":"admin123"}')
TOKEN=$(echo "$LOGIN" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("token",""))' 2>/dev/null)
[ -n "$TOKEN" ] && ok "管理员登录 (admin/admin123) 返回 token" || { ng "登录失败: $LOGIN"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"
CODE=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/accounts")
[ "$CODE" = "401" ] && ok "未授权访问 /api/accounts 返回 401（权限隔离生效）" || ng "权限隔离异常: 期望401 实际$CODE"

# 3. 前端静态资源
for p in / /static/app.js /static/style.css; do
  CODE=$(curl -s -o /dev/null -w '%{http_code}' "$BASE$p")
  [ "$CODE" = "200" ] && ok "前端资源 $p (200)" || ng "前端资源 $p 异常 ($CODE)"
done
curl -s "$BASE/" | grep -q 'points-modal' && ok "积分明细弹窗 DOM 存在" || ng "积分明细弹窗缺失"
curl -s "$BASE/" | grep -q 'theme-switch' && ok "深浅主题切换控件存在" || ng "主题切换控件缺失"
curl -s "$BASE/" | grep -q 'data-accent-set' && ok "强调色主题选项存在" || ng "强调色选项缺失"
curl -s "$BASE/static/app.js" | grep -q '}, 4000)' && ok "前端 4 秒增量轮询已启用" || ng "前端轮询间隔异常"
curl -s "$BASE/static/app.js" | grep -q '值未变：不触碰 DOM' && ok "KPI 增量更新（防闪烁）代码存在" || ng "增量更新代码缺失"
# 补齐功能入口检查
HTML=$(curl -s "$BASE/")
for entry in 'data-view="rewards":积分商城导航' 'id="manual-redeem-modal":手动兑换弹窗' 'id="redeem-config-modal":自动兑换设置' 'id="power-modal":电源管理弹窗' 'id="vm-settings-modal":云电脑设置弹窗' 'id="sms-modal":短信绑定弹窗' 'id="notify-grid":通知设置面板'; do
  pat="${entry%%:*}"; label="${entry##*:}"
  echo "$HTML" | grep -q "$pat" && ok "$label 入口存在" || ng "$label 入口缺失"
done

# 4. 核心只读 API
for ep in status accounts settings logs rewards; do
  R=$(curl -s "$BASE/api/$ep" -H "$AUTH")
  echo "$R" | python3 -m json.tool >/dev/null 2>&1 && ok "GET /api/$ep 返回合法 JSON" || ng "GET /api/$ep 返回异常: ${R:0:80}"
done

# 5. 积分系统专项：注入测试数据后验证语义（今日已获 vs 余额分离）
python3 - <<PY
import json
p="$TMPDIR/app_config.json"
c=json.load(open(p))
a={"id":"ct_smoke001","platform":"ctyun","owner_id":c["users"][0]["id"] if c.get("users") else "u_smoke","name":"冒烟测试账号","user":"13800138000",
   "enabled":True,"stats":{"last_sign_time":"2026-11-24 10:30:00","last_ai_chat_time":"2026-11-24 10:31:00","last_hang_time":"","points":900},
   "desktops":[{"desktop_id":"123456","desktop_name":"测试云电脑","keepalive_enabled":False,"task_enabled":True,
     "live_metrics":{"status":"online","official_tasks":[
       {"name":"登录AI云电脑","current":1,"total":1,"status":2,"points":100},
       {"name":"AI对话","current":1,"total":1,"status":2,"points":100},
       {"name":"使用1小时","current":25,"total":60,"status":1,"points":100}]}}]}
c["accounts"]=[a]
json.dump(c,open(p,'w'),ensure_ascii=False)
PY
kill "$SRV_PID" 2>/dev/null; sleep 0.5
CTYUN_DATA_DIR="$TMPDIR" "$BIN" > "$TMPDIR/server.log" 2>&1 &
SRV_PID=$!
for i in $(seq 1 30); do curl -s "$BASE/health" >/dev/null 2>&1 && break; sleep 0.3; done
TOKEN=$(curl -s -X POST "$BASE/api/auth/login" -H 'Content-Type: application/json' -d '{"username":"admin","password":"admin123"}' | python3 -c 'import sys,json;print(json.load(sys.stdin).get("token",""))' 2>/dev/null)
AUTH="Authorization: Bearer $TOKEN"
STATUS=$(curl -s "$BASE/api/status" -H "$AUTH")
echo "$STATUS" > "$TMPDIR/status.json"
python3 - "$TMPDIR/status.json" <<'PYEOF'
import sys, json
d = json.load(open(sys.argv[1]))
det = d["pointsDetails"][0]
t = {x["name"]: x for x in det["tasks"]}
checks = [
    (det["todayPoints"] == 200, "今日已获=200"),
    (det["totalPoints"] == 900, "余额=900"),
    (d["totalEarnedPoints"] == 200, "汇总=200"),
    (t["使用1小时"]["progress"] == "25/60" and not t["使用1小时"]["completed"], "未完成任务显示进度"),
    (t["登录AI云电脑"]["completed"] and t["登录AI云电脑"]["points"] == 100, "完成任务计100分"),
]
failed = [label for ok, label in checks if not ok]
sys.exit(1 if failed else 0)
PYEOF
[ $? = 0 ] && ok "积分语义: 今日已获=200 / 余额=900 / 未完成任务显示进度" || ng "积分语义异常: $STATUS"

# 6. 下单校验（无真实账号也能测降级拦截）
ACC=$(curl -s "$BASE/api/accounts" -H "$AUTH" | python3 -c 'import sys,json;print(json.load(sys.stdin)[0]["id"])' 2>/dev/null)
R=$(curl -s -X POST "$BASE/api/accounts/$ACC/order" -H "$AUTH" -H 'Content-Type: application/json' -d '{"prodId":17023111,"prodName":"16C32G升配包","prodType":"pointstplupgrade","costPoints":1000,"times":1,"desktopId":"123"}')
echo "$R" | grep -q '积分不足' && ok "下单积分不足校验生效（余额900 < 需1000）" || ng "积分不足校验异常: $R"
R=$(curl -s -X POST "$BASE/api/accounts/$ACC/order" -H "$AUTH" -H 'Content-Type: application/json' -d '{"prodId":17023101,"prodName":"8C16G升配包","prodType":"pointstplupgrade","costPoints":500,"times":1,"desktopId":""}')
echo "$R" | grep -q '必须选择绑定' && ok "升配商品绑定桌面校验生效" || ng "桌面绑定校验异常: $R"

# 7. 二维码本地生成（无外部依赖）
R=$(curl -s -X POST "$BASE/api/account/qrcode/generate" -H "$AUTH" -H 'Content-Type: application/json' -d '{"deviceCode":"smoketest"}')
echo "$R" | grep -q 'svg+xml' && ok "二维码本地 SVG 生成（无外部服务依赖）" || ng "二维码生成异常: ${R:0:80}"

# 8. 可选：浏览器渲染验证
if [ "$WITH_BROWSER" = "1" ]; then
  info "浏览器渲染验证（需要 playwright + chromium）..."
  NODE_PATH=$(npm root -g 2>/dev/null) node - "$BASE" "$TMPDIR" <<'JS' 2>/dev/null || ng "浏览器验证失败（检查 playwright/chromium 或省略 --with-browser）"
const { chromium } = require('playwright');
(async () => {
  const [base, tmp] = process.argv.slice(2);
  const fs = require('fs');
  let exe = '/root/.cache/ms-playwright/chromium-1208/chrome-linux64/chrome';
  if (!fs.existsSync(exe)) exe = undefined;
  const browser = await chromium.launch({ headless: true, executablePath: exe, args: ['--no-sandbox'] });
  for (const [name, w, h] of [['desktop', 1440, 900], ['mobile', 390, 844]]) {
    const page = await browser.newPage({ viewport: { width: w, height: h } });
    const errs = [];
    page.on('pageerror', e => errs.push(e.message));
    await page.goto(base, { waitUntil: 'networkidle' });
    await page.waitForTimeout(500);
    await page.fill('#login-user', 'admin');
    await page.fill('#login-pass', 'admin123');
    await page.click('#login-submit');
    await page.waitForTimeout(1500);
    const visible = await page.locator('#app-view').isVisible();
    // 主题切换验证（移动端顶栏主题按钮隐藏，用 localStorage 注入验证主题系统）
    if (w > 720) {
      await page.click('.theme-btn[data-theme-set="dark"]');
      await page.waitForTimeout(200);
      var darkOk = (await page.getAttribute('html', 'data-theme')) === 'dark';
      await page.click('.theme-btn[data-theme-set="light"]');
      await page.waitForTimeout(200);
      var lightOk = (await page.getAttribute('html', 'data-theme')) === 'light';
    } else {
      await page.evaluate(() => localStorage.setItem('ctyun_theme', 'dark'));
      await page.reload({ waitUntil: 'networkidle' });
      await page.waitForTimeout(1200);
      var darkOk = (await page.getAttribute('html', 'data-theme')) === 'dark';
      await page.evaluate(() => localStorage.setItem('ctyun_theme', 'light'));
      await page.reload({ waitUntil: 'networkidle' });
      await page.waitForTimeout(1200);
      var lightOk = (await page.getAttribute('html', 'data-theme')) === 'light';
    }
    // 移动端溢出检测
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    await page.screenshot({ path: `/tmp/smoke_${name}.png` });
    console.log(`${name}: app=${visible} theme=${darkOk && lightOk} overflow=${overflow} errors=${errs.length}`);
    if (!visible || !darkOk || !lightOk || overflow > 0 || errs.length) process.exitCode = 1;
    await page.close();
  }
  await browser.close();
})();
JS
  [ $? = 0 ] && ok "桌面端 + 移动端渲染正常（截图 /tmp/smoke_*.png）"
fi

echo -e "\n${YELLOW}=== 测试结果: ${GREEN}$PASS PASS${NC} / ${RED}$FAIL FAIL${NC} ===${NC}"
[ "$FAIL" = "0" ] || exit 1
