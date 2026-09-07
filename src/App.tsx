import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { LimitWindow, RuntimeInfo, SavedAccount, SwitchOutcome, UsageSnapshot } from "./types";

type LoadState = "idle" | "loading" | "ready" | "error";
type SavedUsageState = {
  state: LoadState;
  snapshot: UsageSnapshot | null;
  error: string | null;
};

function formatResetTime(timestamp: number | null) {
  if (!timestamp) return "暂无重置时间";
  return new Intl.DateTimeFormat("zh-CN", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date(timestamp * 1_000));
}

function formatWindow(minutes: number | null) {
  if (!minutes) return "额度窗口";
  if (minutes % 10_080 === 0) return `${minutes / 10_080} 周窗口`;
  if (minutes % 1_440 === 0) return `${minutes / 1_440} 天窗口`;
  if (minutes % 60 === 0) return `${minutes / 60} 小时窗口`;
  return `${minutes} 分钟窗口`;
}

function UsageMeter({ window, tone }: { window: LimitWindow | null; tone: "cyan" | "violet" }) {
  const remaining = window?.remainingPercent ?? null;
  const used = window?.usedPercent ?? 0;
  return (
    <section className="meter-block">
      <div className="meter-heading">
        <div>
          <span className="meter-label">{formatWindow(window?.windowDurationMins ?? null)}</span>
          <strong>{remaining === null ? "—" : `${Math.round(remaining)}%`}</strong>
        </div>
        <span className="reset-time">{formatResetTime(window?.resetsAt ?? null)} 重置</span>
      </div>
      <div className="meter-track" aria-label={`已使用 ${used}%`}>
        <div className={`meter-fill ${tone}`} style={{ width: `${Math.min(100, Math.max(0, used))}%` }} />
      </div>
      <div className="meter-foot">
        <span>已使用 {window?.usedPercent ?? "—"}%</span>
        <span>剩余 {remaining ?? "—"}%</span>
      </div>
    </section>
  );
}

