import { access } from "node:fs/promises";
import path from "node:path";
import { spawn } from "node:child_process";

const WINDOWS_CANDIDATES = [
  ["LOCALAPPDATA", "OpenAI", "Codex"],
  ["USERPROFILE", ".local", "bin", "codex.exe"],
];

async function isReadable(filePath) {
  try {
    await access(filePath);
    return true;
  } catch {
    return false;
  }
}

function runLocator(command, args) {
  return new Promise((resolve) => {
    const child = spawn(command, args, {
      windowsHide: true,
      stdio: ["ignore", "pipe", "ignore"],
    });
    let stdout = "";
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.on("error", () => resolve([]));
    child.on("close", (code) => {
      if (code !== 0) {
        resolve([]);
        return;
      }
      resolve(
        stdout
          .split(/\r?\n/u)
          .map((line) => line.trim())
          .filter(Boolean),
      );
    });
  });
}

async function findBundledWindowsCodex(env) {
  const root = env.LOCALAPPDATA;
  if (!root) return null;

  const base = path.join(root, "OpenAI", "Codex", "bin");
  const { readdir } = await import("node:fs/promises");
  try {
    const versions = await readdir(base, { withFileTypes: true });
    const candidates = versions
      .filter((entry) => entry.isDirectory())
      .map((entry) => path.join(base, entry.name, "codex.exe"));
    for (const candidate of candidates.reverse()) {
      if (await isReadable(candidate)) return candidate;
    }
  } catch {
    return null;
  }
  return null;
}

export async function discoverCodexExecutable({ explicitPath, env = process.env } = {}) {
  if (explicitPath) {
    const resolved = path.resolve(explicitPath);
    if (!(await isReadable(resolved))) {
      throw new Error(`指定的 Codex 不存在或不可读取：${resolved}`);
    }
    return resolved;
  }

  const locator = process.platform === "win32" ? "where.exe" : "which";
  const located = await runLocator(locator, ["codex"]);
  for (const candidate of located) {
    if (await isReadable(candidate)) return candidate;
  }

  if (process.platform === "win32") {
    const bundled = await findBundledWindowsCodex(env);
    if (bundled) return bundled;

    for (const [envName, ...segments] of WINDOWS_CANDIDATES) {
      if (!env[envName]) continue;
      const candidate = path.join(env[envName], ...segments);
      if (await isReadable(candidate)) return candidate;
    }
  }

  throw new Error("未找到 codex 可执行文件，请使用 --codex 显式指定路径。");
}

