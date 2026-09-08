import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { LimitWindow, RuntimeInfo, SavedAccount, SwitchOutcome, UsageSnapshot } from "./types";

type LoadState = "idle" | "loading" | "ready" | "error";
type Theme = "light" | "dark";
type SavedUsageState = { state: LoadState; snapshot: UsageSnapshot | null; error: string | null };
type DialogState =
  | { type: "switch"; account: SavedAccount; restartCodex: boolean }
  | { type: "delete"; account: SavedAccount }
  | { type: "rename"; account: SavedAccount };

function errorText(reason: unknown) {
  return reason instanceof Error ? reason.message : String(reason);
}

function formatWindow(minutes: number | null) {
  if (!minutes) return "额度窗口";
  if (minutes % 10_080 === 0) return `${minutes / 10_080} 周额度`;
  if (minutes % 1_440 === 0) return `${minutes / 1_440} 天额度`;
  if (minutes % 60 === 0) return `${minutes / 60} 小时额度`;
  return `${minutes} 分钟额度`;
}

function formatExactTime(timestamp: number | null) {
  if (!timestamp) return "暂无重置时间";
  return new Intl.DateTimeFormat("zh-CN", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(timestamp * 1_000));
}

function formatRelativeTime(timestamp: number | null, now: number) {
  if (!timestamp) return "等待重置时间";
  const minutes = Math.max(0, Math.ceil((timestamp * 1_000 - now) / 60_000));
  if (minutes === 0) return "即将重置";
  if (minutes < 60) return `${minutes} 分钟后重置`;
  if (minutes < 1_440) {
    const hours = Math.floor(minutes / 60);
    const rest = minutes % 60;
    return rest ? `${hours} 小时 ${rest} 分后重置` : `${hours} 小时后重置`;
  }
  const days = Math.floor(minutes / 1_440);
  const hours = Math.floor((minutes % 1_440) / 60);
  return hours ? `${days} 天 ${hours} 小时后重置` : `${days} 天后重置`;
}

function formatCapturedAt(timestamp: number | null) {
  if (!timestamp) return "尚未同步";
  return new Intl.DateTimeFormat("zh-CN", { hour: "2-digit", minute: "2-digit" }).format(
    new Date(timestamp * 1_000),
  );
}

