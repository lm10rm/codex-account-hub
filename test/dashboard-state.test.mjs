import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

// Transpile the production helpers so the suite also runs on Node 22 without TS flags.
const source = await readFile(new URL("../src/dashboard-state.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } });
const { parseAutoRefresh, snapshotForAccount, isSnapshotStale } = await import(
  `data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`
);

test("first launch defaults to 10 minutes while explicitly disabled stays disabled", () => {
  assert.equal(parseAutoRefresh(null), 10);
  assert.equal(parseAutoRefresh(""), 10);
  assert.equal(parseAutoRefresh("invalid"), 10);
  assert.equal(parseAutoRefresh("0"), 0);
  assert.equal(parseAutoRefresh("30"), 30);
  assert.equal(parseAutoRefresh("60"), 60);
});

test("external account switch or logout cannot reuse the previous account snapshot", () => {
  const old = { accountId: "account-a", capturedAt: 100 };
  assert.equal(snapshotForAccount(old, "account-b"), null);
  assert.equal(snapshotForAccount(old, null), null);
  assert.equal(snapshotForAccount(old, "account-a"), old);
  assert.equal(snapshotForAccount(null, "account-a"), null);
});

test("cached usage becomes stale after 30 minutes, without changing quota values", () => {
  const snapshot = { accountId: "account-a", capturedAt: 100, primary: { remainingPercent: 70 } };
  assert.equal(isSnapshotStale(snapshot, 100_000 + 29 * 60_000), false);
  assert.equal(isSnapshotStale(snapshot, 100_000 + 30 * 60_000), true);
  assert.equal(isSnapshotStale(null, Date.now()), false);
  assert.equal(snapshot.primary.remainingPercent, 70);
});
