import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./App.css";

type Provider = "github" | "anthropic" | "openai";
type Freshness = "fresh" | "stale" | "error" | "unsupported";

export interface AllowanceMetric {
  kind: "currency" | "tokens" | "credits" | "requests";
  label: string;
  unit: string;
  consumed: number;
  limit?: number;
  remaining?: number;
}

export interface ProviderSnapshot {
  accountId: string;
  provider: Provider;
  label: string;
  scope: string;
  freshness: Freshness;
  updatedAt: string;
  resetAt?: string;
  metrics: AllowanceMetric[];
  message?: string;
}

interface Dashboard {
  generatedAt: string;
  providers: ProviderSnapshot[];
}

interface AccountInput {
  provider: Provider;
  label: string;
  scopeType: "personal" | "organization";
  scope: string;
  secret: string;
}

interface LocalAccountCandidate {
  id: string;
  provider: Provider;
  label: string;
  account: string;
  source: string;
  canConnect: boolean;
  connected: boolean;
  message: string;
}

const emptyDashboard: Dashboard = {
  generatedAt: new Date().toISOString(),
  providers: [],
};

const demoDashboard: Dashboard = {
  generatedAt: new Date().toISOString(),
  providers: [
    {
      accountId: "demo-github",
      provider: "github",
      label: "GitHub Copilot",
      scope: "Personal account",
      freshness: "fresh",
      updatedAt: new Date().toISOString(),
      resetAt: new Date(Date.now() + 12 * 86_400_000).toISOString(),
      metrics: [
        { kind: "credits", label: "Copilot allowance (AIC)", unit: "AIC", consumed: 612, limit: 1000, remaining: 388 },
        { kind: "currency", label: "Used quota value (USD equivalent)", unit: "USD", consumed: 6.12 },
        { kind: "credits", label: "GitHub-reported credits_used (AIC)", unit: "AIC", consumed: 610 },
      ],
      message: "Copilot AIC and its USD equivalent are quota values, not billed spend. Power BI spend is not currently imported.",
    },
    {
      accountId: "demo-anthropic",
      provider: "anthropic",
      label: "Anthropic API",
      scope: "Organization",
      freshness: "fresh",
      updatedAt: new Date().toISOString(),
      metrics: [
        { kind: "currency", label: "Current period", unit: "USD", consumed: 18.72, limit: 50, remaining: 31.28 },
        { kind: "tokens", label: "Tokens", unit: "tokens", consumed: 4_820_441 },
      ],
      message: "The $50 limit is a local alert threshold, not an Anthropic account balance.",
    },
    {
      accountId: "demo-openai",
      provider: "openai",
      label: "Codex personal plan",
      scope: "Personal subscription",
      freshness: "unsupported",
      updatedAt: new Date().toISOString(),
      metrics: [],
      message: "Personal Codex allowance is not available through a documented third-party API.",
    },
  ],
};

const providerNames: Record<Provider, string> = {
  github: "GitHub Copilot",
  anthropic: "Claude",
  openai: "OpenAI / Codex",
};

const providerMarks: Record<Provider, string> = {
  github: "GH",
  anthropic: "AN",
  openai: "OA",
};

function isTauri() {
  return "__TAURI_INTERNALS__" in window;
}

function formatNumber(value: number, unit: string) {
  if (unit === "USD") {
    return new Intl.NumberFormat("en-US", {
      style: "currency",
      currency: "USD",
      minimumFractionDigits: 2,
      maximumFractionDigits: 3,
    }).format(value);
  }
  return new Intl.NumberFormat("en-US", {
    notation: unit !== "AIC" && value >= 100_000 ? "compact" : "standard",
    minimumFractionDigits: 0,
    maximumFractionDigits: unit === "AIC" ? 2 : 1,
  }).format(value);
}

export function formatPercentage(value: number) {
  const safe = Math.max(0, Math.min(100, value));
  if (safe === 0 || safe === 100) return `${safe}%`;
  const rounded = Math.round(safe * 100) / 100;
  if (rounded === 0) return "<0.01%";
  if (rounded >= 100) return "99.99%";
  return `${new Intl.NumberFormat("en-US", { maximumFractionDigits: 2 }).format(rounded)}%`;
}

function formatResetDate(resetAt: string) {
  return new Date(resetAt).toLocaleDateString([], {
    month: "short",
    day: "numeric",
    timeZone: "UTC",
  });
}

export interface CombinedUsage {
  percentage?: number;
  accountCount: number;
  unitPercentages: Record<string, number>;
}

