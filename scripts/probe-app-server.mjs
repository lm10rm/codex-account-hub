#!/usr/bin/env node

import path from "node:path";
import process from "node:process";
import {
  AppServerError,
  CodexAppServerClient,
  sanitizeAccountResult,
  sanitizeRateLimitsResult,
} from "../src/core/app-server-client.mjs";
import { discoverCodexExecutable } from "../src/core/codex-discovery.mjs";

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 1) {
    const token = argv[index];
    if (token === "--codex") args.codexPath = argv[++index];
    else if (token === "--codex-home") args.codexHome = argv[++index];
    else if (token === "--timeout") args.timeoutMs = Number(argv[++index]);
    else if (token === "--help" || token === "-h") args.help = true;
    else throw new Error(`未知参数：${token}`);
  }
  return args;
}

function printHelp() {
  console.log(`Usage:
  npm run probe -- --codex-home <path> [--codex <path>] [--timeout <ms>]

This command only reads account metadata and rate limits through codex app-server.
It never prints authentication tokens.`);
}

function safeError(error) {
  if (error instanceof AppServerError) {
    return {
      name: error.name,
      message: error.message,
      details: {
        code: error.details?.code ?? null,
        method: error.details?.method ?? null,
        exitCode: error.details?.code ?? null,
        diagnostic: sanitizeDiagnostic(error.details?.stderr),
      },
    };
  }
  return { name: error?.name ?? "Error", message: error?.message ?? String(error) };
}

function sanitizeDiagnostic(value) {
  if (typeof value !== "string" || !value.trim()) return null;
  return value
    .replace(/\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b/gu, "[REDACTED_JWT]")
    .replace(/\bsk-[A-Za-z0-9_-]+\b/gu, "[REDACTED_KEY]")
    .replace(/(access_token|refresh_token|id_token)(["'\s:=]+)[^\s,"'}]+/giu, "$1$2[REDACTED]")
    .trim()
    .slice(-2_000);
}

let client;
try {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    printHelp();
    process.exit(0);
  }
  if (!args.codexHome) {
    throw new Error("缺少 --codex-home。为避免误读其他账号，必须显式指定 Codex Home。");
  }

  const codexHome = path.resolve(args.codexHome);
  const codexPath = await discoverCodexExecutable({ explicitPath: args.codexPath });
  client = new CodexAppServerClient({
    codexPath,
    codexHome,
    timeoutMs: Number.isFinite(args.timeoutMs) ? args.timeoutMs : undefined,
  });

  const initializeResult = await client.start();
  const accountResult = await client.request("account/read", { refreshToken: false });
  const rateLimitsResult = await client.request("account/rateLimits/read", {});

  console.log(
    JSON.stringify(
      {
        ok: true,
        runtime: {
          codexPath,
          codexHome,
          serverVersion: initializeResult?.serverInfo?.version ?? null,
        },
        account: sanitizeAccountResult(accountResult),
        usage: sanitizeRateLimitsResult(rateLimitsResult),
        capturedAt: new Date().toISOString(),
      },
      null,
      2,
    ),
  );
} catch (error) {
  console.error(JSON.stringify({ ok: false, error: safeError(error) }, null, 2));
  process.exitCode = 1;
} finally {
  await client?.stop();
}
