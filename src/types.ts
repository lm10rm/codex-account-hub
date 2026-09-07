export interface AccountInfo {
  accountType: string | null;
  email: string | null;
  planType: string | null;
}

export interface LimitWindow {
  usedPercent: number | null;
  remainingPercent: number | null;
  windowDurationMins: number | null;
  resetsAt: number | null;
}

export interface UsageSnapshot {
  account: AccountInfo | null;
  primary: LimitWindow | null;
  secondary: LimitWindow | null;
  rateLimitReachedType: string | null;
  resetCreditsAvailable: number | null;
  capturedAt: number;
}

export interface RuntimeInfo {
  codexPath: string;
  codexHome: string;
  codexVersion: string | null;
}

export interface SavedAccount {
  id: string;
  label: string;
  email: string | null;
  planType: string | null;
  importedAt: number;
}