function initialTheme(): Theme {
  const saved = window.localStorage.getItem("codex-account-hub-theme");
  if (saved === "light" || saved === "dark") return saved;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function QuotaBlock({ window, tone, now }: { window: LimitWindow | null; tone: "primary" | "secondary"; now: number }) {
  const remaining = window?.remainingPercent ?? null;
  const width = remaining === null ? 0 : Math.min(100, Math.max(0, remaining));
  const severity = remaining !== null && remaining <= 10 ? "critical" : remaining !== null && remaining <= 30 ? "low" : "";
  return (
    <section className={`quota-block ${severity}`}>
      <div className="quota-heading">
        <span>{formatWindow(window?.windowDurationMins ?? null)}</span>
        <strong>{remaining === null ? "—" : `${Math.round(remaining)}%`}</strong>
      </div>
      <div className="quota-track" aria-label={`剩余额度 ${remaining ?? "未知"}%`}>
        <div className={`quota-fill ${tone}`} style={{ width: `${width}%` }} />
      </div>
      <div className="quota-time">
        <span>{formatRelativeTime(window?.resetsAt ?? null, now)}</span>
        <small>{formatExactTime(window?.resetsAt ?? null)}</small>
      </div>
    </section>
  );
}

function ConfirmDialog({ dialog, renameValue, busy, onRenameValue, onCancel, onConfirm }: {
  dialog: DialogState;
  renameValue: string;
  busy: boolean;
  onRenameValue: (value: string) => void;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const isRename = dialog.type === "rename";
  const isDelete = dialog.type === "delete";
  const title = isRename
    ? "重命名账号"
    : isDelete
      ? `删除 ${dialog.account.label}？`
      : dialog.restartCodex
        ? `切换到 ${dialog.account.label} 并重启？`
        : `仅切换到 ${dialog.account.label}？`;
  return (
    <div className="dialog-backdrop" role="presentation" onMouseDown={onCancel}>
      <section className="dialog-panel" role="dialog" aria-modal="true" aria-labelledby="dialog-title" onMouseDown={(event) => event.stopPropagation()}>
        <div className={`dialog-icon ${isDelete ? "danger" : ""}`}>{isDelete ? "!" : isRename ? "✎" : "↻"}</div>
        <h2 id="dialog-title">{title}</h2>
        {isRename ? (
          <label className="dialog-field">
            <span>账号名称</span>
            <input autoFocus maxLength={80} value={renameValue} onChange={(event) => onRenameValue(event.target.value)} onKeyDown={(event) => {
              if (event.key === "Enter" && renameValue.trim()) onConfirm();
            }} />
          </label>
        ) : (
          <p>{isDelete
            ? "只会删除这台电脑中的加密副本，不会注销线上账号。删除后若要再次使用，需要重新添加。"
            : dialog.restartCodex
              ? "应用会先加密保存当前账号，然后关闭所有 Codex 窗口、切换登录并重新启动。正在执行的任务会被中断。"
              : "应用会替换本机 Codex 登录，但不会关闭当前窗口。稍后需要手动重启 Codex 才能完全生效。"}</p>
        )}
        <div className="dialog-actions">
          <button className="button ghost" onClick={onCancel} disabled={busy}>取消</button>
          <button className={`button ${isDelete ? "danger" : "primary"}`} onClick={onConfirm} disabled={busy || (isRename && !renameValue.trim())}>
            {busy ? "处理中…" : isDelete ? "确认删除" : isRename ? "保存名称" : dialog.restartCodex ? "切换并重启" : "确认切换"}
          </button>
        </div>
      </section>
    </div>
  );
}

export default function App() {
  const [theme, setTheme] = useState<Theme>(initialTheme);
  const [runtime, setRuntime] = useState<RuntimeInfo | null>(null);
  const [snapshot, setSnapshot] = useState<UsageSnapshot | null>(null);
  const [state, setState] = useState<LoadState>("idle");
  const [error, setError] = useState<string | null>(null);
  const [savedAccounts, setSavedAccounts] = useState<SavedAccount[]>([]);
  const [savedUsage, setSavedUsage] = useState<Record<string, SavedUsageState>>({});
  const [refreshingVault, setRefreshingVault] = useState(false);
  const fullRefreshInFlight = useRef(false);
  const vaultRefreshInFlight = useRef(false);
  const [importing, setImporting] = useState(false);
  const [addingAccount, setAddingAccount] = useState(false);
  const [switchingAccount, setSwitchingAccount] = useState<string | null>(null);
  const [managingAccount, setManagingAccount] = useState<string | null>(null);
  const [openMenu, setOpenMenu] = useState<string | null>(null);
  const [dialog, setDialog] = useState<DialogState | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const [vaultError, setVaultError] = useState<string | null>(null);
  const [vaultNotice, setVaultNotice] = useState<string | null>(null);
  const [now, setNow] = useState(Date.now());

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    window.localStorage.setItem("codex-account-hub-theme", theme);
  }, [theme]);

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpenMenu(null);
        if (!switchingAccount && !managingAccount) setDialog(null);
      }
    };
    window.addEventListener("keydown", handleEscape);
    return () => window.removeEventListener("keydown", handleEscape);
  }, [managingAccount, switchingAccount]);

  const refreshSavedAccounts = useCallback(async (accounts: SavedAccount[]) => {
    if (!accounts.length || vaultRefreshInFlight.current) return;
    vaultRefreshInFlight.current = true;
    setRefreshingVault(true);
    setSavedUsage((previous) => {
      const next = { ...previous };
      for (const account of accounts) next[account.id] = { state: "loading", snapshot: previous[account.id]?.snapshot ?? null, error: null };
      return next;
    });
    try {
      await Promise.all(accounts.map(async (account) => {
        try {
          const usage = await invoke<UsageSnapshot>("query_saved_usage", { accountId: account.id });
          setSavedUsage((previous) => ({ ...previous, [account.id]: { state: "ready", snapshot: usage, error: null } }));
        } catch (reason) {
          setSavedUsage((previous) => ({ ...previous, [account.id]: { state: "error", snapshot: previous[account.id]?.snapshot ?? null, error: errorText(reason) } }));
        }
      }));
    } finally {
      vaultRefreshInFlight.current = false;
      setRefreshingVault(false);
    }
  }, []);

  const refresh = useCallback(async () => {
    if (fullRefreshInFlight.current || vaultRefreshInFlight.current) return;
    fullRefreshInFlight.current = true;
    setState("loading");
    setError(null);
    const runtimeAndUsage = Promise.allSettled([
      invoke<RuntimeInfo>("discover_runtime"),
      invoke<UsageSnapshot>("query_current_usage"),
    ]);
    let savedRefresh: Promise<void> = Promise.resolve();
    try {
      let savedResult: PromiseSettledResult<SavedAccount[]>;
      try {
        const accounts = await invoke<SavedAccount[]>("list_saved_accounts");
        savedResult = { status: "fulfilled", value: accounts };
        setSavedAccounts(accounts);
        savedRefresh = refreshSavedAccounts(accounts.filter((account) => !account.isActive));
      } catch (reason) {
        savedResult = { status: "rejected", reason };
      }

      const [runtimeResult, usageResult] = await runtimeAndUsage;
      if (runtimeResult.status === "fulfilled") setRuntime(runtimeResult.value);
      if (usageResult.status === "fulfilled") setSnapshot(usageResult.value);
      const failures = [runtimeResult, usageResult, savedResult]
        .filter((result): result is PromiseRejectedResult => result.status === "rejected")
        .map((result) => errorText(result.reason));
      setError(failures.length ? failures.join("；") : null);
      setState(usageResult.status === "fulfilled" ? "ready" : "error");
      await savedRefresh;
    } finally {
      fullRefreshInFlight.current = false;
    }
  }, [refreshSavedAccounts]);

  const importCurrent = useCallback(async () => {
    setImporting(true); setVaultError(null); setVaultNotice(null);
    try {
      await invoke<SavedAccount>("import_current_account", { label: null });
      setVaultNotice("当前账号已安全保存。");
      await refresh();
    } catch (reason) { setVaultError(errorText(reason)); }
    finally { setImporting(false); }
  }, [refresh]);

  const addAccount = useCallback(async () => {
    setAddingAccount(true); setVaultError(null); setVaultNotice(null);
    try {
      await invoke<SavedAccount>("add_account_with_login", { label: null });
      setVaultNotice("新账号已添加并安全保存。");
      await refresh();
    } catch (reason) { setVaultError(errorText(reason)); }
    finally { setAddingAccount(false); }
  }, [refresh]);

  const executeSwitch = useCallback(async (account: SavedAccount, restartCodex: boolean) => {
    setSwitchingAccount(account.id); setVaultError(null); setVaultNotice(null);
    try {
      const outcome = await invoke<SwitchOutcome>("switch_saved_account", { accountId: account.id, restartCodex });
      if (outcome.restartWarning) setVaultError(outcome.restartWarning);
      else if (outcome.restartSucceeded) setVaultNotice(`已切换到 ${outcome.account.label}，Codex 已重新启动。`);
      else setVaultNotice(`已切换到 ${outcome.account.label}，请稍后手动重启 Codex。`);
      setSnapshot(null);
      await refresh();
    } catch (reason) { setVaultError(errorText(reason)); }
    finally { setSwitchingAccount(null); }
  }, [refresh]);

  const executeRename = useCallback(async (account: SavedAccount, label: string) => {
    setManagingAccount(account.id); setVaultError(null);
    try {
      const updated = await invoke<SavedAccount>("rename_saved_account", { accountId: account.id, label });
      setSavedAccounts((accounts) => accounts.map((item) => item.id === account.id ? { ...updated, isActive: item.isActive } : item));
      setVaultNotice("账号名称已更新。");
    } catch (reason) { setVaultError(errorText(reason)); }
    finally { setManagingAccount(null); }
  }, []);

  const reauthorizeAccount = useCallback(async (account: SavedAccount) => {
    setOpenMenu(null);
    setManagingAccount(account.id); setVaultError(null); setVaultNotice(null);
    try {
      await invoke<SavedAccount>("reauthorize_account", { accountId: account.id });
      setVaultNotice(`${account.label} 已重新授权。`);
      await refresh();
    } catch (reason) { setVaultError(errorText(reason)); }
    finally { setManagingAccount(null); }
  }, [refresh]);

  const executeDelete = useCallback(async (account: SavedAccount) => {
    setManagingAccount(account.id); setVaultError(null);
    try {
      await invoke("delete_saved_account", { accountId: account.id });
      setSavedAccounts((accounts) => accounts.filter((item) => item.id !== account.id));
      setSavedUsage((usage) => { const next = { ...usage }; delete next[account.id]; return next; });
      setVaultNotice(`${account.label} 已从本机保险库删除。`);
    } catch (reason) { setVaultError(errorText(reason)); }
    finally { setManagingAccount(null); }
  }, []);

  const confirmDialog = useCallback(async () => {
    if (!dialog) return;
    setOpenMenu(null);
    const current = dialog;
    if (current.type === "rename" && !renameValue.trim()) return;
    if (current.type === "switch") await executeSwitch(current.account, current.restartCodex);
    if (current.type === "delete") await executeDelete(current.account);
    if (current.type === "rename") await executeRename(current.account, renameValue.trim());
    setDialog(null);
  }, [dialog, executeDelete, executeRename, executeSwitch, renameValue]);

  useEffect(() => { void refresh(); }, [refresh]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen("tray-refresh-requested", () => void refresh()).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [refresh]);

  const sortedAccounts = useMemo(() => [...savedAccounts].sort((left, right) => Number(right.isActive) - Number(left.isActive)), [savedAccounts]);
  const activeAccount = sortedAccounts.find((account) => account.isActive) ?? null;
  const operationBusy = importing || addingAccount || switchingAccount !== null || managingAccount !== null;
  const currentName = activeAccount?.label ?? snapshot?.account?.email ?? "未识别当前账号";

  useEffect(() => {
    void invoke("update_tray_current_account", { label: currentName }).catch(() => undefined);
  }, [currentName]);

  return (
    <main className="app-shell" onClick={() => setOpenMenu(null)}>
      <header className="topbar">
        <div className="brand"><span className="brand-mark">C</span><div><h1>Codex Account Hub</h1><p>账号额度与安全切换</p></div></div>
        <div className="top-actions">
          <button className="icon-button" aria-label={theme === "dark" ? "切换到浅色模式" : "切换到深色模式"} title={theme === "dark" ? "切换到浅色模式" : "切换到深色模式"} onClick={() => setTheme((value) => value === "dark" ? "light" : "dark")}>
            {theme === "dark" ? "☀" : "☾"}
          </button>
          <button className="button secondary" onClick={() => void refresh()} disabled={state === "loading" || refreshingVault || operationBusy}>
            <span className={state === "loading" || refreshingVault ? "spin" : ""}>↻</span>{state === "loading" || refreshingVault ? "正在刷新" : "刷新全部"}
          </button>
        </div>
      </header>

      <section className="summary-bar">
        <div className={`summary-current ${snapshot?.account ? "" : "unknown"}`}><span className="status-dot" /><span>当前</span><strong>{currentName}</strong><span className="plan-label">{activeAccount?.planType?.toUpperCase() ?? snapshot?.account?.planType?.toUpperCase() ?? "UNKNOWN"}</span></div>
        <div className="summary-meta"><span>{savedAccounts.length} 个账号</span><span className="divider" /><span>{runtime?.codexVersion ?? "Codex runtime"}</span></div>
      </section>

      {(vaultNotice || vaultError || error || addingAccount) && (
        <section className={`notice-bar ${vaultError || error ? "error" : ""}`}>
          <span>{vaultError || error ? "!" : addingAccount ? "↗" : "✓"}</span>
          <p>{vaultError ?? error ?? (addingAccount ? "请在刚打开的官方页面完成登录，当前 Codex 账号不会被切换。" : vaultNotice)}</p>
          {(vaultError || error) && <button onClick={() => void refresh()}>重试</button>}
        </section>
      )}

      <section className="accounts-section">
        <div className="section-heading">
          <div><span className="section-kicker">账号看板</span><h2>所有账号额度</h2><p>进度条显示剩余额度，重置时间每 30 秒更新。</p></div>
          <div className="section-actions">
            {!activeAccount && <button className="button secondary" onClick={() => void importCurrent()} disabled={operationBusy}>{importing ? "正在保存…" : "导入当前账号"}</button>}
            <button className="button primary" onClick={() => void addAccount()} disabled={operationBusy}><span>＋</span>{addingAccount ? "等待登录…" : "添加账号"}</button>
          </div>
        </div>

        {sortedAccounts.length === 0 ? (
          <section className="empty-state"><div className="empty-icon">C</div><h3>还没有保存账号</h3><p>先导入当前登录，再添加其他账号，就能在这里统一查看额度。</p><button className="button primary" onClick={() => void importCurrent()} disabled={operationBusy}>{importing ? "正在保存…" : "导入当前账号"}</button></section>
        ) : (
          <div className="account-list">
            {sortedAccounts.map((account) => {
              const storedUsage = savedUsage[account.id];
              const usage: SavedUsageState = account.isActive && snapshot
                ? { state: state === "loading" ? "loading" : error ? "error" : "ready", snapshot, error }
                : storedUsage ?? { state: account.isActive && state === "loading" ? "loading" : "idle", snapshot: null, error: null };
              return (
                <article className={`account-row ${account.isActive ? "active" : ""}`} key={account.id}>
                  <div className="account-row-heading">
                    <div className="account-identity"><div className="avatar">{account.label.trim().charAt(0).toUpperCase() || "C"}</div><div><div className="account-title"><h3>{account.label}</h3>{account.isActive && <span className="active-badge">当前账号</span>}</div><div className="account-subtitle"><span>{account.planType?.toUpperCase() ?? "UNKNOWN PLAN"}</span><span className={`sync-state ${usage.state}`}>{usage.state === "loading" ? "刷新中" : usage.state === "error" ? usage.snapshot ? "缓存数据" : "读取失败" : usage.state === "ready" ? "已同步" : "等待刷新"}</span><span>{formatCapturedAt(usage.snapshot?.capturedAt ?? null)}</span></div></div></div>
                    <div className="account-actions">
                      {!account.isActive && <button className="button switch-primary" onClick={() => setDialog({ type: "switch", account, restartCodex: true })} disabled={operationBusy}>{switchingAccount === account.id ? "正在切换…" : "切换并重启"}</button>}
                      <div className="menu-wrap" onClick={(event) => event.stopPropagation()}>
                        <button className="icon-button menu-trigger" aria-label={`${account.label} 更多操作`} aria-expanded={openMenu === account.id} onClick={() => setOpenMenu((value) => value === account.id ? null : account.id)} disabled={operationBusy}>•••</button>
                        {openMenu === account.id && <div className="account-menu">
                          <button onClick={() => { setOpenMenu(null); void refreshSavedAccounts([account]); }}>↻<span>刷新额度</span></button>
                          {!account.isActive && <button onClick={() => { setOpenMenu(null); setDialog({ type: "switch", account, restartCodex: false }); }}>⇄<span>仅切换账号</span></button>}
                          <button onClick={() => { setOpenMenu(null); setRenameValue(account.label); setDialog({ type: "rename", account }); }}>✎<span>重命名</span></button>
                          <button onClick={() => void reauthorizeAccount(account)}>↗<span>重新授权</span></button>
                          {!account.isActive && <button className="danger" onClick={() => { setOpenMenu(null); setDialog({ type: "delete", account }); }}>×<span>删除账号</span></button>}
                        </div>}
                      </div>
                    </div>
                  </div>
                  {usage.error && <p className="account-error" title={usage.error}>刷新失败，正在显示上次成功数据：{usage.error}</p>}
                  <div className={`quota-grid ${usage.state === "loading" && !usage.snapshot ? "loading" : ""}`}>
                    <QuotaBlock window={usage.snapshot?.primary ?? null} tone="primary" now={now} />
                    <QuotaBlock window={usage.snapshot?.secondary ?? null} tone="secondary" now={now} />
                    <div className="credits-block"><span>重置券</span><strong>{usage.snapshot?.resetCreditsAvailable ?? "—"}</strong><small>当前可用</small></div>
                  </div>
                </article>
              );
            })}
          </div>
        )}
      </section>

      <footer className="app-footer"><span><span className="status-dot" /> 本地安全读取</span><span>关闭窗口后驻留系统托盘</span></footer>
      {dialog && <ConfirmDialog dialog={dialog} renameValue={renameValue} busy={switchingAccount !== null || managingAccount !== null} onRenameValue={setRenameValue} onCancel={() => { if (!switchingAccount && !managingAccount) setDialog(null); }} onConfirm={() => void confirmDialog()} />}
    </main>
  );
}
