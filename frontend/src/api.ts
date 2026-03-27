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
  total_wallet_positions_market_value_bid?: string;
  total_wallet_positions_market_value_mid?: string;
  total_wallet_portfolio_value_bid?: string;
  total_wallet_portfolio_value_mid?: string;
  strategy_financials?: Record<string, {
    wallet_balance: string;
    positions_market_value: string;
    portfolio_value: string;
    realized_pnl: string;
  }>;
  weather_rejection_summary?: {
    retained_window_minutes: number;
    retained_count: number;
    unsupported_city_count?: number;
    retained_top: { label: string; count: number }[];
    recent_1h: {
      count: number;
      unsupported_city_count?: number;
      top_reasons: { label: string; count: number }[];
      top_reason: { label: string; count: number } | null;
      top_spread_cities: { label: string; count: number }[];
      top_price_cities: { label: string; count: number }[];
    };
    recent_6h: {
      count: number;
      unsupported_city_count?: number;
      top_reasons: { label: string; count: number }[];
      top_reason: { label: string; count: number } | null;
      top_spread_cities: { label: string; count: number }[];
      top_price_cities: { label: string; count: number }[];
    };
  };
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
  crypto_cooldown_summary?: {
    active_count: number;
    buckets: {
      kind: string;
      asset: string;
      event_subtype: string;
      shape: string;
      scope_label: string;
      trigger_count: number;
      current_count: number;
      triggered_at: string;
      post_trigger_bad_exit_count: number;
      remaining_secs: number;
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
    selector_shape?: string;
    source_bucket?: string;
    scope_label: string;
    source_reason: string;
    rationale: string;
    support_count?: number;
  }[];
  crypto_override_patch_preview?: {
    supported_row_count: number;
    unsupported_suggestion_count: number;
    unsupported_suggestions: string[];
    rows: {
      scope_label: string;
      selector_asset_class: string;
      selector_event_subtype: string;
      selector_shape: string;
      source_bucket: string;
      resolution_bucket: string;
      horizon: string;
      market_type: string;
      fields: {
        target_field: string;
        direction: string;
        source_reason: string;
        support_count: number;
        preview_value: string;
      }[];
    }[];
    toml: string;
  };
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
    selector_shape?: string;
    source_bucket?: string;
    scope_label: string;
    source_reason: string;
    rationale: string;
    support_count?: number;
  }[];
  crypto_post_entry_override_patch_preview?: {
    supported_row_count: number;
    unsupported_suggestion_count: number;
    unsupported_suggestions: string[];
    rows: {
      scope_label: string;
      selector_asset_class: string;
      selector_event_subtype: string;
      selector_shape: string;
      source_bucket: string;
      resolution_bucket: string;
      horizon: string;
      market_type: string;
      fields: {
        target_field: string;
        direction: string;
        source_reason: string;
        support_count: number;
        preview_value: string;
      }[];
    }[];
    toml: string;
  };
  crypto_override_patch_export_audit?: {
    recorded_at: string;
    mode: string;
    format: string;
    filename: string;
    export_sha: string;
    scope_label?: string | null;
  }[];
  crypto_auto_patch_effectiveness_summary?: {
    recent_count: number;
    relax_pressure_summary?: {
      leader_label: string;
      same_day_count: number;
      next_day_count: number;
      mixed_count: number;
      unknown_count: number;
      same_day_pressure_score: number;
      next_day_pressure_score: number;
    };
    priority_bucket_summary?: {
      row_count: number;
      leader_scope_label: string;
      leader_label: string;
      leader_recommended_action: "hold" | "observe" | "continue_tighten" | "consider_relax";
      leader_action_label: string;
      leader_field_action_label: string;
      leader_target_fields: string[];
      subtype_focus_label: string;
      subtype_focus_action_label: string;
      subtype_focus_summary_label: string;
      subtype_focus_field_summary_label: string;
      subtype_focus_event_subtype: string;
      subtype_focus_scope_labels: string[];
      subtype_focus_recommended_action: "hold" | "observe" | "continue_tighten" | "consider_relax";
      subtype_focus_target_fields: string[];
      asset_focus_label: string;
      asset_focus_action_label: string;
      asset_focus_summary_label: string;
      asset_focus_field_summary_label: string;
      asset_focus_asset: string;
      asset_focus_scope_labels: string[];
      asset_focus_recommended_action: "hold" | "observe" | "continue_tighten" | "consider_relax";
      asset_focus_target_fields: string[];
      rows: {
        scope_label: string;
        resolution_bucket: string;
        asset_class: string;
        event_subtype: string;
        shape: string;
        priority_score: number;
        cooldown_severity_score: number;
        window_pressure_score: number;
        long_window_pressure_score: number;
        priority_reason_label: string;
      }[];
    };
    patches: {
      created_at: string;
      runtime_applied_at: string;
      mode: string;
      filename: string;
      export_sha: string;
      scope_labels: string[];
      post_apply_bad_exit_count: number;
      post_apply_realized_pnl: string;
      current_open_positions: number;
      current_open_pnl_bid: string;
      outcome: "effective" | "observe" | "retain_or_tighten";
      effective_streak: number;
      recommended_action: "hold" | "observe" | "continue_tighten" | "consider_relax";
      blocked_by_long_window_relax_guard?: boolean;
      current_priority_score: number;
      current_cooldown_severity_score: number;
      current_window_pressure_score: number;
      current_long_window_pressure_score: number;
      priority_reason_label: string;
      relax_uses_conservative_post_entry: boolean;
      relax_uses_fallback_post_entry: boolean;
      relax_uses_entry_fallback: boolean;
    }[];
    long_window_relax_guard_summary?: {
      blocked_count: number;
      continuing_pressure_count: number;
      stabilizing_count: number;
      leader_label: string;
      cadence_blocked_count: number;
      rows: {
        runtime_applied_at: string;
        scope_labels: string[];
        effective_streak: number;
        current_long_window_pressure_score: number;
        current_open_positions: number;
        current_open_pnl_bid: string;
        post_apply_bad_exit_count: number;
        post_apply_realized_pnl: string;
        effect_label: string;
        window_effect_label: string;
        note: string;
      }[];
    };
  };
  crypto_bucket_window_summary?: {
    row_count: number;
    rows: {
      window_label: string;
      resolution_bucket: string;
      shape: string;
      asset_class: string;
      trade_count: number;
      realized_pnl: string;
      open_positions: number;
      open_pnl_bid: string;
      bad_exit_count: number;
    }[];
  };
  crypto_subtype_window_summary?: {
    row_count: number;
    rows: {
      window_label: string;
      resolution_bucket: string;
      shape: string;
      asset_class: string;
      event_subtype: string;
      trade_count: number;
      realized_pnl: string;
      open_positions: number;
      open_pnl_bid: string;
      bad_exit_count: number;
    }[];
  };
  crypto_asset_long_window_summary?: {
    row_count: number;
    leader_asset?: string | null;
    leader_label: string;
    leader_action_label: string;
    rows: {
      asset: string;
      trade_count: number;
      realized_pnl: string;
      open_positions: number;
      open_pnl_bid: string;
      bad_exit_count: number;
      pressure_score: number;
    }[];
  };
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
  smart_money_route_summary?: {
    configured_routes: number;
    route_mismatch_rejections: number;
  };
  smart_money_exit_summary?: {
    total_exits: number;
    reason_counts: { label: string; count: number }[];
  };
  smart_money_leader_discovery_summary?: {
    candidate_count: number;
    top_candidates: SmartMoneyLeaderCandidate[];
  };
  smart_money_leader_attribution_summary?: {
    top_leaders: {
      leader: string;
      signals: number;
      accepted: number;
      rejected: number;
      accept_rate: number;
    }[];
  };
  smart_money_leader_pnl_attribution_summary?: {
    top_leaders: {
      leader: string;
      estimated_open_size: string;
      estimated_exited_size: string;
      estimated_realized_pnl: string;
      estimated_exit_count: number;
    }[];
  };
  smart_money_trade_attribution_summary?: {
    top_leaders: {
      leader: string;
      actual_filled_size: string;
      actual_fee: string;
      actual_realized_profit: string;
      trade_count: number;
    }[];
  };
  smart_money_leader_health_summary?: {
    top_leaders: {
      leader: string;
      signals: number;
      accepted: number;
      rejected: number;
      accept_rate: number;
      estimated_realized_pnl: string;
      actual_realized_profit: string;
      trade_count: number;
      suggested_action: string;
      rationale: string;
    }[];
  };
  smart_money_review_queue_summary?: {
    pending_count: number;
    action_counts: { label: string; count: number }[];
    top_actions: {
      leader: string;
      address: string | null;
      label: string | null;
      suggested_action: string;
      current_state: string;
      actionable: boolean;
      rationale: string;
    }[];
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
    leader_addresses: string[];
    leader_labels: string[];
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
    estimated_profit: string;
    attributed_leaders: {
      leader: string;
      estimated_size: string;
      estimated_profit: string;
    }[];
  }[];
  accounts: AccountStatusEntry[];
}

