import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { createServer } from "node:http";
import { build } from "vite";

// Browser integration tests use a virtual IPC adapter; no Tauri process or real account is accessed.
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || "playwright");
const fixtures = `
const a = 'aaaaaaaaaaaaaaaaaaaaaaaa', b = 'bbbbbbbbbbbbbbbbbbbbbbbb';
const scenario = new URLSearchParams(location.search).get('scenario');
const originalNow = Date.now;
let offset = 0;
Date.now = () => originalNow() + offset;
export const state = { active: scenario === 'recovery' ? null : a, failure: scenario === 'auth-error' ? 'authentication_required' : scenario === 'stale' ? 'network' : scenario === 'recovery' ? 'local_io' : null, listeners: new Map(), mismatch: false, currentQueries: 0, savedQueries: 0, listCalls: 0, pendingLogin: null, renewed: false, switchCalls: 0, restartCalls: 0, recoveryCalls: 0 };
export function progress(requestId, phase) {
  for (const callback of state.listeners.get('login-progress') || []) callback({ payload: { requestId, phase } });
}
export async function login(args) {
  state.loginRequest = args.requestId;
  progress(args.requestId, scenario === 'saving-login' ? 'saving' : 'waiting');
  await new Promise((resolve, reject) => { state.resolveLogin = resolve; state.rejectLogin = reject; });
}
export const accounts = [
  { id: a, label: '测试账号 A', email: 'a***@example.com', planType: 'plus', importedAt: 1 },
  { id: b, label: '测试账号 B', email: 'b***@example.com', planType: 'plus', importedAt: 1 },
];
export function snapshot(id, old = false) {
  return { accountId: id, account: { accountType: 'chatgpt', email: id === a ? 'a***@example.com' : 'b***@example.com', planType: 'plus' },
    primary: { usedPercent: id === a ? state.renewed ? 10 : 30 : 60, remainingPercent: id === a ? state.renewed ? 90 : 70 : 40, windowDurationMins: 300, resetsAt: Math.floor(Date.now() / 1000) + 3600 },
    secondary: null, rateLimitReachedType: null, resetCreditsAvailable: 2, capturedAt: Math.floor(Date.now() / 1000) - (old ? 1900 : 0) };
}
window.__hubTest = { a, b, state, advance: ms => offset += ms, emit: event => { for (const callback of state.listeners.get(event) || []) callback({}); } };
`;
const core = `
import { state, accounts, snapshot, login } from 'virtual:hub-fixtures';
export async function invoke(command, args = {}) {
  const scenario = new URLSearchParams(location.search).get('scenario');
  switch (command) {
    case 'recovery_status': return { messages: scenario === 'startup-recovery' ? ['恢复记录暂时无法处理，请重试恢复'] : [], needsAttention: scenario === 'startup-recovery', restartSuggested: false };
    case 'retry_recovery': state.recoveryCalls++; return { messages: ['已从加密恢复记录还原上次有效登录。'], needsAttention: false, restartSuggested: true };
    case 'restart_codex': state.restartCalls++; return { succeeded: true, warning: null };
    case 'cancel_login':
      if (args.requestId !== state.loginRequest || scenario === 'saving-login') return false;
      setTimeout(() => state.rejectLogin('登录已取消'), 30);
      return true;
    case 'add_account_with_login': await login(args); return accounts[0];
    case 'list_saved_accounts':
      state.listCalls++;
      if (state.pendingLogin) await state.pendingLogin;
      return { accounts: accounts.map(a => ({ ...a, isActive: a.id === state.active })), currentAccountId: state.active };
    case 'discover_runtime': return { codexPath: 'fixture', codexHome: 'fixture', codexVersion: 'fixture' };
    case 'load_usage_cache': return new URLSearchParams(location.search).get('scenario') === 'stale' ? { [accounts[0].id]: snapshot(accounts[0].id, true) } : {};
    case 'query_current_usage':
      state.currentQueries++;
      if (new URLSearchParams(location.search).get('scenario') === 'concurrent-reauth' && state.currentQueries === 1) {
        const old = snapshot(state.active);
        await new Promise(resolve => { state.releaseQuery = resolve; });
        return old;
      }
      await new Promise(resolve => setTimeout(resolve, 30));
      if (state.failure) throw { code: state.failure, message: state.failure === 'authentication_required' ? '登录失效，请重新授权' : state.failure === 'local_io' ? '当前认证文件损坏，可切换到已保存账号恢复' : '网络连接失败', reauthRequired: state.failure === 'authentication_required' };
      return snapshot(state.active);
    case 'query_saved_usage':
      state.savedQueries++;
      await new Promise(resolve => setTimeout(resolve, 50));
      return snapshot(state.mismatch ? accounts[0].id : args.accountId);
    case 'reauthorize_account':
      if (scenario === 'cancel-reauth') await login(args);
      if (state.releaseQuery) {
        state.pendingLogin = new Promise(resolve => setTimeout(resolve, 100));
        state.releaseQuery();
        await state.pendingLogin;
        state.renewed = true;
        state.pendingLogin = null;
      }
      state.failure = null;
      return { account: accounts.find(a => a.id === args.accountId), currentAuthUpdated: args.accountId === state.active };
    case 'switch_saved_account':
      state.switchCalls++; state.active = args.accountId; state.failure = null;
      return { account: accounts.find(a => a.id === args.accountId), restartRequested: args.restartCodex, restartSucceeded: false, codexWasRunning: false, processesClosed: 0, restartWarning: scenario === 'restart-failure' ? '未检测到 Codex 启动' : null };
    case 'update_tray_current_account': return;
    default: throw new Error('Unexpected test command: ' + command);
  }
}
`;
const events = `
import { state } from 'virtual:hub-fixtures';
export async function listen(event, callback) {
  const listeners = state.listeners.get(event) || new Set();
  state.listeners.set(event, listeners); listeners.add(callback);
  return () => listeners.delete(callback);
}
`;
const modules = new Map([
  ["@tauri-apps/api/core", core],
  ["@tauri-apps/api/event", events],
  ["virtual:hub-fixtures", fixtures],
]);
const bundle = await build({
  configFile: false,
  root: fileURLToPath(new URL("../", import.meta.url)),
  esbuild: { jsx: "automatic" },
  plugins: [{
    name: "fixture-ipc",
    enforce: "pre",
    resolveId(id) { if (modules.has(id)) return `\0${id}`; },
    load(id) { return modules.get(id.slice(1)); },
  }],
  build: { write: false, minify: false },
});
const assets = new Map(bundle.output.map(item => [item.fileName, item.type === "chunk" ? item.code : item.source]));
const server = createServer((request, response) => {
  const pathname = new URL(request.url, "http://127.0.0.1").pathname;
  const name = pathname === "/" ? "index.html" : pathname.slice(1);
  if (!assets.has(name)) { response.writeHead(404); response.end(); return; }
  response.setHeader("Content-Type", name.endsWith(".html") ? "text/html; charset=utf-8" : name.endsWith(".css") ? "text/css" : "text/javascript");
  response.end(assets.get(name));
});
let browser;
try {
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  browser = await chromium.launch({ channel: process.env.TEST_BROWSER_CHANNEL || "msedge", headless: true });
  const page = await browser.newPage({ viewport: { width: 1120, height: 760 } });
  const errors = [];
  if (process.env.DEBUG_HUB_UI) {
    page.on("request", request => console.log("REQUEST", request.url()));
    page.on("response", response => console.log("RESPONSE", response.status(), response.url()));
    page.on("console", message => console.log("BROWSER", message.text()));
  }
  page.setDefaultTimeout(10_000);
  page.on("pageerror", error => { errors.push(error.message); console.error(error.message); });
  const row = label => page.locator(".account-row").filter({ has: page.getByRole("heading", { name: label, exact: true }) });
  const settled = async () => {
    await page.waitForFunction(() => document.querySelectorAll(".account-row").length === 2 && !document.querySelector(".sync-state.loading") && !document.querySelector(".spin"));
  };
  const check = (condition, message) => { assert.ok(condition, message); console.log(`PASS ${message}`); };

  await page.goto(base, { waitUntil: "commit" });
  await settled();
  check(await page.locator("select").inputValue() === "10", "首次默认 10 分钟刷新");
  check(await row("测试账号 A").locator(".quota-heading strong").first().innerText() === "70%", "当前账号额度正确");

  const beforeFocus = await page.evaluate(() => ({ current: window.__hubTest.state.currentQueries, saved: window.__hubTest.state.savedQueries, list: window.__hubTest.state.listCalls }));
  await page.evaluate(() => {
    for (let i = 0; i < 5; i++) {
      window.dispatchEvent(new Event('blur'));
      window.dispatchEvent(new Event('focus'));
      window.dispatchEvent(new Event('resize'));
      window.__hubTest.emit('tauri://move');
    }
  });
  await page.waitForTimeout(200);
  check(await page.evaluate(expected => window.__hubTest.state.currentQueries === expected.current && window.__hubTest.state.savedQueries === expected.saved && window.__hubTest.state.listCalls === expected.list, beforeFocus), '拖动相关的网页焦点和移动事件不触发刷新');
  await page.evaluate(() => { for (let i = 0; i < 5; i++) window.__hubTest.emit('tauri://focus'); });
  await page.waitForFunction(previous => window.__hubTest.state.listCalls > previous, beforeFocus.list);
  await page.waitForTimeout(200);
  check(await page.evaluate(expected => window.__hubTest.state.currentQueries === expected.current && window.__hubTest.state.savedQueries === expected.saved && window.__hubTest.state.listCalls === expected.list + 1, beforeFocus), '重复窗口激活合并身份检查，账号未变不查询额度');

  await page.evaluate(() => { window.__hubTest.state.active = window.__hubTest.b; window.__hubTest.emit('tauri://focus'); });
  await page.waitForFunction(() => document.querySelector(".account-row.active h3")?.textContent === "测试账号 B");
  await settled();
  check(await row("测试账号 B").locator(".quota-heading strong").first().innerText() === "40%", "外部换号后身份与额度同步");
  check(await row("测试账号 A").locator(".quota-heading strong").first().innerText() === "70%", "旧账号没有串入新账号额度");

  await page.evaluate(() => { window.__hubTest.state.active = window.__hubTest.a; window.__hubTest.advance(11 * 60_000); window.__hubTest.emit("background-refresh-clock"); });
  await page.waitForFunction(() => document.querySelector(".account-row.active h3")?.textContent === "测试账号 A");
  await settled();
  check(await row("测试账号 A").locator(".quota-heading strong").first().innerText() === "70%", "后台时钟触发时重新识别外部换号");

  await page.goto(`${base}/?scenario=auth-error`);
  await settled();
  check((await row("测试账号 A").locator(".account-error").innerText()).includes("登录失效"), "首次查询失败在当前卡片显示错误");
  await row("测试账号 A").getByRole("button", { name: "重新授权", exact: true }).click();
  await page.waitForFunction(() => [...document.querySelectorAll(".notice-bar")].some(el => el.textContent.includes("当前登录和保险库已更新")));
  await settled();
  check(await row("测试账号 A").locator(".quota-heading strong").first().innerText() === "70%", "重新授权完成后自动刷新并恢复额度");

  await page.goto(`${base}/?scenario=concurrent-reauth`);
  await page.waitForFunction(() => typeof window.__hubTest.state.releaseQuery === "function");
  await row("测试账号 A").getByRole("button", { name: "测试账号 A 更多操作" }).click();
  await row("测试账号 A").getByRole("button", { name: /重新授权/ }).click();
  await page.waitForFunction(() => window.__hubTest.state.currentQueries >= 2 && document.querySelector(".account-row.active .quota-heading strong")?.textContent === "90%");
  check(true, "授权完成与旧查询重叠时仍执行后续刷新");

  await page.goto(`${base}/?scenario=recovery`);
  await settled();
  await row("测试账号 A").getByRole("button", { name: "测试账号 A 更多操作" }).click();
  await row("测试账号 A").getByRole("button", { name: "仅切换账号" }).click();
  await page.getByRole("button", { name: "确认切换", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".account-row.active h3")?.textContent === "测试账号 A");
  await settled();
  check(await row("测试账号 A").locator(".quota-heading strong").first().innerText() === "70%", "主认证异常时可通过已保存账号恢复");

  await page.goto(`${base}/?scenario=stale`);
  await settled();
  check((await row("测试账号 A").locator(".sync-state").innerText()).includes("数据陈旧"), "旧缓存明确标注数据陈旧");
  check(await row("测试账号 A").locator(".quota-heading strong").first().innerText() === "70%", "网络失败保留最后成功额度");
  await page.evaluate(() => { window.__hubTest.state.mismatch = true; });
  await page.getByRole("button", { name: "刷新全部" }).click();
  await settled();
  check((await row("测试账号 B").locator(".account-error").innerText()).includes("额度与账号不一致"), "错误归属的响应被拒绝");
  check(await row("测试账号 B").locator(".quota-heading strong").first().innerText() === "40%", "错误归属响应不覆盖已有额度");
  for (const scenario of ['cancel-login', 'cancel-reauth']) {
    await page.goto(`${base}/?scenario=${scenario}`);
    await settled();
    if (scenario === 'cancel-login') await page.getByRole('button', { name: '添加账号' }).click();
    else {
      await row('测试账号 A').getByRole('button', { name: '测试账号 A 更多操作' }).click();
      await row('测试账号 A').getByRole('button', { name: /重新授权/ }).click();
    }
    await page.getByRole('button', { name: '取消登录' }).click();
    await page.waitForFunction(() => !document.querySelector('.login-status') && [...document.querySelectorAll('.notice-bar')].some(el => el.textContent.includes('登录已取消')));
    check(await page.getByRole('button', { name: '添加账号' }).isEnabled(), `${scenario} 取消后解除操作锁定`);
    check(await page.evaluate(() => window.__hubTest.state.active === window.__hubTest.a && window.__hubTest.state.switchCalls === 0), `${scenario} 保留当前账号`);
    await page.waitForFunction(() => ![...document.querySelectorAll('.notice-bar')].some(el => el.textContent.includes('登录已取消')));
    check(true, `${scenario} 完成提示自动消失`);
  }

  await page.goto(`${base}/?scenario=saving-login`);
  await settled();
  await page.getByRole('button', { name: '添加账号' }).click();
  await page.waitForFunction(() => document.querySelector('.login-status')?.textContent.includes('正在安全保存账号'));
  check(await page.getByRole('button', { name: '取消登录' }).isDisabled(), '提交保存期间禁止取消');
  await page.waitForTimeout(5200);
  check(await page.locator('.login-status').isVisible(), '进行中的登录提示不会被完成提示计时器清除');
  await page.evaluate(() => window.__hubTest.state.resolveLogin());
  await page.waitForFunction(() => !document.querySelector('.login-status'));
  await settled();

  await page.goto(`${base}/?scenario=startup-recovery`);
  await settled();
  check((await page.locator('.recovery-status').innerText()).includes('请重试恢复'), '启动恢复异常有可操作提示');
  await page.waitForTimeout(5200);
  check(await page.locator('.recovery-status.error').isVisible(), '恢复异常不会定时消失');
  await page.getByRole('button', { name: '重试恢复' }).click();
  await page.waitForFunction(() => document.querySelector('.recovery-status')?.textContent.includes('还原上次有效登录'));
  await settled();
  check(await page.evaluate(() => window.__hubTest.state.recoveryCalls === 1 && window.__hubTest.state.restartCalls === 0), '恢复重试成功后不会自动重启');
  await page.waitForFunction(() => !document.querySelector('.recovery-status'));
  check(await page.getByRole('button', { name: '重新启动 Codex', exact: true }).isVisible(), '恢复成功提示自动消失，待重启入口仍保留');

  await page.goto(`${base}/?scenario=restart-failure`);
  await settled();
  await row('测试账号 B').getByRole('button', { name: '切换并重启', exact: true }).click();
  await page.getByRole('dialog').getByRole('button', { name: '切换并重启', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('.restart-status')?.textContent.includes('重启未完成'));
  await settled();
  check(await page.locator('.account-row.active h3').innerText() === '测试账号 B', '重启失败仍保留切换成功结果');
  await page.waitForTimeout(5200);
  check((await page.locator('.restart-status').innerText()).includes('重启未完成'), '重启失败及重试入口持续保留');
  await page.getByRole('button', { name: '重新启动 Codex', exact: true }).click();
  check(await page.evaluate(() => window.__hubTest.state.restartCalls === 0), '单独重启在确认前不执行');
  await page.getByRole('button', { name: '确认重启', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('.restart-status')?.textContent.includes('已检测到 Codex 启动'));
  check(await page.evaluate(() => window.__hubTest.state.switchCalls === 1 && window.__hubTest.state.restartCalls === 1), '重试重启不重复切换账号');
  await page.waitForFunction(() => !document.querySelector('.restart-status'));
  check(true, '重启成功提示自动消失');
  assert.deepEqual(errors, [], "浏览器没有未捕获异常");
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
