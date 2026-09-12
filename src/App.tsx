import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { AccountList, LimitWindow, LoginProgress, RecoveryReport, RestartResult, ReauthorizationOutcome, RuntimeInfo, SavedAccount, SwitchOutcome, UsageSnapshot } from "./types";
import { isSnapshotStale, parseAutoRefresh, snapshotForAccount } from "./dashboard-state";
import type { AutoRefreshMinutes } from "./dashboard-state";

type LoadState = "idle" | "loading" | "ready" | "error";
const NOTICE_DURATION_MS = 5_000;
type Theme = "light" | "dark";
type UsageErrorCode = "authentication_required" | "account_changed" | "timeout" | "runtime_unavailable" | "network" | "app_server" | "local_io" | "unknown";
type UsageError = { code: UsageErrorCode; message: string; reauthRequired: boolean };
type SavedUsageState = { state: LoadState; snapshot: UsageSnapshot | null; error: UsageError | null; cached?: boolean };
type DialogState =
  | { type: "switch"; account: SavedAccount; restartCodex: boolean }
  | { type: "delete"; account: SavedAccount }
  | { type: "rename"; account: SavedAccount };

function usageError(reason: unknown): UsageError {
  if (reason && typeof reason === "object" && "message" in reason) {
    const value = reason as Partial<UsageError>;
    return {
      code: value.code ?? "unknown",
      message: String(value.message),
      reauthRequired: Boolean(value.reauthRequired),
    };
  }
  return { code: "unknown", message: reason instanceof Error ? reason.message : String(reason), reauthRequired: false };
}

function errorText(reason: unknown) {
  return usageError(reason).message;
}

function errorLabel(error: UsageError) {
  if (error.code === "authentication_required") return "登录失效";
  if (error.code === "timeout") return "查询超时";
  if (error.code === "runtime_unavailable") return "Codex 不可用";
  if (error.code === "network") return "网络异常";
  if (error.code === "local_io") return "本地文件异常";
  if (error.code === "account_changed") return "账号已变化";
  return "读取失败";
}