export interface SmartMoneyLeaderCandidate {
  address: string;
  label: string;
  source_tags: string[];
  first_seen_at: string;
  last_seen_at: string;
  leaderboard_rank: number | null;
  leaderboard_volume: string;
  leaderboard_pnl: string;
  open_positions_count: number;
  open_notional: string;
  closed_positions_count: number;
  closed_total_bought: string;
  closed_realized_pnl: string;
  sampled_markets: number;
  market_position_count: number;
  holder_position_count: number;
  activity_volume: string;
  activity_pnl: string;
  verified: boolean;
  discovery_score: string;
  promoted: boolean;
  blocked: boolean;
  degrade_multiplier?: string | null;
  route_categories?: string[];
  route_question_keywords?: string[];
  route_event_title_keywords?: string[];
  metadata?: Record<string, unknown> | null;
  updated_at: string;
}

export interface PromoteSmartMoneyLeaderResponse {
  candidate: SmartMoneyLeaderCandidate;
  promoted: boolean;
  wallets_toml: string;
  auto_discover_candidate: string;
  note: string;
}

export interface BlockSmartMoneyLeaderResponse {
  candidate: SmartMoneyLeaderCandidate;
  blocked: boolean;
  note: string;
}

export interface DegradeSmartMoneyLeaderResponse {
  candidate: SmartMoneyLeaderCandidate;
  degraded: boolean;
  multiplier: string;
  note: string;
}

