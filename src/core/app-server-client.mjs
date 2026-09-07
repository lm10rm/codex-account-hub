import { spawn } from "node:child_process";
import readline from "node:readline";

const DEFAULT_TIMEOUT_MS = 15_000;

export class AppServerError extends Error {
  constructor(message, details = {}) {
    super(message);
    this.name = "AppServerError";
    this.details = details;
  }
}

export class CodexAppServerClient {
  #child;
  #nextId = 1;
  #pending = new Map();
  #notifications = new Map();
  #stderr = "";
  #closed = false;

  constructor({ codexPath, codexHome, timeoutMs = DEFAULT_TIMEOUT_MS }) {
    if (!codexPath) throw new TypeError("codexPath is required");
    if (!codexHome) throw new TypeError("codexHome is required");

    this.codexPath = codexPath;
    this.codexHome = codexHome;
    this.timeoutMs = timeoutMs;
  }

  async start() {
    if (this.#child) throw new AppServerError("App Server 已经启动");

    this.#child = spawn(this.codexPath, ["app-server", "--stdio"], {
      env: {
        ...process.env,
        CODEX_HOME: this.codexHome,
      },
      windowsHide: true,
      stdio: ["pipe", "pipe", "pipe"],
    });

    this.#child.stdin.setDefaultEncoding("utf8");
    this.#child.stderr.setEncoding("utf8");
    this.#child.stderr.on("data", (chunk) => {
      this.#stderr = `${this.#stderr}${chunk}`.slice(-8_000);
    });

    const lines = readline.createInterface({ input: this.#child.stdout });
    lines.on("line", (line) => this.#handleLine(line));
    this.#child.on("error", (error) => this.#failAll(error));
    this.#child.on("close", (code, signal) => {
      this.#closed = true;
      this.#failAll(
        new AppServerError("App Server 已退出", {
          code,
          signal,
          stderr: this.#stderr,
        }),
      );
    });

    const initialized = await this.request("initialize", {
      clientInfo: {
        name: "codex_account_hub",
        title: "Codex Account Hub",
        version: "0.1.0",
      },
      capabilities: {},
    });
    this.notify("initialized", {});
    return initialized;
  }

  request(method, params = {}, { timeoutMs = this.timeoutMs } = {}) {
    if (!this.#child || this.#closed) {
      return Promise.reject(new AppServerError("App Server 尚未启动或已经关闭"));
    }

    const id = this.#nextId++;
    const payload = { method, id, params };

    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new AppServerError(`请求超时：${method}`, { method, timeoutMs }));
      }, timeoutMs);

      this.#pending.set(id, { resolve, reject, timer, method });
      this.#write(payload);
    });
  }

  notify(method, params = {}) {
    if (!this.#child || this.#closed) {
      throw new AppServerError("App Server 尚未启动或已经关闭");
    }
    this.#write({ method, params });
  }

  onNotification(method, listener) {
    const listeners = this.#notifications.get(method) ?? new Set();
    listeners.add(listener);
    this.#notifications.set(method, listeners);
    return () => listeners.delete(listener);
  }

  async stop() {
    if (!this.#child || this.#closed) return;
    this.#child.stdin.end();

    await new Promise((resolve) => {
      const timer = setTimeout(() => {
        this.#child.kill();
        resolve();
      }, 1_500);
      this.#child.once("close", () => {
        clearTimeout(timer);
        resolve();
      });
    });
  }

  #write(payload) {
    this.#child.stdin.write(`${JSON.stringify(payload)}\n`);
  }

  #handleLine(line) {
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      return;
    }

    if (Object.hasOwn(message, "id")) {
      const pending = this.#pending.get(message.id);
      if (!pending) return;

      clearTimeout(pending.timer);
      this.#pending.delete(message.id);
      if (message.error) {
        pending.reject(
          new AppServerError(`请求失败：${pending.method}`, {
            method: pending.method,
            code: message.error.code,
            message: message.error.message,
          }),
        );
      } else {
        pending.resolve(message.result);
      }
      return;
    }

    if (message.method) {
      for (const listener of this.#notifications.get(message.method) ?? []) {
        listener(message.params);
      }
    }
  }

  #failAll(error) {
    for (const pending of this.#pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.#pending.clear();
  }
}

export function sanitizeAccountResult(result) {
  const account = result?.account;
  if (!account) {
    return {
      account: null,
      requiresOpenaiAuth: Boolean(result?.requiresOpenaiAuth),
    };
  }

  return {
    account: {
      type: account.type ?? null,
      email: redactEmail(account.email),
      planType: account.planType ?? null,
    },
    requiresOpenaiAuth: Boolean(result?.requiresOpenaiAuth),
  };
}

export function sanitizeRateLimitsResult(result) {
  return {
    ordinaryUsageAllowed: result?.ordinaryUsageAllowed ?? null,
    rateLimits: sanitizeRateLimits(result?.rateLimits),
    additionalRateLimits: Array.isArray(result?.additionalRateLimits)
      ? result.additionalRateLimits.map(sanitizeAdditionalRateLimit)
      : [],
    credits: sanitizeCredits(result?.credits),
    rateLimitResetCredits: result?.rateLimitResetCredits
      ? {
          availableCount: result.rateLimitResetCredits.availableCount ?? null,
        }
      : null,
  };
}

function sanitizeRateLimits(value) {
  if (!value) return null;
  return {
    primary: sanitizeWindow(value.primary),
    secondary: sanitizeWindow(value.secondary),
    rateLimitReachedType: value.rateLimitReachedType ?? null,
  };
}

function sanitizeWindow(value) {
  if (!value) return null;
  const usedPercent = numberOrNull(value.usedPercent);
  return {
    usedPercent,
    remainingPercent: usedPercent === null ? null : Math.max(0, 100 - usedPercent),
    windowDurationMins: numberOrNull(value.windowDurationMins),
    resetsAt: numberOrNull(value.resetsAt),
    resetsAtIso: unixSecondsToIso(value.resetsAt),
  };
}

function sanitizeAdditionalRateLimit(value) {
  return {
    limitName: value?.limitName ?? value?.limit_name ?? null,
    modelSlug: value?.modelSlug ?? value?.model_slug ?? null,
    normalModelSlug: value?.normalModelSlug ?? value?.normal_model_slug ?? null,
    rateLimits: sanitizeRateLimits(value?.rateLimits ?? value?.rate_limits),
  };
}

function sanitizeCredits(value) {
  if (!value) return null;
  return {
    hasCredits: value.hasCredits ?? null,
    unlimited: value.unlimited ?? null,
    balance: numberOrNull(value.balance),
  };
}

function redactEmail(email) {
  if (typeof email !== "string") return null;
  const at = email.indexOf("@");
  if (at <= 1) return `***${email.slice(Math.max(0, at))}`;
  return `${email[0]}***${email.slice(at)}`;
}

function numberOrNull(value) {
  return Number.isFinite(value) ? value : null;
}

function unixSecondsToIso(value) {
  if (!Number.isFinite(value)) return null;
  return new Date(value * 1_000).toISOString();
}

