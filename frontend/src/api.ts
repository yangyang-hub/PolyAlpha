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
  wallet_balance: string;
  strategy_financials?: Record<string, {
    wallet_balance: string;
    positions_market_value: string;
    portfolio_value: string;
    realized_pnl: string;
  }>;
  positions_snapshot_updated_at: string | null;
  crypto_gate_reject_summary?: {
    recent_count: number;
    top_reason: { label: string; count: number } | null;
    top_asset: { label: string; count: number } | null;
    top_subtype: { label: string; count: number } | null;
    reason_counts: { label: string; count: number }[];
    asset_counts: { label: string; count: number }[];
    subtype_counts: { label: string; count: number }[];
    reason_windows: {
      recent_8: { label: string; count: number }[];
      recent_24: { label: string; count: number }[];
    };
    asset_windows: {
      recent_8: { label: string; count: number }[];
      recent_24: { label: string; count: number }[];
    };
    subtype_windows: {
      recent_8: { label: string; count: number }[];
      recent_24: { label: string; count: number }[];
    };
    reason_details: {
      label: string;
      count: number;
      top_asset: { label: string; count: number } | null;
      top_subtype: { label: string; count: number } | null;
    }[];
  };
  crypto_gate_scale_summary?: {
    recent_count: number;
    top_reason: { label: string; count: number } | null;
    top_asset: { label: string; count: number } | null;
    top_subtype: { label: string; count: number } | null;
    reason_counts: { label: string; count: number }[];
    asset_counts: { label: string; count: number }[];
    subtype_counts: { label: string; count: number }[];
    reason_windows: {
      recent_8: { label: string; count: number }[];
      recent_24: { label: string; count: number }[];
    };
    asset_windows: {
      recent_8: { label: string; count: number }[];
      recent_24: { label: string; count: number }[];
    };
    subtype_windows: {
      recent_8: { label: string; count: number }[];
      recent_24: { label: string; count: number }[];
    };
    reason_details: {
      label: string;
      count: number;
      top_asset: { label: string; count: number } | null;
      top_subtype: { label: string; count: number } | null;
    }[];
  };
  crypto_entry_tuning_hints?: {
    kind: string;
    priority: string;
    title: string;
    detail: string;
    scope_label?: string;
    support_count?: number;
  }[];
  crypto_override_suggestions?: {
    kind?: string;
    priority: string;
    target_field: string;
    direction: string;
    selector_asset_class: string;
    selector_event_subtype: string;
    scope_label: string;
    source_reason: string;
    rationale: string;
    support_count?: number;
  }[];
  crypto_post_entry_tuning_hints?: {
    kind: string;
    priority: string;
    title: string;
    detail: string;
    scope_label?: string;
    support_count?: number;
  }[];
  crypto_post_entry_override_suggestions?: {
    kind?: string;
    priority: string;
    target_field: string;
    direction: string;
    selector_asset_class: string;
    selector_event_subtype: string;
    scope_label: string;
    source_reason: string;
    rationale: string;
    support_count?: number;
  }[];
  smart_money_signal_summary?: {
    recent_signal_count: number;
    recent_entry_attempts: number;
    recent_entry_accepted: number;
    recent_entry_rejected: number;
    wallet_counts: { label: string; count: number }[];
    source_counts: { label: string; count: number }[];
  };
  smart_money_gate_reject_summary?: {
    total_rejected: number;
    reason_counts: { label: string; count: number }[];
  };
  smart_money_exit_summary?: {
    total_exits: number;
    reason_counts: { label: string; count: number }[];
  };
  smart_money_wallet_scores?: {
    address: string;
    label: string;
    base_weight: string;
    effective_weight: string;
    profile_score: string;
    recent_signal_count: number;
    auto_discovered: boolean;
  }[];
  smart_money_recent_decisions?: {
    recorded_at: string;
    token_id: string;
    condition_id: string;
    signal_type: string;
    accepted: boolean;
    reject_reason: string | null;
    wallet_count: number;
    max_wallet_weight: string;
    source_data_api: boolean;
    source_onchain: boolean;
  }[];
  smart_money_recent_exits?: {
    recorded_at: string;
    token_id: string;
    condition_id: string;
    reason: string;
    question: string;
    best_bid: string;
    avg_cost: string;
    size: string;
  }[];
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
  target_cities_providers?: Record<string, "noaa" | "open_meteo" | "kma" | "met_office">;
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
  asset: string | null;
  direction: string | null;
  condition_id: string | null;
  question: string | null;
  outcome: string | null;
  current_price: string | null;
  unrealized_pnl: string | null;
}

export interface CryptoAlphaConfigSection {
  min_edge_bps: number;
  max_position_pct: number;
  kelly_fraction: number;
  refresh_interval_secs: number;
  spot_refresh_interval_secs: number;
  history_refresh_interval_secs: number;
  iv_refresh_interval_secs: number;
  coingecko_api_key: string;
  exit_buffer_bps: number;
  capital_efficiency_threshold: number;
  drift_decay: number;
  max_spread_bps: number;
  relative_stop_loss_ratio: number;
  max_exposure_per_asset_pct: number;
  max_exposure_per_asset_direction_pct: number;
}

export interface CryptoCandidateDecisionEntry {
  recorded_at: string;
  asset: string;
  direction: string;
  action: string;
  reason: string;
  event_context_source: string | null;
  event_title: string | null;
  event_category: string | null;
  event_subtype: string | null;
  selected_question: string;
  replaced_question: string | null;
  selected_estimated_profit: string;
  replaced_estimated_profit: string | null;
  selected_efficiency: string;
  replaced_efficiency: string | null;
  selected_executable_profit_retention: string;
  replaced_executable_profit_retention: string | null;
  selected_executable_size_retention: string;
  replaced_executable_size_retention: string | null;
  selected_executable_quality_score: string;
  replaced_executable_quality_score: string | null;
  selected_executable_efficiency: string;
  replaced_executable_efficiency: string | null;
  selected_depth_buffer: string;
  replaced_depth_buffer: string | null;
}

export interface CryptoExitDecisionEntry {
  recorded_at: string;
  asset: string | null;
  reason: string;
  event_context_source: string | null;
  event_title: string | null;
  event_category: string | null;
  event_subtype: string | null;
  question: string;
  best_bid: string;
  avg_cost: string;
  size: string;
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

export function fetchCryptoAlphaConfig(): Promise<CryptoAlphaConfigSection> {
  return get("/api/config/crypto_alpha");
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

export function fetchCryptoCandidateDecisions(): Promise<CryptoCandidateDecisionEntry[]> {
  return get("/api/crypto/decisions");
}

export function fetchCryptoExitDecisions(): Promise<CryptoExitDecisionEntry[]> {
  return get("/api/crypto/exits");
}

export async function fetchMetrics(): Promise<string> {
  const res = await fetch(`${BASE}/metrics`);
  if (!res.ok) throw new Error(`metrics: ${res.status}`);
  return res.text();
}