const selectionStorageKey = "ai-allowance.selected-accounts.v1";
const refreshStorageKey = "ai-allowance.refresh-schedule.v1";

interface SelectionPreference {
  customized: boolean;
  accountIds: string[];
}

interface RefreshPreference {
  localSeconds: number;
  providerMinutes: number;
}

export function metricUsedAmount(metric: AllowanceMetric) {
  if (metric.limit === undefined || !Number.isFinite(metric.limit) || metric.limit <= 0) return undefined;
  const used = metric.remaining !== undefined && Number.isFinite(metric.remaining)
    ? metric.limit - metric.remaining
    : Number.isFinite(metric.consumed)
      ? metric.consumed
      : undefined;
  return used === undefined ? undefined : Math.max(0, Math.min(metric.limit, used));
}

function metricRemainingAmount(metric: AllowanceMetric) {
  const used = metricUsedAmount(metric);
  return used === undefined || metric.limit === undefined ? undefined : metric.limit - used;
}

export function isEligibleSnapshot(snapshot: ProviderSnapshot) {
  return snapshot.metrics.some((metric) => metricUsedAmount(metric) !== undefined);
}

export function calculateCombinedUsage(snapshots: ProviderSnapshot[]): CombinedUsage {
  const eligibleSnapshots = snapshots.filter(isEligibleSnapshot);
  const groups = new Map<string, { used: number; limit: number }>();

  for (const snapshot of eligibleSnapshots) {
    for (const metric of snapshot.metrics) {
      const used = metricUsedAmount(metric);
      if (used === undefined || metric.limit === undefined) continue;
      const unit = metric.unit.trim().toLowerCase() || metric.kind;
      const group = groups.get(unit) ?? { used: 0, limit: 0 };
      group.used += used;
      group.limit += metric.limit;
      groups.set(unit, group);
    }
  }

  const unitPercentages = Object.fromEntries(
    [...groups.entries()].map(([unit, group]) => [
      unit,
      Math.max(0, Math.min(100, (group.used / group.limit) * 100)),
    ]),
  );
  const percentages = Object.values(unitPercentages);

  return {
    percentage: percentages.length
      ? percentages.reduce((sum, percentage) => sum + percentage, 0) / percentages.length
      : undefined,
    accountCount: eligibleSnapshots.length,
    unitPercentages,
  };
}

function loadSelectionPreference(): SelectionPreference {
  try {
    const stored = window.localStorage.getItem(selectionStorageKey);
    if (!stored) return { customized: false, accountIds: [] };
    const parsed = JSON.parse(stored) as Partial<SelectionPreference>;
    return {
      customized: parsed.customized === true,
      accountIds: Array.isArray(parsed.accountIds)
        ? parsed.accountIds.filter((id): id is string => typeof id === "string")
        : [],
    };
  } catch {
    return { customized: false, accountIds: [] };
  }
}

function loadRefreshPreference(): RefreshPreference {
  try {
    const stored = window.localStorage.getItem(refreshStorageKey);
    if (!stored) return { localSeconds: 5, providerMinutes: 15 };
    const parsed = JSON.parse(stored) as Partial<RefreshPreference>;
    return {
      localSeconds: [5, 15, 30, 60].includes(parsed.localSeconds ?? 0) ? parsed.localSeconds! : 5,
      providerMinutes: [1, 5, 15, 30, 60].includes(parsed.providerMinutes ?? 0) ? parsed.providerMinutes! : 15,
    };
  } catch {
    return { localSeconds: 5, providerMinutes: 15 };
  }
}

function nearestResetAt(snapshots: ProviderSnapshot[]) {
  return snapshots
    .map((snapshot) => snapshot.resetAt)
    .filter((resetAt): resetAt is string => typeof resetAt === "string" && Number.isFinite(Date.parse(resetAt)))
    .sort((left, right) => Date.parse(left) - Date.parse(right))[0];
}