export default function App() {
  const [runtime, setRuntime] = useState<RuntimeInfo | null>(null);
  const [snapshot, setSnapshot] = useState<UsageSnapshot | null>(null);
  const [state, setState] = useState<LoadState>("idle");
  const [error, setError] = useState<string | null>(null);
  const [savedAccounts, setSavedAccounts] = useState<SavedAccount[]>([]);
  const [savedUsage, setSavedUsage] = useState<Record<string, SavedUsageState>>({});
  const [refreshingVault, setRefreshingVault] = useState(false);
  const vaultRefreshInFlight = useRef(false);
  const [importing, setImporting] = useState(false);
  const [addingAccount, setAddingAccount] = useState(false);
  const [switchingAccount, setSwitchingAccount] = useState<string | null>(null);
  const [vaultError, setVaultError] = useState<string | null>(null);
  const [vaultNotice, setVaultNotice] = useState<string | null>(null);

  const refreshSavedAccounts = useCallback(async (accounts: SavedAccount[]) => {
    if (!accounts.length || vaultRefreshInFlight.current) return;
    vaultRefreshInFlight.current = true;
    setRefreshingVault(true);
    setSavedUsage((previous) => {
      const next = { ...previous };
      for (const account of accounts) {
        next[account.id] = {
          state: "loading",
          snapshot: previous[account.id]?.snapshot ?? null,
          error: null,
        };
      }
      return next;
    });

    try {
      await Promise.all(
        accounts.map(async (account) => {
          try {
            const usage = await invoke<UsageSnapshot>("query_saved_usage", { accountId: account.id });
            setSavedUsage((previous) => ({
              ...previous,
              [account.id]: { state: "ready", snapshot: usage, error: null },
            }));
          } catch (reason) {
            setSavedUsage((previous) => ({
              ...previous,
              [account.id]: {
                state: "error",
                snapshot: previous[account.id]?.snapshot ?? null,
                error: reason instanceof Error ? reason.message : String(reason),
              },
            }));
          }
        }),
      );
    } finally {
      vaultRefreshInFlight.current = false;
      setRefreshingVault(false);
    }
  }, []);

  const refresh = useCallback(async () => {
    setState("loading");
    setError(null);
    try {
      const [runtimeResult, usageResult, savedResult] = await Promise.all([
        invoke<RuntimeInfo>("discover_runtime"),
        invoke<UsageSnapshot>("query_current_usage"),
        invoke<SavedAccount[]>("list_saved_accounts"),
      ]);
      setRuntime(runtimeResult);
      setSnapshot(usageResult);
      setSavedAccounts(savedResult);
      setState("ready");
      void refreshSavedAccounts(savedResult);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setState("error");
    }
  }, [refreshSavedAccounts]);

  const importCurrent = useCallback(async () => {
    setImporting(true);
    setVaultError(null);
    setVaultNotice(null);
    try {
      const saved = await invoke<SavedAccount>("import_current_account", { label: null });
      setSavedAccounts((accounts) => {
        const others = accounts.filter((account) => account.id !== saved.id);
        return [saved, ...others];
      });
      await refreshSavedAccounts([saved]);
    } catch (reason) {
      setVaultError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setImporting(false);
    }
  }, [refreshSavedAccounts]);

  const addAccount = useCallback(async () => {
    setAddingAccount(true);
    setVaultError(null);
    setVaultNotice(null);
    try {
      const saved = await invoke<SavedAccount>("add_account_with_login", { label: null });
      setSavedAccounts((accounts) => {
        const others = accounts.filter((account) => account.id !== saved.id);
        return [saved, ...others];
      });
      await refreshSavedAccounts([saved]);
    } catch (reason) {
      setVaultError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setAddingAccount(false);
    }
  }, [refreshSavedAccounts]);

  const switchAccount = useCallback(
    async (account: SavedAccount, restartCodex: boolean) => {
      const confirmed = window.confirm(
        restartCodex
          ? `切换到 ${account.label} 并重启 Codex？\n\n应用会先加密保存当前账号，再替换本机登录。所有正在运行的 Codex 任务和窗口都会被关闭。`
          : `仅切换到 ${account.label}？\n\n应用会替换本机 Codex 登录，但不会关闭当前 Codex。你需要稍后手动重启 Codex 才能确保生效。`,
      );
      if (!confirmed) return;
      setSwitchingAccount(account.id);
      setVaultError(null);
      setVaultNotice(null);
      try {
        const outcome = await invoke<SwitchOutcome>("switch_saved_account", {
          accountId: account.id,
          restartCodex,
        });
        if (outcome.restartWarning) {
          setVaultError(outcome.restartWarning);
        } else if (outcome.restartSucceeded) {
          setVaultNotice(`已切换到 ${outcome.account.label}，Codex 已重新启动。`);
        } else {
          setVaultNotice(`已切换到 ${outcome.account.label}，请稍后手动重启 Codex。`);
        }
        await refresh();
      } catch (reason) {
        setVaultError(reason instanceof Error ? reason.message : String(reason));
      } finally {
        setSwitchingAccount(null);
      }
    },
    [refresh],
  );

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const capturedAt = useMemo(() => {
    if (!snapshot) return "尚未刷新";
    return new Intl.DateTimeFormat("zh-CN", {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    }).format(new Date(snapshot.capturedAt * 1_000));
  }, [snapshot]);

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand">
          <span className="brand-mark">C</span>
          <div>
            <h1>Codex Account Hub</h1>
            <p>本地账号额度与切换中心</p>
          </div>
        </div>
        <button className="refresh-button" onClick={() => void refresh()} disabled={state === "loading"}>
          <span className={state === "loading" ? "spin" : ""}>↻</span>
          {state === "loading" ? "正在刷新" : "刷新当前账号"}
        </button>
      </header>

      <section className="hero">
        <div>
          <span className="eyebrow">CURRENT ACCOUNT</span>
          <h2>所有额度，一眼看清。</h2>
          <p>数据直接来自本机 Codex App Server，凭据不会离开官方 Codex 进程。</p>
        </div>
        <div className="runtime-pill">
          <span className="status-dot" />
          {runtime?.codexVersion ?? "Codex runtime"}
        </div>
      </section>

      {error ? (
        <section className="error-card">
          <strong>暂时无法读取额度</strong>
          <p>{error}</p>
          <button onClick={() => void refresh()}>重试</button>
        </section>
      ) : (
        <section className="account-card">
          <div className="account-header">
            <div className="avatar">{snapshot?.account?.email?.[0]?.toUpperCase() ?? "?"}</div>
            <div className="identity">
              <div className="identity-row">
                <h3>{snapshot?.account?.email ?? (state === "loading" ? "正在读取账号…" : "未检测到账号")}</h3>
                <span className="active-badge">当前使用</span>
              </div>
              <p>{snapshot?.account?.planType?.toUpperCase() ?? "UNKNOWN PLAN"}</p>
            </div>
            <div className="health">
              <span className="status-dot" />
              登录有效
            </div>
          </div>

          <div className="usage-grid">
            <UsageMeter window={snapshot?.primary ?? null} tone="cyan" />
            <UsageMeter window={snapshot?.secondary ?? null} tone="violet" />
          </div>

          <footer className="account-footer">
            <span>最后刷新：{capturedAt}</span>
            <span>可用额度重置券：{snapshot?.resetCreditsAvailable ?? "—"}</span>
            <span className="secure-label">本地安全读取</span>
          </footer>
        </section>
      )}

      <section className="coming-next">
        <div>
          <span className="eyebrow">ENCRYPTED VAULT</span>
          <h3>已保存账号 · {savedAccounts.length}</h3>
          <p>
            {savedAccounts.length
              ? savedAccounts.map((account) => account.label).join(" · ")
              : "将当前登录加密保存后，才能继续录入和管理其他账号。"}
          </p>
          {addingAccount && <p className="login-hint">请在刚打开的官方页面完成登录，当前 Codex 账号不会被切换。</p>}
          {vaultNotice && <p className="vault-notice">{vaultNotice}</p>}
          {vaultError && <p className="vault-error">{vaultError}</p>}
        </div>
        <div className="vault-actions">
          <button
            className="vault-button"
            onClick={() => void importCurrent()}
            disabled={importing || addingAccount || !snapshot?.account}
          >
            {importing ? "正在加密保存…" : "导入当前账号"}
          </button>
          <button
            className="vault-button primary"
            onClick={() => void addAccount()}
            disabled={addingAccount || importing}
          >
            {addingAccount ? "等待官方登录…" : "添加另一个账号"}
          </button>
        </div>
      </section>

      {savedAccounts.length > 0 && (
        <section className="vault-panel">
          <div className="vault-panel-heading">
            <div>
              <span className="eyebrow">ISOLATED USAGE</span>
              <h3>保险库账号额度</h3>
            </div>
            <button
              className="vault-button"
              onClick={() => void refreshSavedAccounts(savedAccounts)}
              disabled={refreshingVault}
            >
              {refreshingVault ? "正在刷新…" : "刷新全部账号"}
            </button>
          </div>
          <div className="saved-account-list">
            {savedAccounts.map((account) => {
              const usage = savedUsage[account.id];
              return (
                <article className="saved-account-card" key={account.id}>
                  <div className="saved-account-identity">
                    <div className="saved-account-name">
                      <strong>{account.label}</strong>
                      <span>{account.planType?.toUpperCase() ?? "UNKNOWN PLAN"}</span>
                    </div>
                    <div className="saved-card-actions">
                      <span className={`saved-status ${usage?.state ?? "idle"}`}>
                        {usage?.state === "loading"
                          ? "刷新中"
                          : usage?.state === "error"
                            ? "读取失败"
                            : usage?.state === "ready"
                              ? "已同步"
                              : "等待刷新"}
                      </span>
                      <button
                        className="switch-button"
                        onClick={() => void switchAccount(account, true)}
                        disabled={switchingAccount !== null || addingAccount || importing}
                      >
                        {switchingAccount === account.id ? "正在切换…" : "切换并重启"}
                      </button>
                      <button
                        className="switch-button subtle"
                        onClick={() => void switchAccount(account, false)}
                        disabled={switchingAccount !== null || addingAccount || importing}
                      >
                        仅切换
                      </button>
                    </div>
                  </div>
                  {usage?.error ? (
                    <p className="saved-error">{usage.error}</p>
                  ) : (
                    <div className="saved-limit-grid">
                      <div>
                        <span>{formatWindow(usage?.snapshot?.primary?.windowDurationMins ?? null)}</span>
                        <strong>{usage?.snapshot?.primary?.remainingPercent ?? "—"}%</strong>
                        <small>{formatResetTime(usage?.snapshot?.primary?.resetsAt ?? null)} 重置</small>
                      </div>
                      <div>
                        <span>{formatWindow(usage?.snapshot?.secondary?.windowDurationMins ?? null)}</span>
                        <strong>{usage?.snapshot?.secondary?.remainingPercent ?? "—"}%</strong>
                        <small>{formatResetTime(usage?.snapshot?.secondary?.resetsAt ?? null)} 重置</small>
                      </div>
                    </div>
                  )}
                </article>
              );
            })}
          </div>
        </section>
      )}

      <footer className="app-footer">
        <span>{runtime?.codexHome ?? "正在发现 Codex Home…"}</span>
        <span>不会显示或记录 OAuth token</span>
      </footer>
    </main>
  );
}
