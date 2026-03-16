// Dev mode: relative paths go through Vite proxy → backend
// Production: served by Axum, relative paths hit the same origin
const BASE = "";

// --- Types ---

export interface HealthResponse {
  status: string;
  service: string;
  uptime_seconds: number;
  checks: Record<string, string>;
}

export interface StatusResponse {
  uptime_seconds: number;
  enabled_strategies: string[];
  scan_interval_ms: number;
  health_port: number;
  lr_enabled: boolean;
  event_calendar_enabled: boolean;
  accounts_configured: number;
  accounts_ready: number;
  trading_ready: boolean;
  positions_snapshot_updated_at: string | null;
  accounts: AccountStatusEntry[];
}

export interface AccountStatusEntry {
  name: string;
  strategies: string[];
  proxy_wallet: string;
  private_key_env: string;
  private_key_present: boolean;
}

export interface LrMarketStatus {
  condition_id: string;
  question: string;
  daily_rate: string;
  outstanding_orders: number;
  yes_bid: string | null;
  yes_ask: string | null;
  no_bid: string | null;
  no_ask: string | null;
}

export interface LrRuntimeStatus {
  active_markets: LrMarketStatus[];
  total_exposure: string;
  cached_balance: string;
  market_mode: string;
  last_refresh: string | null;
}

export interface SectionMeta {
  target_cities_options?: string[];
  supported_cities_options?: string[];
  target_cities_empty_means_all?: boolean;
  target_cities_risk_tiers?: Record<string, "low" | "medium" | "high">;
  target_cities_providers?: Record<string, "noaa" | "open_meteo" | "kma">;
  target_cities_trade_enabled?: Record<string, boolean>;
  target_cities_settlement_notes?: Record<string, string>;
  target_cities_validation_status?: Record<string, "validated" | "default_protected">;
  target_cities_extra_edge_bps?: Record<string, number>;
  target_cities_sigma_multipliers?: Record<string, number>;
}

export interface PositionEntry {
  token_id: string;
  size: string;
  avg_cost: string;
  cost_basis: string;
  strategy: string | null;
  condition_id: string | null;
  question: string | null;
  outcome: string | null;
  current_price: string | null;
  unrealized_pnl: string | null;
}

// --- API functions ---

async function get<T>(path: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`);
  if (!res.ok) {
    const body = await res.text();
    throw new Error(`${res.status}: ${body}`);
  }
  return res.json();
}

export function fetchHealth(): Promise<HealthResponse> {
  return get("/health");
}

export function fetchStatus(): Promise<StatusResponse> {
  return get("/api/status");
}

export function fetchConfig(): Promise<Record<string, unknown>> {
  return get("/api/config");
}

export function fetchSection(section: string): Promise<Record<string, unknown>> {
  return get(`/api/config/${section}`);
}

export function fetchSectionMeta(section: string): Promise<SectionMeta> {
  return get(`/api/config/meta/${section}`);
}

export function fetchLRStatus(): Promise<LrRuntimeStatus> {
  return get("/api/lr/status");
}

export function fetchPositions(strategy?: string): Promise<PositionEntry[]> {
  const q = strategy ? `?strategy=${encodeURIComponent(strategy)}` : "";
  return get(`/api/positions${q}`);
}

export async function fetchMetrics(): Promise<string> {
  const res = await fetch(`${BASE}/metrics`);
  if (!res.ok) throw new Error(`metrics: ${res.status}`);
  return res.text();
}