function Hourglass({ percentage, compact = false }: { percentage?: number; compact?: boolean }) {
  const safe = Math.max(0, Math.min(100, percentage ?? 45));
  const status = percentage === undefined ? "unknown" : safe >= 90 ? "critical" : safe >= 75 ? "warning" : "healthy";
  const topHeight = 58 * (100 - safe) / 100;
  const bottomHeight = 58 * safe / 100;
  const formattedPercentage = percentage === undefined ? undefined : formatPercentage(safe);
  return (
    <div className={`hourglass ${compact ? "hourglass--compact" : ""} hourglass--${status}`} role="img" aria-label={formattedPercentage === undefined ? "Combined usage unavailable" : `${formattedPercentage.replace("%", " percent")} used`}>
      <div className="hourglass__glow" />
      <svg viewBox="0 0 160 210" aria-hidden="true">
        <defs>
          <linearGradient id={`glass-${compact}`} x1="0" x2="1">
            <stop offset="0" stopColor="var(--cp-panel)" />
            <stop offset="0.42" stopColor="var(--cp-sheen)" />
            <stop offset="0.7" stopColor="var(--cp-panel)" />
            <stop offset="1" stopColor="var(--cp-surface-soft)" />
          </linearGradient>
          <linearGradient id={`metal-${compact}`} x1="0" y1="0" x2="1" y2="1">
            <stop offset="0" stopColor="var(--cp-text-soft)" />
            <stop offset="0.5" stopColor="var(--cp-text)" />
            <stop offset="1" stopColor="var(--cp-border-strong)" />
          </linearGradient>
          <clipPath id={`top-chamber-${compact}`}>
            <path d="M45 39 H115 C113 67 99 82 84 101 H76 C61 82 47 67 45 39 Z" />
          </clipPath>
          <clipPath id={`bottom-chamber-${compact}`}>
            <path d="M76 105 H84 C99 124 113 139 115 169 H45 C47 139 61 124 76 105 Z" />
          </clipPath>
        </defs>
        <rect className="hourglass__base-shadow" x="25" y="183" width="110" height="10" rx="5" />
        <rect className="hourglass__base" x="20" y="176" width="120" height="13" rx="6.5" fill={`url(#metal-${compact})`} />
        <rect className="hourglass__cap" x="20" y="21" width="120" height="13" rx="6.5" fill={`url(#metal-${compact})`} />
        <path className="hourglass__glass" d="M39 34 H121 C119 68 104 85 87 103 C104 121 119 138 121 176 H39 C41 138 56 121 73 103 C56 85 41 68 39 34 Z" fill={`url(#glass-${compact})`} />
        <g clipPath={`url(#top-chamber-${compact})`}>
          <rect className="hourglass__sand-fill" x="43" y={97 - topHeight} width="74" height={topHeight} />
          <path className="hourglass__sand-ridge" d={`M43 ${98 - topHeight} Q80 ${90 - topHeight} 117 ${98 - topHeight} V105 H43 Z`} />
        </g>
        <g clipPath={`url(#bottom-chamber-${compact})`}>
          <rect className="hourglass__sand-fill" x="43" y={169 - bottomHeight} width="74" height={bottomHeight} />
          <path className="hourglass__sand-ridge" d={`M43 ${170 - bottomHeight} Q80 ${153 - bottomHeight} 117 ${170 - bottomHeight} V177 H43 Z`} />
        </g>
        {safe > 0 && safe < 100 && (
          <>
            <line className="hourglass__sand-stream" x1="80" y1="100" x2="80" y2="145" />
            <circle className="hourglass__grain hourglass__grain--one" cx="78" cy="121" r="1.5" />
            <circle className="hourglass__grain hourglass__grain--two" cx="82" cy="136" r="1.2" />
          </>
        )}
        <path className="hourglass__rod" d="M31 31 L47 180 M129 31 L113 180" />
        <path className="hourglass__shine" d="M54 43 C56 68 65 81 75 94" />
      </svg>
      <strong>{formattedPercentage ?? "—"}</strong>
    </div>
  );
}

