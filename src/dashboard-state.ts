import type { UsageSnapshot } from "./types";

export type AutoRefreshMinutes = 0 | 10 | 30 | 60;

export function parseAutoRefresh(saved: string | null): AutoRefreshMinutes {
  if (saved === null || saved.trim() === "") return 10;
  const minutes = Number(saved);
  return minutes === 0 || minutes === 10 || minutes === 30 || minutes === 60 ? minutes : 10;
}

export function snapshotForAccount(snapshot: UsageSnapshot | null, accountId: string | null) {
  return snapshot && accountId && snapshot.accountId === accountId ? snapshot : null;
}

export function isSnapshotStale(snapshot: UsageSnapshot | null, now: number) {
  return !!snapshot && now - snapshot.capturedAt * 1_000 >= 30 * 60_000;
}
