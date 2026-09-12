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
export const state = { active: scenario === 'recovery' ? null : a, failure: scenario === 'auth-error' ? 'authentication_required' : scenario === 'stale' ? 'network' : scenario === 'recovery' ? 'local_io' : null, listeners: new Map(), mismatch: false, currentQueries: 0, pendingLogin: null, renewed: false };
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
import { state, accounts, snapshot } from 'virtual:hub-fixtures';
export async function invoke(command, args = {}) {
  switch (command) {
    case 'list_saved_accounts':
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
      await new Promise(resolve => setTimeout(resolve, 50));
      return snapshot(state.mismatch ? accounts[0].id : args.accountId);
    case 'reauthorize_account':
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
      state.active = args.accountId; state.failure = null;
      return { account: accounts.find(a => a.id === args.accountId), restartRequested: args.restartCodex, restartSucceeded: false, codexWasRunning: false, processesClosed: 0, restartWarning: null };
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

  await page.evaluate(() => { window.__hubTest.state.active = window.__hubTest.b; window.dispatchEvent(new Event("focus")); });
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
  await page.waitForFunction(() => document.querySelector(".notice-bar")?.textContent.includes("当前登录和保险库已更新"));
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
  assert.deepEqual(errors, [], "浏览器没有未捕获异常");
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