export interface RestoreSmartMoneyLeaderResponse {
  candidate: SmartMoneyLeaderCandidate;
  restored: boolean;
  note: string;
}

export interface ApplySmartMoneyLeaderRouteTemplateResponse {
  candidate: SmartMoneyLeaderCandidate;
  template: string;
  note: string;
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
  bid_price: string | null;
  mid_price: string | null;
  unrealized_pnl_bid: string | null;
  unrealized_pnl_mid: string | null;
  resolution_bucket: string | null;
  is_legacy: boolean;
  current_price: string | null;
  unrealized_pnl: string | null;
}

export interface CryptoTradeEntry {
  trade_id: string;
  opportunity_id: string | null;
  order_id: string | null;
  token_id: string;
  side: string;
  price: string;
  size: string;
  filled_size: string | null;
  fee: string | null;
  tx_type: string;
  tx_hash: string | null;
  status: string;
  created_at: string;
  strategy: string | null;
  condition_id: string | null;
  question: string | null;
  account_name: string | null;
  proxy_wallet: string | null;
  opportunity_status: string | null;
  estimated_profit: string | null;
  actual_profit: string | null;
  detected_at: string | null;
  executed_at: string | null;
  smart_money_attribution?: {
    leader: string;
    estimated_size: string;
    estimated_profit: string;
  }[] | null;
  smart_money_trade_attribution?: {
    leader: string;
    actual_filled_size: string;
    actual_fee: string;
    actual_realized_profit: string;
  }[] | null;
}

export interface SmartMoneyAuditEntry {
  created_at: string;
  changed_by: string;
  version: number;
  blocked_wallet_count: number;
  degraded_wallet_count: number;
  wallet_count: number;
  auto_discover_candidate_count: number;
  route_count: number;
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

export interface CryptoOverridePatchExport {
  mode: string;
  toml: string;
  filename?: string;
  scope_label?: string;
  focus_label?: string;
  export_sha?: string;
  generated_at?: string;
  selected_bucket_count?: number;
  entry_row_count?: number;
  post_entry_row_count?: number;
  selected_field_count?: number;
  field_level?: boolean;
  selected_target_fields?: string[];
  uses_conservative_post_entry?: boolean;
  uses_fallback_post_entry?: boolean;
  uses_entry_fallback?: boolean;
  recommended_action?: string;
  action_label?: string;
  note?: string;
}

export interface CryptoOverridePatchAuditEntry {
  created_at: string;
  changed_by: string;
  version: number;
  action: string;
  mode: string;
  filename: string;
  export_sha: string;
  scope_label?: string | null;
  generated_at?: string | null;
  runtime_applied: boolean;
  runtime_applied_at?: string | null;
  uses_conservative_post_entry: boolean;
  uses_fallback_post_entry: boolean;
  uses_entry_fallback: boolean;
}

export interface ApplyCryptoOverridePatchResponse {
  applied: boolean;
  action: string;
  runtime_applied: boolean;
  filename: string;
  export_sha: string;
  note: string;
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

export function fetchSmartMoneyLeaders(): Promise<SmartMoneyLeaderCandidate[]> {
  return get("/api/smart-money/leaders");
}

export async function promoteSmartMoneyLeader(address: string): Promise<PromoteSmartMoneyLeaderResponse> {
  const res = await fetch(`${BASE}/api/smart-money/leaders/promote`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ address }),
  });
  if (!res.ok) {
    const body = await res.text();
    throw new Error(`${res.status}: ${body}`);
  }
  return res.json();
}