function initialAutoRefresh(): AutoRefreshMinutes {
  return parseAutoRefresh(window.localStorage.getItem("codex-account-hub-auto-refresh"));
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
  return new Intl.DateTimeFormat("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" }).format(
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
              ? "应用会先备份并切换登录，然后关闭所有 Codex 窗口并重新启动。正在执行的任务会被中断。"
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
  const [currentAccountId, setCurrentAccountId] = useState<string | null>(null);
  const currentAccountIdRef = useRef<string | null>(null);
  const [autoRefreshMinutes, setAutoRefreshMinutes] = useState<AutoRefreshMinutes>(initialAutoRefresh);
  const [savedAccounts, setSavedAccounts] = useState<SavedAccount[]>([]);
  const [savedUsage, setSavedUsage] = useState<Record<string, SavedUsageState>>({});
  const [refreshingVault, setRefreshingVault] = useState(false);
  const cacheHydrated = useRef(false);
  const fullRefreshInFlight = useRef(false);
  const pendingOperationRefresh = useRef(false);
  const vaultRefreshInFlight = useRef(false);
  const backgroundRefreshInFlight = useRef(false);
  const backgroundFailures = useRef(new Map<string, { failures: number; nextAllowedAt: number }>());
  const nextBackgroundRefreshAt = useRef(Date.now() + autoRefreshMinutes * 60_000);
  const autoRefreshMinutesRef = useRef(autoRefreshMinutes);
  const operationBusyRef = useRef(false);
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
  const [loginProgress, setLoginProgress] = useState<LoginProgress | null>(null);
  const [recoveryReport, setRecoveryReport] = useState<RecoveryReport | null>(null);
  const [recovering, setRecovering] = useState(false);
  const [restartNeeded, setRestartNeeded] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [restartDialog, setRestartDialog] = useState(false);
  const [restartMessage, setRestartMessage] = useState<string | null>(null);

  useEffect(() => {
    if (!vaultNotice || vaultError || error) return;
    const timer = window.setTimeout(() => setVaultNotice(null), NOTICE_DURATION_MS);
    return () => window.clearTimeout(timer);
  }, [vaultNotice, vaultError, error]);

  useEffect(() => {
    if (!restartMessage || restartNeeded) return;
    const timer = window.setTimeout(() => setRestartMessage(null), NOTICE_DURATION_MS);
    return () => window.clearTimeout(timer);
  }, [restartMessage, restartNeeded]);

  useEffect(() => {
    if (!recoveryReport?.messages.length || recoveryReport.needsAttention) return;
    const timer = window.setTimeout(() => setRecoveryReport(null), NOTICE_DURATION_MS);
    return () => window.clearTimeout(timer);
  }, [recoveryReport]);

  useEffect(() => {
    void invoke<RecoveryReport>("recovery_status").then((report) => {
      setRecoveryReport(report);
      if (report.restartSuggested) setRestartNeeded(true);
    }).catch((reason) => setRecoveryReport({ messages: [errorText(reason)], needsAttention: true, restartSuggested: false }));
  }, []);

  const runLogin = useCallback(async <T,>(command: string, args: Record<string, unknown>): Promise<T> => {
    const requestId = crypto.randomUUID();
    setLoginProgress({ requestId, phase: "starting" });
    let unlisten: (() => void) | undefined;
    try {
      unlisten = await listen<LoginProgress>("login-progress", ({ payload }) => {
        if (payload.requestId === requestId) setLoginProgress(payload);
      });
      return await invoke<T>(command, { ...args, requestId });
    } finally {
      unlisten?.();
      setLoginProgress(null);
    }
  }, []);

  const cancelLogin = async () => {
    if (!loginProgress) return;
    try {
      const cancelled = await invoke<boolean>("cancel_login", { requestId: loginProgress.requestId });
      if (cancelled) setLoginProgress((current) => current ? { ...current, phase: "cancelling" } : null);
    } catch (reason) { setVaultError(errorText(reason)); }
  };

  const reconcileAccounts = useCallback(async () => {
    const listing = await invoke<AccountList>("list_saved_accounts");
    currentAccountIdRef.current = listing.currentAccountId;
    setCurrentAccountId(listing.currentAccountId);
    setSavedAccounts(listing.accounts);
    setSnapshot((previous) => snapshotForAccount(previous, listing.currentAccountId));
    return listing;
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    window.localStorage.setItem("codex-account-hub-theme", theme);
  }, [theme]);

  useEffect(() => {
    window.localStorage.setItem("codex-account-hub-auto-refresh", String(autoRefreshMinutes));
    autoRefreshMinutesRef.current = autoRefreshMinutes;
    nextBackgroundRefreshAt.current = autoRefreshMinutes === 0
      ? Number.POSITIVE_INFINITY
      : Date.now() + autoRefreshMinutes * 60_000;
  }, [autoRefreshMinutes]);

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpenMenu(null);
        if (!switchingAccount && !managingAccount) setDialog(null);
        if (!restarting) setRestartDialog(false);
      }
    };
    window.addEventListener("keydown", handleEscape);
    return () => window.removeEventListener("keydown", handleEscape);
  }, [managingAccount, switchingAccount, restarting]);

  const refreshSavedAccounts = useCallback(async (accounts: SavedAccount[]) => {
    const outcomes: Record<string, UsageError | null> = {};
    if (!accounts.length || vaultRefreshInFlight.current) return outcomes;
    vaultRefreshInFlight.current = true;
    setRefreshingVault(true);
    setSavedUsage((previous) => {
      const next = { ...previous };
      for (const account of accounts) next[account.id] = { ...previous[account.id], state: "loading", snapshot: previous[account.id]?.snapshot ?? null, error: null };
      return next;
    });
    try {
      await Promise.all(accounts.map(async (account) => {
        try {
          const usage = await invoke<UsageSnapshot>("query_saved_usage", { accountId: account.id });
          if (!snapshotForAccount(usage, account.id)) throw { code: "account_changed", message: "额度与账号不一致，请重新刷新", reauthRequired: false };
          setSavedUsage((previous) => ({ ...previous, [account.id]: { state: "ready", snapshot: usage, error: null } }));
          backgroundFailures.current.delete(account.id);
          outcomes[account.id] = null;
        } catch (reason) {
          const queryError = usageError(reason);
          setSavedUsage((previous) => ({ ...previous, [account.id]: { state: "error", snapshot: previous[account.id]?.snapshot ?? null, error: queryError } }));
          outcomes[account.id] = queryError;
        }
      }));
    } finally {
      vaultRefreshInFlight.current = false;
      setRefreshingVault(false);
    }
    return outcomes;
  }, []);

  const refreshCurrentUsage = useCallback(async (expectedId: string | null): Promise<UsageError | null> => {
    setState("loading");
    setError(null);
    if (expectedId) setSavedUsage((previous) => ({ ...previous, [expectedId]: {
      ...previous[expectedId], state: "loading", snapshot: previous[expectedId]?.snapshot ?? null, error: null,
    } }));
    try {
      const usage = await invoke<UsageSnapshot>("query_current_usage");
      const listing = await reconcileAccounts();
      setSavedUsage((previous) => {
        const next = { ...previous, [usage.accountId]: { state: "ready" as const, snapshot: usage, error: null } };
        if (expectedId && expectedId !== usage.accountId && next[expectedId]?.state === "loading") {
          next[expectedId] = { ...next[expectedId], state: next[expectedId].snapshot ? "ready" : "idle" };
        }
        return next;
      });
      setSnapshot(snapshotForAccount(usage, listing.currentAccountId));
      backgroundFailures.current.delete(usage.accountId);
      setState("ready");
      return null;
    } catch (reason) {
      const queryError = usageError(reason);
      setError(queryError.message);
      setState("error");
      if (expectedId) setSavedUsage((previous) => ({ ...previous, [expectedId]: {
        state: "error", snapshot: previous[expectedId]?.snapshot ?? null, error: queryError,
      } }));
      await reconcileAccounts().catch(() => undefined);
      return queryError;
    }
  }, [reconcileAccounts]);

  const refresh = useCallback(async (afterOperation = false): Promise<void> => {
    if (fullRefreshInFlight.current || vaultRefreshInFlight.current) {
      if (afterOperation) pendingOperationRefresh.current = true;
      return;
    }
    if (!afterOperation && operationBusyRef.current) return;
    fullRefreshInFlight.current = true;
    setState("loading");
    setError(null);
    const cachedUsage = cacheHydrated.current
      ? Promise.resolve<Record<string, UsageSnapshot> | null>(null)
      : invoke<Record<string, UsageSnapshot>>("load_usage_cache").catch(() => null);
    cacheHydrated.current = true;
    try {
      const listing = await reconcileAccounts();
      const accounts = listing.accounts;
      const cached = await cachedUsage;
      if (cached) {
        setSavedUsage((previous) => {
          const next = { ...previous };
          for (const account of accounts) {
            const cachedSnapshot = cached[account.id];
            const current = previous[account.id];
            if (cachedSnapshot && (!current?.snapshot || cachedSnapshot.capturedAt > current.snapshot.capturedAt)) {
              next[account.id] = { state: "ready", snapshot: cachedSnapshot, error: null, cached: true };
            }
          }
          return next;
        });
        const activeSnapshot = listing.currentAccountId ? snapshotForAccount(cached[listing.currentAccountId], listing.currentAccountId) : null;
        if (activeSnapshot) {
          setSnapshot((current) => !current || activeSnapshot.capturedAt > current.capturedAt ? activeSnapshot : current);
        }
      }
      await Promise.all([
        invoke<RuntimeInfo>("discover_runtime").then(setRuntime).catch(() => undefined),
        refreshCurrentUsage(listing.currentAccountId),
        refreshSavedAccounts(accounts.filter((account) => !account.isActive)),
      ]);
    } catch (reason) {
      setError(errorText(reason));
      setState("error");
    } finally {
      fullRefreshInFlight.current = false;
      const interval = autoRefreshMinutesRef.current;
      if (interval > 0) nextBackgroundRefreshAt.current = Date.now() + interval * 60_000;
      if (pendingOperationRefresh.current) {
        pendingOperationRefresh.current = false;
        queueMicrotask(() => void refresh(true));
      }
    }
  }, [reconcileAccounts, refreshCurrentUsage, refreshSavedAccounts]);

  const refreshAccount = useCallback(async (account: SavedAccount): Promise<UsageError | null> => {
    if (operationBusyRef.current || fullRefreshInFlight.current || vaultRefreshInFlight.current) return null;
    fullRefreshInFlight.current = true;
    try {
      const listing = await reconcileAccounts();
      const latest = listing.accounts.find((item) => item.id === account.id);
      if (!latest) return null;
      if (latest.isActive) return await refreshCurrentUsage(latest.id);
      const outcomes = await refreshSavedAccounts([latest]);
      return outcomes[latest.id] ?? null;
    } catch (reason) {
      const queryError = usageError(reason);
      setError(queryError.message);
      setState("error");
      return queryError;
    } finally {
      fullRefreshInFlight.current = false;
      if (pendingOperationRefresh.current) {
        pendingOperationRefresh.current = false;
        queueMicrotask(() => void refresh(true));
      }
    }
  }, [reconcileAccounts, refreshCurrentUsage, refreshSavedAccounts, refresh]);

  const importCurrent = useCallback(async () => {
    setImporting(true); setVaultError(null); setVaultNotice(null);
    try {
      await invoke<SavedAccount>("import_current_account", { label: null });
      setVaultNotice("当前账号已安全保存。");
      await refresh(true);
    } catch (reason) { setVaultError(errorText(reason)); }
    finally { setImporting(false); }
  }, [refresh]);

  const addAccount = useCallback(async () => {
    setAddingAccount(true); setVaultError(null); setVaultNotice(null);
    try {
      await runLogin<SavedAccount>("add_account_with_login", { label: null });
      setVaultNotice("新账号已添加并安全保存。");
      await refresh(true);
    } catch (reason) {
      if (errorText(reason) === "登录已取消") setVaultNotice("登录已取消，临时认证已清理。");
      else setVaultError(errorText(reason));
    }
    finally { setAddingAccount(false); }
  }, [refresh, runLogin]);

  const executeSwitch = useCallback(async (account: SavedAccount, restartCodex: boolean) => {
    setSwitchingAccount(account.id); setVaultError(null); setVaultNotice(null); setRestartMessage(null);
    try {
      const outcome = await invoke<SwitchOutcome>("switch_saved_account", { accountId: account.id, restartCodex });
      setVaultNotice(`账号已切换到 ${outcome.account.label}。`);
      setRestartNeeded(!outcome.restartSucceeded);
      setRestartMessage(outcome.restartSucceeded ? "已检测到 Codex 启动。" : outcome.restartWarning
        ? `账号已切换，Codex 重启未完成：${outcome.restartWarning}` : "当前登录已更新，可在方便时重启 Codex。");
      setSnapshot(null);
      await refresh(true);
    } catch (reason) { setVaultError(errorText(reason)); }
    finally { setSwitchingAccount(null); }
  }, [refresh]);

  const executeRename = useCallback(async (account: SavedAccount, label: string) => {
    setManagingAccount(account.id); setVaultError(null); setVaultNotice(null);
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
      const outcome = await runLogin<ReauthorizationOutcome>("reauthorize_account", { accountId: account.id });
      backgroundFailures.current.delete(account.id);
      setVaultNotice(outcome.currentAuthUpdated
        ? `${account.label} 的当前登录和保险库已更新。请在方便时重启 Codex，让桌面端使用新授权。`
        : `${account.label} 的保险库凭据已更新。切换到该账号后即可使用新授权。`);
      if (outcome.currentAuthUpdated) { setRestartNeeded(true); setRestartMessage(null); }
      await refresh(true);
    } catch (reason) {
      if (errorText(reason) === "登录已取消") setVaultNotice("登录已取消，原账号保持不变。");
      else setVaultError(errorText(reason));
    }
    finally { setManagingAccount(null); }
  }, [refresh, runLogin]);

  const retryRecovery = async () => {
    setRecovering(true);
    try {
      const report = await invoke<RecoveryReport>("retry_recovery");
      setRecoveryReport(report.messages.length ? report : { ...report, messages: ["恢复检查完成，没有待处理的问题。"] });
      if (report.restartSuggested) setRestartNeeded(true);
      await refresh(true);
    } catch (reason) { setRecoveryReport({ messages: [errorText(reason)], needsAttention: true, restartSuggested: false }); }
    finally { setRecovering(false); }
  };

  const executeRestart = async () => {
    setRestarting(true);
    try {
      const result = await invoke<RestartResult>("restart_codex");
      setRestartNeeded(!result.succeeded);
      setRestartMessage(result.succeeded ? "已检测到 Codex 启动，账号认证保持不变。" : result.warning ?? "Codex 未能启动，请重试。");
    } catch (reason) { setRestartNeeded(true); setRestartMessage(errorText(reason)); }
    finally { setRestarting(false); setRestartDialog(false); }
  };

  const executeDelete = useCallback(async (account: SavedAccount) => {
    setManagingAccount(account.id); setVaultError(null); setVaultNotice(null);
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
  const operationBusy = importing || addingAccount || switchingAccount !== null || managingAccount !== null || recovering || restarting;
  operationBusyRef.current = operationBusy;
  const currentSnapshot = snapshotForAccount(snapshot, currentAccountId);
  const currentName = activeAccount?.label ?? currentSnapshot?.account?.email ?? "未识别当前账号";

  const runBackgroundRefresh = useCallback(async () => {
    if (
      autoRefreshMinutesRef.current === 0
      || backgroundRefreshInFlight.current
      || operationBusyRef.current
      || fullRefreshInFlight.current
      || vaultRefreshInFlight.current
    ) return;

    backgroundRefreshInFlight.current = true;
    try {
      const listing = await reconcileAccounts();
      for (const account of [...listing.accounts].sort((left, right) => Number(right.isActive) - Number(left.isActive))) {
        if (operationBusyRef.current || fullRefreshInFlight.current || vaultRefreshInFlight.current) break;
        const backoff = backgroundFailures.current.get(account.id);
        if (backoff && backoff.nextAllowedAt > Date.now()) continue;

        const queryError = await refreshAccount(account);
        if (!queryError) {
          backgroundFailures.current.delete(account.id);
          continue;
        }
        const failures = (backoff?.failures ?? 0) + 1;
        const delay = queryError.reauthRequired
          ? Number.POSITIVE_INFINITY
          : Math.min(6 * 60 * 60_000, 10 * 60_000 * 2 ** (failures - 1));
        backgroundFailures.current.set(account.id, { failures, nextAllowedAt: Date.now() + delay });
      }
    } catch (reason) {
      setError(errorText(reason));
    } finally {
      backgroundRefreshInFlight.current = false;
    }
  }, [reconcileAccounts, refreshAccount]);

  useEffect(() => {
    let disposed = false;
    let checking = false;
    let unlisten: (() => void) | undefined;
    const busy = () => operationBusyRef.current || backgroundRefreshInFlight.current
      || fullRefreshInFlight.current || vaultRefreshInFlight.current;
    const onFocus = async () => {
      if (disposed || checking || busy()) return;
      checking = true;
      try {
        // Native activation only checks local identity. WebView focus also fires after title-bar drags.
        const listing = await invoke<AccountList>("list_saved_accounts");
        if (!disposed && !busy() && listing.currentAccountId !== currentAccountIdRef.current) await refresh();
      } catch (reason) {
        if (!disposed && !busy()) setError(errorText(reason));
      } finally { checking = false; }
    };
    void listen("tauri://focus", () => void onFocus(), { target: { kind: "Window", label: "main" } }).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });
    return () => { disposed = true; unlisten?.(); };
  }, [refresh]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen("background-refresh-clock", () => {
      if (autoRefreshMinutesRef.current === 0 || Date.now() < nextBackgroundRefreshAt.current) return;
      if (operationBusyRef.current || fullRefreshInFlight.current || vaultRefreshInFlight.current) {
        nextBackgroundRefreshAt.current = Date.now() + 60_000;
        return;
      }
      nextBackgroundRefreshAt.current = Date.now() + autoRefreshMinutesRef.current * 60_000;
      void runBackgroundRefresh();
    }).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [runBackgroundRefresh]);

  useEffect(() => {
    void invoke("update_tray_current_account", { label: currentName }).catch(() => undefined);
  }, [currentName]);

  return (
    <main className="app-shell" onClick={() => setOpenMenu(null)}>
      <header className="topbar">
        <div className="brand"><span className="brand-mark">C</span><div><h1>Codex Account Hub</h1><p>账号额度与安全切换</p></div></div>
        <div className="top-actions">
          <label className="auto-refresh-control" title="应用驻留托盘时也会按此周期自动刷新">
            <span>自动刷新</span>
            <select value={autoRefreshMinutes} onChange={(event) => setAutoRefreshMinutes(Number(event.target.value) as AutoRefreshMinutes)} disabled={operationBusy}>
              <option value={0}>关闭</option>
              <option value={10}>10 分钟</option>
              <option value={30}>30 分钟</option>
              <option value={60}>60 分钟</option>
            </select>
          </label>
          <button className="icon-button" aria-label={theme === "dark" ? "切换到浅色模式" : "切换到深色模式"} title={theme === "dark" ? "切换到浅色模式" : "切换到深色模式"} onClick={() => setTheme((value) => value === "dark" ? "light" : "dark")}>
            {theme === "dark" ? "☀" : "☾"}
          </button>
          <button className="button secondary" onClick={() => void refresh()} disabled={state === "loading" || refreshingVault || operationBusy}>
            <span className={state === "loading" || refreshingVault ? "spin" : ""}>↻</span>{state === "loading" || refreshingVault ? "正在刷新" : "刷新全部"}
          </button>
        </div>
      </header>

      <section className="summary-bar">
        <div className={`summary-current ${currentAccountId ? "" : "unknown"}`}><span className="status-dot" /><span>当前</span><strong>{currentName}</strong><span className="plan-label">{currentSnapshot?.account?.planType?.toUpperCase() ?? activeAccount?.planType?.toUpperCase() ?? "UNKNOWN"}</span></div>
        <div className="summary-meta"><span>{savedAccounts.length} 个账号</span><span className="divider" /><span>{runtime?.codexVersion ?? "Codex runtime"}</span></div>
      </section>

      {loginProgress && <section className="notice-bar login-status" role="status">
        <span>↗</span><p>{{ starting: "正在准备登录…", queued: "正在等待当前操作结束…", waiting: "请在官方页面完成登录。", verifying: "正在验证登录账号…", saving: "正在安全保存账号，请稍候…", cancelling: "正在取消登录并清理临时认证…" }[loginProgress.phase]}</p>
        <button onClick={() => void cancelLogin()} disabled={["starting", "saving", "cancelling"].includes(loginProgress.phase)}>取消登录</button>
      </section>}

      {recoveryReport && recoveryReport.messages.length > 0 && <section className={`notice-bar recovery-status ${recoveryReport.needsAttention ? "error" : ""}`} role="status">
        <span>{recoveryReport.needsAttention ? "!" : "✓"}</span><p>{recoveryReport.messages.join(" ")}</p>
        <button disabled={operationBusy} onClick={() => void retryRecovery()}>{recovering ? "正在恢复…" : "重试恢复"}</button>
        {!recoveryReport.needsAttention && <button onClick={() => setRecoveryReport(null)}>关闭提示</button>}
      </section>}

      {(restartNeeded || restartMessage) && <section className="notice-bar restart-status" role="status">
        <span>↻</span><p>{restartMessage ?? "当前登录已更新，请在方便时重启 Codex。"}</p>
        {restartNeeded && <button onClick={() => setRestartDialog(true)} disabled={operationBusy}>重新启动 Codex</button>}
      </section>}

      {(vaultNotice || vaultError || error) && (
        <section className={`notice-bar ${vaultError || error ? "error" : ""}`}>
          <span>{vaultError || error ? "!" : addingAccount ? "↗" : "✓"}</span>
          <p>{vaultError ?? error ?? (addingAccount ? "请在刚打开的官方页面完成登录，当前 Codex 账号不会被切换。" : vaultNotice)}</p>
          {(vaultError || error) && <button disabled={operationBusy} onClick={() => void refresh()}>重试</button>}
          {vaultError && <button disabled={operationBusy} onClick={() => void retryRecovery()}>检查并恢复</button>}
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
              const usage: SavedUsageState = savedUsage[account.id] ?? { state: account.isActive && state === "loading" ? "loading" : "idle", snapshot: null, error: null };
              const stale = isSnapshotStale(usage.snapshot, now);
              return (
                <article className={`account-row ${account.isActive ? "active" : ""}`} key={account.id}>
                  <div className="account-row-heading">
                    <div className="account-identity"><div className="avatar">{account.label.trim().charAt(0).toUpperCase() || "C"}</div><div><div className="account-title"><h3>{account.label}</h3>{account.isActive && <span className="active-badge">当前账号</span>}</div><div className="account-subtitle"><span>{usage.snapshot?.account?.planType?.toUpperCase() ?? account.planType?.toUpperCase() ?? "UNKNOWN PLAN"}</span><span className={`sync-state ${stale ? "stale" : usage.state}`}>{usage.state === "loading" ? "刷新中" : usage.state === "error" ? usage.snapshot ? "缓存数据" : usage.error ? errorLabel(usage.error) : "读取失败" : usage.cached ? "缓存数据" : usage.state === "ready" ? "已同步" : "等待刷新"}{stale ? " · 数据陈旧" : ""}</span><span>{formatCapturedAt(usage.snapshot?.capturedAt ?? null)}</span></div></div></div>
                    <div className="account-actions">
                      {!account.isActive && <button className="button switch-primary" onClick={() => setDialog({ type: "switch", account, restartCodex: true })} disabled={operationBusy}>{switchingAccount === account.id ? "正在切换…" : "切换并重启"}</button>}
                      <div className="menu-wrap" onClick={(event) => event.stopPropagation()}>
                        <button className="icon-button menu-trigger" aria-label={`${account.label} 更多操作`} aria-expanded={openMenu === account.id} onClick={() => setOpenMenu((value) => value === account.id ? null : account.id)} disabled={operationBusy}>•••</button>
                        {openMenu === account.id && <div className="account-menu">
                          <button onClick={() => { setOpenMenu(null); void refreshAccount(account); }}>↻<span>刷新额度</span></button>
                          {!account.isActive && <button onClick={() => { setOpenMenu(null); setDialog({ type: "switch", account, restartCodex: false }); }}>⇄<span>仅切换账号</span></button>}
                          <button onClick={() => { setOpenMenu(null); setRenameValue(account.label); setDialog({ type: "rename", account }); }}>✎<span>重命名</span></button>
                          <button onClick={() => void reauthorizeAccount(account)}>↗<span>重新授权</span></button>
                          {!account.isActive && <button className="danger" onClick={() => { setOpenMenu(null); setDialog({ type: "delete", account }); }}>×<span>删除账号</span></button>}
                        </div>}
                      </div>
                    </div>
                  </div>
                  {usage.error && <div className="account-error"><span title={usage.error.message}>{usage.snapshot ? `${errorLabel(usage.error)}，正在显示上次成功数据` : errorLabel(usage.error)}：{usage.error.message}</span><button onClick={() => usage.error?.reauthRequired ? void reauthorizeAccount(account) : void refreshAccount(account)} disabled={state === "loading" || refreshingVault || operationBusy}>{usage.error.reauthRequired ? "重新授权" : "重试"}</button></div>}
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
      {restartDialog && <div className="dialog-backdrop"><section className="dialog-panel" role="dialog" aria-modal="true" aria-labelledby="restart-title">
        <h2 id="restart-title">重新启动 Codex？</h2><p>将关闭 Codex 窗口并中断正在执行的任务。当前账号认证不会再次切换。</p>
        <div className="dialog-actions"><button className="button ghost" disabled={restarting} onClick={() => setRestartDialog(false)}>取消</button><button className="button primary" disabled={restarting} onClick={() => void executeRestart()}>{restarting ? "正在重启…" : "确认重启"}</button></div>
      </section></div>}
    </main>
  );
}