function ProviderCard({ snapshot }: { snapshot: ProviderSnapshot }) {
  const percentage = calculateCombinedUsage([snapshot]).percentage;
  const statusLabel = snapshot.freshness === "fresh"
    ? "Connected"
    : snapshot.freshness === "error"
      ? "Connected · refresh issue"
      : snapshot.freshness === "unsupported"
        ? "Connected · metric unsupported"
        : "Connected · stale";
  return (
    <article className={`provider-card provider-card--${snapshot.freshness}`}>
      <header>
        <div className={`provider-mark provider-mark--${snapshot.provider}`}>{providerMarks[snapshot.provider]}</div>
        <div className="provider-identity">
          <h3>{snapshot.label}</h3>
          <p>{snapshot.scope}</p>
        </div>
        <div className="provider-state">
          {percentage !== undefined && <strong>{formatPercentage(percentage)} used</strong>}
          <small className={`connection-label connection-label--${snapshot.freshness}`}>
            <span className={`status-dot status-dot--${snapshot.freshness}`} aria-hidden="true" />
            {statusLabel}
          </small>
        </div>
      </header>

      {snapshot.metrics.length > 0 ? (
        <div className="provider-content">
          <div className="metric-list">
            {snapshot.metrics.map((metric) => {
              const used = metricUsedAmount(metric);
              const remaining = metricRemainingAmount(metric);
              return (
                <div className="metric" key={`${metric.kind}-${metric.label}`}>
                  <span>{metric.label}</span>
                  <strong>{remaining !== undefined ? `${formatNumber(remaining, metric.unit)} left` : formatNumber(metric.consumed, metric.unit)}</strong>
                  {used !== undefined && metric.limit !== undefined && <small>{formatNumber(used, metric.unit)} used of {formatNumber(metric.limit, metric.unit)}</small>}
                </div>
              );
            })}
          </div>
        </div>
      ) : (
        <div className="unavailable">
          <span aria-hidden="true">!</span>
          <div>
            <strong>{snapshot.freshness === "error" ? "Account connected, usage unavailable" : "Unavailable from provider API"}</strong>
            <p>{snapshot.message}</p>
            {snapshot.provider === "github" && snapshot.message?.includes("needs the `user` scope") && (
              <code>gh auth refresh -h github.com -s user</code>
            )}
          </div>
        </div>
      )}

      {snapshot.metrics.length > 0 && snapshot.message && <p className="provider-note">{snapshot.message}</p>}
      <footer>
        <span>Updated {new Date(snapshot.updatedAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</span>
        {snapshot.resetAt && <span>Resets {formatResetDate(snapshot.resetAt)} UTC</span>}
      </footer>
    </article>
  );
}

function AccountDialog({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const [form, setForm] = useState<AccountInput>({ provider: "github", label: "GitHub Copilot", scopeType: "personal", scope: "", secret: "" });
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (!isTauri()) {
      setError("Account storage is available in the native desktop app.");
      return;
    }
    setSaving(true);
    setError("");
    try {
      await invoke("save_account", { input: form });
      onSaved();
      onClose();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="dialog-backdrop" role="presentation" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <form className="dialog" onSubmit={submit}>
        <div className="dialog__header">
          <div><span className="eyebrow">Official API connection</span><h2>Add provider account</h2></div>
          <button className="icon-button" type="button" onClick={onClose} aria-label="Close">×</button>
        </div>
        <label>Provider
          <select value={form.provider} onChange={(event) => {
            const provider = event.target.value as Provider;
            setForm({ ...form, provider, label: providerNames[provider] });
          }}>
            <option value="github">GitHub Copilot</option>
            <option value="anthropic">Anthropic Claude API</option>
            <option value="openai">OpenAI API</option>
          </select>
        </label>
        <label>Account type
          <select value={form.scopeType} onChange={(event) => setForm({ ...form, scopeType: event.target.value as AccountInput["scopeType"] })}>
            <option value="personal">Personal</option>
            <option value="organization">Organization</option>
          </select>
        </label>
        <label>{form.scopeType === "organization" ? "Organization slug / ID" : "Username (GitHub only)"}
          <input value={form.scope} onChange={(event) => setForm({ ...form, scope: event.target.value })} placeholder={form.scopeType === "organization" ? "my-organization" : "optional"} />
        </label>
        <label>Display name
          <input value={form.label} onChange={(event) => setForm({ ...form, label: event.target.value })} required />
        </label>
        <label>{form.provider === "github" ? "GitHub token" : "Admin API key"}
          <input type="password" value={form.secret} onChange={(event) => setForm({ ...form, secret: event.target.value })} required autoComplete="off" />
        </label>
        <p className="security-note">The secret is stored in Windows Credential Manager and is never exposed to the webview after saving.</p>
        {error && <p className="form-error">{error}</p>}
        <div className="dialog__actions">
          <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
          <button className="button button--primary" type="submit" disabled={saving}>{saving ? "Saving…" : "Save account"}</button>
        </div>
      </form>
    </div>
  );
}

interface SettingsPageProps {
  providers: ProviderSnapshot[];
  eligibleProviders: ProviderSnapshot[];
  selectedAccountIds: Set<string>;
  selectionCustomized: boolean;
  refreshPreference: RefreshPreference;
  theme: string;
  localAccounts: LocalAccountCandidate[];
  discovering: boolean;
  onClose: () => void;
  onToggleSelected: (accountId: string) => void;
  onUseAll: () => void;
  onRefreshChange: (preference: RefreshPreference) => void;
  onThemeChange: (theme: "light" | "dark") => void;
  onAddAccount: () => void;
  onDiscover: () => void;
  onConnectLocal: (candidate: LocalAccountCandidate) => void;
}

function SettingsPage({
  providers,
  eligibleProviders,
  selectedAccountIds,
  selectionCustomized,
  refreshPreference,
  theme,
  localAccounts,
  discovering,
  onClose,
  onToggleSelected,
  onUseAll,
  onRefreshChange,
  onThemeChange,
  onAddAccount,
  onDiscover,
  onConnectLocal,
}: SettingsPageProps) {
  const [section, setSection] = useState<"general" | "accounts" | "appearance">("general");
  const sectionCopy = {
    general: ["General", "Control how often AI Allowance updates its local and provider data."],
    accounts: ["Accounts", "Choose summary participation and manage provider connections."],
    appearance: ["Appearance", "Choose how AI Allowance looks on this device."],
  } as const;

  return (
    <div className="settings-workspace">
      <aside className="settings-sidebar">
        <div className="settings-sidebar__title">
          <div className="brand-mark" aria-hidden="true">AI</div>
          <div>
            <strong>AI Allowance</strong>
            <span>Settings</span>
          </div>
        </div>
        <nav className="settings-nav" aria-label="Settings categories">
          <button className={section === "general" ? "settings-nav__item settings-nav__item--active" : "settings-nav__item"} onClick={() => setSection("general")}>
            <span aria-hidden="true">↻</span><span><strong>General</strong><small>Refresh schedule</small></span>
          </button>
          <button className={section === "accounts" ? "settings-nav__item settings-nav__item--active" : "settings-nav__item"} onClick={() => setSection("accounts")}>
            <span aria-hidden="true">◎</span><span><strong>Accounts</strong><small>Providers and summary</small></span>
          </button>
          <button className={section === "appearance" ? "settings-nav__item settings-nav__item--active" : "settings-nav__item"} onClick={() => setSection("appearance")}>
            <span aria-hidden="true">◐</span><span><strong>Appearance</strong><small>Light or dark</small></span>
          </button>
        </nav>
        <div className="settings-sidebar__footer"><i className="privacy-dot" /> Local settings only</div>
      </aside>

      <section className="settings-content">
        <header className="settings-content__header">
          <div>
            <span className="eyebrow">Settings</span>
            <h1>{sectionCopy[section][0]}</h1>
            <p>{sectionCopy[section][1]}</p>
          </div>
          <button className="icon-button" onClick={onClose} aria-label="Close settings" title="Close settings">×</button>
        </header>

        {section === "general" && (
          <section className="settings-card refresh-settings" aria-labelledby="refresh-settings-title">
            <div className="settings-card__heading">
              <div>
                <span className="eyebrow">Updates</span>
                <h2 id="refresh-settings-title">Refresh schedule</h2>
              </div>
            </div>
            <div className="refresh-fields">
              <label>
                Provider allowance and cost
                <select
                  value={refreshPreference.providerMinutes}
                  onChange={(event) => onRefreshChange({ ...refreshPreference, providerMinutes: Number(event.target.value) })}
                >
                  <option value={1}>Every minute</option>
                  <option value={5}>Every 5 minutes</option>
                  <option value={15}>Every 15 minutes</option>
                  <option value={30}>Every 30 minutes</option>
                  <option value={60}>Every hour</option>
                </select>
                <small>Official provider requests may be delayed or rate-limited.</small>
              </label>
              <label>
                Local Copilot telemetry
                <select
                  value={refreshPreference.localSeconds}
                  onChange={(event) => onRefreshChange({ ...refreshPreference, localSeconds: Number(event.target.value) })}
                >
                  <option value={5}>Every 5 seconds</option>
                  <option value={15}>Every 15 seconds</option>
                  <option value={30}>Every 30 seconds</option>
                  <option value={60}>Every minute</option>
                </select>
                <small>Reads only the local Copilot session database.</small>
              </label>
            </div>
          </section>
        )}

        {section === "accounts" && (
          <>
            <section className="account-selection settings-card" aria-labelledby="account-selection-title">
              <div className="account-selection__heading">
                <div>
                  <span className="eyebrow">Included in summary</span>
                  <h2 id="account-selection-title">Allowance accounts</h2>
                </div>
                {selectionCustomized && eligibleProviders.length > 0 && (
                  <button className="selection-reset" type="button" onClick={onUseAll}>Use all automatically</button>
                )}
              </div>
              {eligibleProviders.length > 0 ? (
                <div className="account-selection__options">
                  {eligibleProviders.map((provider) => (
                    <label className="account-toggle" key={provider.accountId}>
                      <input type="checkbox" checked={selectedAccountIds.has(provider.accountId)} onChange={() => onToggleSelected(provider.accountId)} />
                      <span className="account-toggle__control" aria-hidden="true" />
                      <span className="account-toggle__label"><strong>{provider.label}</strong><small>{provider.scope}</small></span>
                    </label>
                  ))}
                </div>
              ) : (
                <p className="account-selection__empty">No accounts with an authoritative positive limit are available.</p>
              )}
            </section>

            <section className="settings-card account-management" aria-labelledby="account-management-title">
              <div className="settings-card__heading">
                <div><span className="eyebrow">Connections</span><h2 id="account-management-title">Provider accounts</h2></div>
                <div className="settings-actions">
                  <button className="button button--secondary" onClick={onDiscover} disabled={discovering}>{discovering ? "Scanning…" : "Scan this machine"}</button>
                  <button className="button button--primary" onClick={onAddAccount}>Add provider account</button>
                </div>
              </div>
              {providers.length > 0 ? (
                <div className="managed-account-list">
                  {providers.map((provider) => (
                    <div className="managed-account" key={provider.accountId}>
                      <div className={`provider-mark provider-mark--${provider.provider}`}>{providerMarks[provider.provider]}</div>
                      <div><strong>{provider.label}</strong><small>{provider.scope}</small></div>
                      <span className={`connection-label connection-label--${provider.freshness}`}>
                        <i className={`status-dot status-dot--${provider.freshness}`} aria-hidden="true" />
                        {provider.freshness === "fresh" ? "Connected" : provider.freshness}
                      </span>
                    </div>
                  ))}
                </div>
              ) : (
                <p className="account-selection__empty">No provider accounts are connected.</p>
              )}
              {localAccounts.length > 0 && (
                <div className="local-account-results">
                  <span className="eyebrow">Found on this machine</span>
                  {localAccounts.map((candidate) => (
                    <div className="discovered-account" key={candidate.id}>
                      <div className={`provider-mark provider-mark--${candidate.provider}`}>{providerMarks[candidate.provider]}</div>
                      <div><strong>{candidate.label} · {candidate.account}</strong><p>{candidate.message}</p></div>
                      <button className="button button--primary" disabled={!candidate.canConnect} onClick={() => onConnectLocal(candidate)}>
                        {candidate.canConnect ? "Connect" : "Not importable"}
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </section>
          </>
        )}

        {section === "appearance" && (
          <section className="settings-card appearance-settings" aria-labelledby="appearance-settings-title">
            <span className="eyebrow">Theme</span>
            <h2 id="appearance-settings-title">Color mode</h2>
            <p>Choose the appearance used by the dashboard, Settings, and widget.</p>
            <div className="theme-options">
              <button className={theme === "light" ? "theme-option theme-option--active" : "theme-option"} onClick={() => onThemeChange("light")}>
                <span className="theme-option__preview theme-option__preview--light" aria-hidden="true" />
                <strong>Light</strong><small>Warm and bright</small>
              </button>
              <button className={theme === "dark" ? "theme-option theme-option--active" : "theme-option"} onClick={() => onThemeChange("dark")}>
                <span className="theme-option__preview theme-option__preview--dark" aria-hidden="true" />
                <strong>Dark</strong><small>Calm and low-glare</small>
              </button>
            </div>
          </section>
        )}
      </section>
    </div>
  );
}

function App() {
  const widgetMode = isTauri() && getCurrentWindow().label === "widget";
  const [dashboard, setDashboard] = useState<Dashboard>(() => isTauri() ? emptyDashboard : demoDashboard);
  const [activePage, setActivePage] = useState<"dashboard" | "settings">("dashboard");
  const [refreshing, setRefreshing] = useState(false);
  const [discovering, setDiscovering] = useState(false);
  const [localAccounts, setLocalAccounts] = useState<LocalAccountCandidate[]>([]);
  const [showDialog, setShowDialog] = useState(false);
  const [theme, setTheme] = useState(document.documentElement.dataset.theme ?? "light");
  const [selectionPreference, setSelectionPreference] = useState<SelectionPreference>(loadSelectionPreference);
  const [refreshPreference, setRefreshPreference] = useState<RefreshPreference>(loadRefreshPreference);

  const eligibleProviders = useMemo(() => dashboard.providers.filter(isEligibleSnapshot), [dashboard]);
  const eligibleAccountIds = useMemo(() => eligibleProviders.map((provider) => provider.accountId), [eligibleProviders]);
  const selectedAccountIds = useMemo(
    () => selectionPreference.customized
      ? new Set(selectionPreference.accountIds)
      : new Set(eligibleAccountIds),
    [eligibleAccountIds, selectionPreference],
  );
  const selectedProviders = useMemo(
    () => eligibleProviders.filter((provider) => selectedAccountIds.has(provider.accountId)),
    [eligibleProviders, selectedAccountIds],
  );
  const combinedUsage = useMemo(() => calculateCombinedUsage(selectedProviders), [selectedProviders]);
  const overall = combinedUsage.percentage;
  const hasAccounts = dashboard.providers.length > 0;
  const liveAic = dashboard.providers
    .flatMap((provider) => provider.metrics)
    .find((metric) => metric.unit === "AIC");
  const nearestReset = nearestResetAt(selectedProviders);

  async function loadDashboard(refresh = false) {
    if (!isTauri()) return;
    setRefreshing(true);
    try {
      const next = await invoke<Dashboard>(refresh ? "refresh_dashboard" : "get_dashboard");
      setDashboard(next);
    } catch (error) {
      console.error("Unable to load dashboard", error);
    } finally {
      setRefreshing(false);
    }
  }

  async function discoverLocalAccounts() {
    if (!isTauri()) return;
    setDiscovering(true);
    try {
      setLocalAccounts(await invoke<LocalAccountCandidate[]>("discover_local_accounts"));
    } finally {
      setDiscovering(false);
    }
  }

  async function autoWireLocalAccounts() {
    if (!isTauri()) return;
    setDiscovering(true);
    try {
      const discovered = await invoke<LocalAccountCandidate[]>("auto_wire_local_accounts");
      setLocalAccounts(discovered.filter((account) => !account.connected));
      await loadDashboard(true);
    } catch (error) {
      console.error("Unable to auto-wire local accounts", error);
    } finally {
      setDiscovering(false);
    }
  }

  async function connectLocalAccount(candidate: LocalAccountCandidate) {
    await invoke("connect_local_account", { candidateId: candidate.id });
    setLocalAccounts((accounts) => accounts.filter((account) => account.id !== candidate.id));
    await loadDashboard(true);
  }

  async function openWidget() {
    if (isTauri()) await invoke("set_widget_visible", { visible: true });
  }

  function applyTheme(next: "light" | "dark") {
    document.documentElement.dataset.theme = next;
    setTheme(next);
  }

  function toggleTheme() {
    applyTheme(theme === "dark" ? "light" : "dark");
  }

  function toggleSelectedAccount(accountId: string) {
    setSelectionPreference((current) => {
      const selected = new Set(current.customized ? current.accountIds : eligibleAccountIds);
      if (selected.has(accountId)) selected.delete(accountId);
      else selected.add(accountId);
      return { customized: true, accountIds: [...selected] };
    });
  }

  function useAllEligibleAccounts() {
    setSelectionPreference({ customized: false, accountIds: [] });
  }

  useEffect(() => {
    try {
      window.localStorage.setItem(selectionStorageKey, JSON.stringify(selectionPreference));
    } catch {
      // Keep the current in-memory selection when storage is unavailable.
    }
  }, [selectionPreference]);

  useEffect(() => {
    try {
      window.localStorage.setItem(refreshStorageKey, JSON.stringify(refreshPreference));
    } catch {
      // Keep the current in-memory schedule when storage is unavailable.
    }
  }, [refreshPreference]);

  useEffect(() => {
    const syncPreferences = (event: StorageEvent) => {
      if (event.key === selectionStorageKey) setSelectionPreference(loadSelectionPreference());
      if (event.key === refreshStorageKey) setRefreshPreference(loadRefreshPreference());
    };
    window.addEventListener("storage", syncPreferences);
    if (isTauri()) {
      void autoWireLocalAccounts();
    } else {
      void loadDashboard();
    }
    if (!isTauri()) return () => {
      window.removeEventListener("storage", syncPreferences);
    };

    const unlistenRefresh = listen("refresh-requested", () => loadDashboard(true));
    const unlistenDashboard = listen<Dashboard>("dashboard-updated", (event) => setDashboard(event.payload));
    return () => {
      window.removeEventListener("storage", syncPreferences);
      void unlistenRefresh.then((unlisten) => unlisten());
      void unlistenDashboard.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    const localInterval = window.setInterval(() => loadDashboard(false), refreshPreference.localSeconds * 1000);
    const providerInterval = window.setInterval(() => loadDashboard(true), refreshPreference.providerMinutes * 60 * 1000);
    return () => {
      window.clearInterval(localInterval);
      window.clearInterval(providerInterval);
    };
  }, [refreshPreference]);

  if (widgetMode) {
    return (
      <main className="app-shell app-shell--widget">
        <section className="widget-summary" aria-label="Combined selected usage">
          <Hourglass percentage={overall} />
          <span className="widget-used-caption">used</span>
          <div className="widget-context">
            <span>{selectedProviders.length} selected</span>
            <span>{nearestReset ? `Resets ${formatResetDate(nearestReset)} UTC` : "Reset unavailable"}</span>
          </div>
        </section>
      </main>
    );
  }

  if (activePage === "settings") {
    return (
      <main className="app-shell app-shell--settings">
        <SettingsPage
          providers={dashboard.providers}
          eligibleProviders={eligibleProviders}
          selectedAccountIds={selectedAccountIds}
          selectionCustomized={selectionPreference.customized}
          refreshPreference={refreshPreference}
          theme={theme}
          localAccounts={localAccounts}
          discovering={discovering}
          onClose={() => setActivePage("dashboard")}
          onToggleSelected={toggleSelectedAccount}
          onUseAll={useAllEligibleAccounts}
          onRefreshChange={setRefreshPreference}
          onThemeChange={applyTheme}
          onAddAccount={() => setShowDialog(true)}
          onDiscover={discoverLocalAccounts}
          onConnectLocal={connectLocalAccount}
        />
        {showDialog && <AccountDialog onClose={() => setShowDialog(false)} onSaved={() => loadDashboard(true)} />}
      </main>
    );
  }

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand">
          <div className="brand-mark" aria-hidden="true">AI</div>
          <div><h1>AI Allowance</h1><p>Official usage across your agents</p></div>
        </div>
        <div className="topbar-actions">
          <button className="icon-button" onClick={toggleTheme} aria-label="Toggle theme">{theme === "dark" ? "☀" : "◐"}</button>
          <button className="widget-button" onClick={openWidget} aria-label="Open desktop widget" title="Open compact always-on-top widget"><span>▣</span> Widget</button>
          <button className="icon-button" onClick={() => setActivePage("settings")} aria-label="Open settings" title="Settings">⚙</button>
        </div>
      </header>

      <>
          <section className="summary">
            <div className="summary-copy">
              <span className="eyebrow">Combined selected usage</span>
              <h2>{overall === undefined ? "Usage unavailable" : `${formatPercentage(overall)} used`}</h2>
              <p>{overall === undefined
                ? eligibleProviders.length
                  ? "Select at least one eligible account in Settings to calculate combined usage."
                  : liveAic
                    ? `${formatNumber(liveAic.consumed, "AIC")} AIC is reported, but no authoritative positive limit is available.`
                    : hasAccounts
                      ? "Connected accounts do not currently report an authoritative positive limit."
                      : "Connect an account with an official usage limit."
                : "For each unit, used amounts and limits are summed across selected accounts; those unit percentages are then averaged equally. Reported remaining values take precedence when deriving used amounts."}</p>
              <div className="summary-stats">
                <div><span>Next reset</span><strong>{nearestReset ? `${formatResetDate(nearestReset)} UTC` : "Unavailable"}</strong></div>
                <div><span>Selected accounts</span><strong>{selectedProviders.length} / {eligibleProviders.length}</strong></div>
                <div><span>Last sync</span><strong>{new Date(dashboard.generatedAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</strong></div>
              </div>
            </div>
            <Hourglass percentage={overall} />
          </section>

          <div className="section-heading">
            <div><span className="eyebrow">Accounts</span><h2>Provider status</h2></div>
            <button className="button button--secondary" onClick={() => loadDashboard(true)} disabled={refreshing}>
              <span className={refreshing ? "spin" : ""}>↻</span> {refreshing ? "Refreshing…" : "Refresh"}
            </button>
          </div>

          {dashboard.providers.length > 0 ? (
            <section className="provider-grid">
              {dashboard.providers.map((provider) => <ProviderCard snapshot={provider} key={provider.accountId} />)}
            </section>
          ) : (
            <section className="empty-state">
              <h3>No accounts connected</h3>
              <p>Open Settings to connect an authenticated local CLI account or add an official provider API credential.</p>
              <div>
                <button className="button button--primary" onClick={() => setActivePage("settings")}>Open settings</button>
              </div>
            </section>
          )}

          <footer className="app-footer">
            <span><i className="privacy-dot" /> Data stays on this device</span>
          </footer>
      </>

      {showDialog && <AccountDialog onClose={() => setShowDialog(false)} onSaved={() => loadDashboard(true)} />}
    </main>
  );
}

export default App;