export async function blockSmartMoneyLeader(address: string): Promise<BlockSmartMoneyLeaderResponse> {
  const res = await fetch(`${BASE}/api/smart-money/leaders/block`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ address }),
  });
  if (!res.ok) {
    const body = await res.text();
    throw new Error(`${res.status}: ${body}`);
  }
  return res.json();
}

export async function degradeSmartMoneyLeader(address: string): Promise<DegradeSmartMoneyLeaderResponse> {
  const res = await fetch(`${BASE}/api/smart-money/leaders/degrade`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ address }),
  });
  if (!res.ok) {
    const body = await res.text();
    throw new Error(`${res.status}: ${body}`);
  }
  return res.json();
}

export async function restoreSmartMoneyLeader(address: string): Promise<RestoreSmartMoneyLeaderResponse> {
  const res = await fetch(`${BASE}/api/smart-money/leaders/restore`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ address }),
  });
  if (!res.ok) {
    const body = await res.text();
    throw new Error(`${res.status}: ${body}`);
  }
  return res.json();
}

export async function applySmartMoneyLeaderRouteTemplate(
  address: string,
  template: string,
): Promise<ApplySmartMoneyLeaderRouteTemplateResponse> {
  const res = await fetch(`${BASE}/api/smart-money/leaders/route-template`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ address, template }),
  });
  if (!res.ok) {
    const body = await res.text();
    throw new Error(`${res.status}: ${body}`);
  }
  return res.json();
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

export function fetchCryptoTrades(limit = 200): Promise<CryptoTradeEntry[]> {
  return get(`/api/crypto/trades?limit=${limit}`);
}

export function fetchCryptoOverridePatch(
  mode:
    | "full"
    | "cooldown_priority"
    | "relax_candidate"
    | "subtype_focus"
    | "asset_focus"
    | "asset_long_window_focus" = "full",
): Promise<CryptoOverridePatchExport> {
  return get(`/api/crypto/override-patch?mode=${encodeURIComponent(mode)}`);
}

export function fetchSelectedCryptoOverridePatch(
  bucket: string,
  shape: "range" | "directional",
): Promise<CryptoOverridePatchExport> {
  return get(
    `/api/crypto/override-patch?mode=selected&bucket=${encodeURIComponent(bucket)}&shape=${encodeURIComponent(shape)}`,
  );
}

export function fetchCryptoOverridePatchAudit(limit = 50): Promise<CryptoOverridePatchAuditEntry[]> {
  return get(`/api/crypto/override-patch/audit?limit=${limit}`);
}

export async function applyCryptoOverridePatch(payload: {
  action?: "review" | "approve" | "apply_runtime";
  mode: string;
  filename: string;
  export_sha: string;
  toml: string;
  scope_label?: string | null;
  generated_at?: string | null;
  uses_conservative_post_entry?: boolean;
  uses_fallback_post_entry?: boolean;
  uses_entry_fallback?: boolean;
}): Promise<ApplyCryptoOverridePatchResponse> {
  const res = await fetch(`${BASE}/api/crypto/override-patch/apply`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  if (!res.ok) {
    const body = await res.text();
    throw new Error(`${res.status}: ${body}`);
  }
  return res.json();
}

export function cryptoOverridePatchDownloadPath(
  mode:
    | "full"
    | "cooldown_priority"
    | "relax_candidate"
    | "selected"
    | "subtype_focus"
    | "asset_focus"
    | "asset_long_window_focus",
  options?: { bucket?: string; shape?: "range" | "directional" },
): string {
  const params = new URLSearchParams({ mode, format: "toml" });
  if (options?.bucket) {
    params.set("bucket", options.bucket);
  }
  if (options?.shape) {
    params.set("shape", options.shape);
  }
  return `${BASE}/api/crypto/override-patch?${params.toString()}`;
}

export function fetchStrategyTrades(strategy: string, limit = 200): Promise<CryptoTradeEntry[]> {
  return get(`/api/trades?strategy=${encodeURIComponent(strategy)}&limit=${limit}`);
}

export function fetchSmartMoneyAudit(limit = 50): Promise<SmartMoneyAuditEntry[]> {
  return get(`/api/smart-money/audit?limit=${limit}`);
}

export async function fetchMetrics(): Promise<string> {
  const res = await fetch(`${BASE}/metrics`);
  if (!res.ok) throw new Error(`metrics: ${res.status}`);
  return res.text();
}
