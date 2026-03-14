import type { SectionMeta } from "../api";

interface Props {
  title: string;
  section: string;
  data: Record<string, unknown>;
  meta?: SectionMeta;
}

const FIELD_HINTS: Record<string, Record<string, string>> = {
  strategy: {
    enabled: "启用的策略列表，如 weather, crypto, smart_money",
    scan_interval_ms: "策略扫描间隔（毫秒）",
    min_spread_bps: "最小买卖价差（基点），低于此值跳过",
    min_profit_usdc: "最小预期利润（USDC），低于此值不执行",
    max_trade_size_usdc: "单笔最大交易金额（USDC）",
    order_type: "订单类型：FOK（立即全部成交）或 GTC（挂单）",
    max_market_end_days: "只交易 N 天内到期的市场，留空不限",
  },
  risk: {
    max_position_per_market: "单市场最大仓位（USDC）",
    max_total_exposure: "所有仓位总敞口上限（USDC）",
    max_daily_loss: "单日最大亏损（USDC），超过暂停交易",
    circuit_breaker_loss: "熔断亏损阈值（USDC），触发后停止所有交易",
    circuit_breaker_consecutive_losses: "连续亏损次数触发熔断",
    max_slippage_bps: "最大滑点（基点）",
    min_order_usdc: "最小订单金额（USDC），低于此值跳过",
    min_profit_usdc: "最小利润要求（USDC）",
    max_exposure_per_strategy: "单策略最大敞口（USDC）",
    max_markets_per_strategy: "单策略最大持仓市场数",
  },
  market_filter: {
    min_liquidity: "最小市场流动性（USDC）",
    min_volume_24h: "最小 24h 交易量（USDC）",
    max_markets: "最大发现市场数",
    ws_max_instruments: "WebSocket 最大订阅数（建议 ≤ 500）",
    market_refresh_interval_secs: "市场刷新间隔（秒），0 = 不刷新",
  },
  weather: {
    min_edge_bps: "最小 edge（模型概率 - 市场价，基点）",
    max_spread_bps: "最大买卖价差（基点），超过则跳过",
    max_position_pct: "仓位占余额比例上限（0-1）",
    max_position_usdc: "单笔最大仓位（USDC）",
    kelly_fraction: "Kelly 公式分数上限（0-1）",
    forecast_error: "预报误差参数（温度/降水/降雪/风速的 σ 值）",
    refresh_interval_secs: "预报数据刷新间隔（秒）",
    exit_buffer_bps: "模型反转退出缓冲（基点），模型概率 < 买一价 - 缓冲时卖出",
    capital_efficiency_threshold: "资金效率退出阈值，买一价 ≥ 此值时卖出锁利",
    dynamic_sigma: "动态 σ：根据预报天数放大误差 σ×√天数",
    forecast_change_detection: "预报变化检测：仅在预报显著变化时交易",
    forecast_change_threshold: "变化阈值：预报变化需超过此倍数的 σ",
    max_entry_price: "最大入场价：只买低于此价的 token（高赔率策略）",
    profit_take_threshold: "止盈价：价格涨到此值自动卖出",
    noaa_user_agent: "NOAA API 的 User-Agent 头",
    target_cities: "目标城市列表，只扫描这些城市的天气市场",
  },
  crypto_alpha: {
    min_edge_bps: "最小 edge（GBM模型概率 - 市场价，基点）",
    max_position_pct: "仓位占余额比例上限（0-1）",
    kelly_fraction: "Kelly 公式分数上限（0-1）",
    refresh_interval_secs: "价格数据刷新间隔（秒）",
    coingecko_api_key: "CoinGecko Demo API Key（留空则禁用备用源）",
    exit_buffer_bps: "模型反转退出缓冲（基点）",
    capital_efficiency_threshold: "资金效率退出阈值（0-1）",
    drift_decay: "漂移衰减：0=风险中性(Black-Scholes)，1=完全历史漂移",
    max_spread_bps: "最大买卖价差（基点），超过则跳过",
    max_position_usdc: "单笔最大仓位（USDC）",
  },
  event_calendar: {
    enabled: "是否启用事件日历过滤",
    finnhub_api_key: "Finnhub API Key（美国宏观经济事件）",
    coinmarketcal_api_key: "CoinMarketCal API Key（加密货币事件）",
    refresh_interval_secs: "事件数据刷新间隔（秒）",
    pre_event_hours: "事件前 N 小时开始降低仓位",
    post_event_hours: "事件后 N 小时恢复正常",
    high_impact_multiplier: "高影响事件仓位乘数（0-1，如 0.25 = 降至 25%）",
    medium_impact_multiplier: "中影响事件仓位乘数（0-1）",
    low_impact_multiplier: "低影响事件仓位乘数（0-1）",
    static_events: "手动配置的静态事件列表（JSON）",
  },
  liquidity_rewards: {
    enabled: "是否启用流动性奖励做市",
    max_markets: "同时报价的最大市场数",
    max_position_per_market: "单市场最大仓位（USDC）",
    max_total_exposure: "所有奖励市场总敞口上限（USDC）",
    market_refresh_secs: "市场选择刷新间隔（秒）",
    quote_refresh_secs: "报价刷新间隔（秒）",
    spread_fraction: "使用奖励最大价差的比例（0-1，如 0.8 = 80%）",
    min_order_size: "最小订单金额（USDC）",
    inventory_skew_factor: "库存偏斜因子（0-1），持仓偏重时扩大价差",
    min_daily_rate: "最低日奖励率（USDC），低于此值不参与",
    requote_trigger_bps: "BBO 偏移触发重新报价的阈值（基点）",
    requote_cooldown_secs: "重新报价冷却时间（秒）",
    verify_scoring: "是否通过 CLOB API 验证订单计分",
    quote_yes: "是否在 YES 侧报价",
    quote_no: "是否在 NO 侧报价",
    fill_check_secs: "成交检测间隔（秒），0 = 禁用",
    order_depth_level: "报价深度层级，0=中间价，N=第 N 档",
    cancel_depth_level: "撤单深度层级，订单到达此档时撤单重挂",
    failed_cooldown_secs: "下单失败后冷却时间（秒）",
    market_mode: "市场模式：auto（自动）/ manual（手动）/ hybrid（混合）",
    manual_markets: "手动管理的市场列表（JSON）",
    allow_neg_risk: "是否允许 NegRisk 多结果市场",
  },
  smart_money: {
    wallets: "跟踪的钱包列表（JSON: address, label, weight）",
    follow_ratio: "跟单比例（0-1，如 0.1 = 跟踪仓位的 10%）",
    max_position_usdc: "单市场最大仓位（USDC）",
    poll_interval_secs: "Data API 轮询间隔（秒）",
    signal_ttl_secs: "信号有效期（秒），过期丢弃",
    exit_buffer_bps: "退出缓冲（基点）",
    capital_efficiency_threshold: "资金效率退出阈值（0-1）",
    onchain_enabled: "是否启用链上 Transfer 事件监控",
    onchain_poll_secs: "链上事件轮询间隔（秒）",
    auto_discover_enabled: "是否自动发现高收益钱包",
  },
};

export default function ConfigSection({ title, section, data, meta }: Props) {
  const hints = FIELD_HINTS[section] ?? {};

  return (
    <div className="card bg-base-200 shadow-sm">
      <div className="card-body p-4">
        <div className="mb-3 flex items-center justify-between">
          <h3 className="card-title text-base">{title}</h3>
          <span className="badge badge-outline">只读展示</span>
        </div>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          {Object.entries(data).map(([key, value]) => (
            <FieldDisplay
              key={key}
              section={section}
              fieldKey={key}
              value={value}
              meta={meta}
              hint={hints[key]}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

function riskBadgeClass(tier: "low" | "medium" | "high" | undefined) {
  switch (tier) {
    case "low":
      return "badge badge-success badge-outline";
    case "medium":
      return "badge badge-warning badge-outline";
    case "high":
      return "badge badge-error badge-outline";
    default:
      return "badge badge-ghost";
  }
}

function FieldDisplay({
  section,
  fieldKey,
  value,
  meta,
  hint,
}: {
  section: string;
  fieldKey: string;
  value: unknown;
  meta?: SectionMeta;
  hint?: string;
}) {
  const label = fieldKey.replace(/_/g, " ");

  if (typeof value === "boolean") {
    return (
      <div className="rounded-box border border-base-300 bg-base-100 p-3" title={hint}>
        <div className="text-xs opacity-70">{label}</div>
        {hint && <div className="mt-1 text-xs opacity-40">{hint}</div>}
        <div className="mt-2">
          <span className={`badge ${value ? "badge-success" : "badge-ghost"}`}>
            {value ? "启用" : "关闭"}
          </span>
        </div>
      </div>
    );
  }

  if (typeof value === "number" || typeof value === "string") {
    return (
      <div className="rounded-box border border-base-300 bg-base-100 p-3">
        <div className="text-xs opacity-70">{label}</div>
        {hint && <div className="mt-1 text-xs opacity-40">{hint}</div>}
        <div className="mt-2 font-mono text-sm break-all">{String(value)}</div>
      </div>
    );
  }

  if (Array.isArray(value)) {
    const isStringArray = value.every((v) => typeof v === "string");
    if (isStringArray && section === "weather" && fieldKey === "target_cities") {
      const selected = value as string[];
      const options = Array.isArray(meta?.target_cities_options)
        ? (meta?.target_cities_options as string[])
        : [];
      const supportedOptions = Array.isArray(meta?.supported_cities_options)
        ? (meta?.supported_cities_options as string[])
        : options;
      const riskTiers = meta?.target_cities_risk_tiers ?? {};
      const providers = meta?.target_cities_providers ?? {};
      const tradeEnabled = meta?.target_cities_trade_enabled ?? {};
      const settlementNotes = meta?.target_cities_settlement_notes ?? {};
      const validationStatus = meta?.target_cities_validation_status ?? {};
      const extraEdgeBps = meta?.target_cities_extra_edge_bps ?? {};
      const sigmaMultipliers = meta?.target_cities_sigma_multipliers ?? {};
      const counts = options.reduce(
        (acc, city) => {
          const tier = riskTiers[city] ?? "high";
          acc[tier] += 1;
          return acc;
        },
        { low: 0, medium: 0, high: 0 },
      );
      const auditOnlyCities = supportedOptions.filter((city) => !tradeEnabled[city]);

      return (
        <div className="rounded-box border border-base-300 bg-base-100 p-3 sm:col-span-2">
          <div className="text-xs opacity-70">{label}</div>
          {hint && <div className="mt-1 text-xs opacity-40">{hint}</div>}
          <div className="mt-2 flex flex-wrap gap-2 text-xs">
            <span className="badge badge-success badge-outline">低风险 {counts.low}</span>
            <span className="badge badge-warning badge-outline">中风险 {counts.medium}</span>
            <span className="badge badge-error badge-outline">高风险 {counts.high}</span>
            <span className="badge badge-ghost">
              sigma: low x{sigmaMultipliers.low ?? 1}, medium x{sigmaMultipliers.medium ?? 1}, high x{sigmaMultipliers.high ?? 1}
            </span>
          </div>
          <div className="mt-2 text-xs opacity-60">
            {selected.length === 0
              ? "当前为留空模式：允许所有当前可交易的天气城市。"
              : `当前选中 ${selected.length} 个城市。`}
          </div>
          <div className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-3">
            {options.map((city) => {
              const active = selected.length === 0 || selected.includes(city);
              const tier = riskTiers[city];
              const provider = providers[city];
              const settlementNote = settlementNotes[city];
              const validation = validationStatus[city];
              const extraEdge = extraEdgeBps[city] ?? 0;
              return (
                <div
                  key={city}
                  className={`rounded-btn border px-3 py-2 text-sm ${active ? "border-primary bg-primary/5" : "border-base-300 bg-base-100"}`}
                >
                  <div className="flex items-center justify-between gap-2">
                    <span>{city}</span>
                    <span className={riskBadgeClass(tier)}>
                      {tier === "low" ? "低" : tier === "medium" ? "中" : "高"}
                    </span>
                  </div>
                  <div className="mt-1 flex flex-wrap gap-1 text-[11px] opacity-70">
                    <span className="badge badge-ghost badge-xs">
                      {provider === "open_meteo" ? "Open-Meteo" : "NOAA"}
                    </span>
                    <span className="badge badge-success badge-outline badge-xs">可交易</span>
                    <span className={`badge badge-xs ${validation === "validated" ? "badge-info badge-outline" : "badge-warning badge-outline"}`}>
                      {validation === "validated" ? "已验证结算" : "默认保护"}
                    </span>
                    {extraEdge > 0 && (
                      <span className="badge badge-warning badge-outline badge-xs">
                        +{extraEdge}bps edge
                      </span>
                    )}
                    {active && <span className="badge badge-primary badge-outline badge-xs">已纳入</span>}
                  </div>
                  {settlementNote && <div className="mt-1 text-[11px] opacity-60">{settlementNote}</div>}
                </div>
              );
            })}
          </div>
          {auditOnlyCities.length > 0 && (
            <div className="mt-3 rounded-box border border-base-300 p-3">
              <div className="text-xs font-medium opacity-70">国际审计城市（暂不参与实盘）</div>
              <div className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-3">
                {auditOnlyCities.map((city) => {
                  const tier = riskTiers[city] ?? "high";
                  const provider = providers[city];
                  const settlementNote = settlementNotes[city];
                  const validation = validationStatus[city];
                  const extraEdge = extraEdgeBps[city] ?? 0;
                  return (
                    <div key={city} className="rounded-btn border border-dashed border-base-300 px-3 py-2 text-sm">
                      <div className="flex items-center justify-between gap-2">
                        <span>{city}</span>
                        <span className={riskBadgeClass(tier)}>
                          {tier === "low" ? "低" : tier === "medium" ? "中" : "高"}
                        </span>
                      </div>
                      <div className="mt-1 flex flex-wrap gap-1 text-[11px] opacity-70">
                        <span className="badge badge-ghost badge-xs">
                          {provider === "open_meteo" ? "Open-Meteo" : "NOAA"}
                        </span>
                        <span className="badge badge-warning badge-outline badge-xs">audit-only</span>
                        <span className={`badge badge-xs ${validation === "validated" ? "badge-info badge-outline" : "badge-warning badge-outline"}`}>
                          {validation === "validated" ? "已验证结算" : "默认保护"}
                        </span>
                        {extraEdge > 0 && (
                          <span className="badge badge-warning badge-outline badge-xs">
                            +{extraEdge}bps edge
                          </span>
                        )}
                      </div>
                      {settlementNote && (
                        <div className="mt-1 text-[11px] opacity-60">结算站点: {settlementNote}</div>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          )}
        </div>
      );
    }

    return (
      <div className="rounded-box border border-base-300 bg-base-100 p-3 sm:col-span-2">
        <div className="text-xs opacity-70">{label}</div>
        {hint && <div className="mt-1 text-xs opacity-40">{hint}</div>}
        <div className="mt-2 font-mono text-sm break-all">
          {isStringArray ? (value as string[]).join(", ") || "[]" : JSON.stringify(value, null, 2)}
        </div>
      </div>
    );
  }

  return (
    <div className="rounded-box border border-base-300 bg-base-100 p-3 sm:col-span-2">
      <div className="text-xs opacity-70">{label}</div>
      {hint && <div className="mt-1 text-xs opacity-40">{hint}</div>}
      <pre className="mt-2 overflow-x-auto whitespace-pre-wrap break-all rounded bg-base-200 p-2 text-xs">
        {JSON.stringify(value, null, 2)}
      </pre>
    </div>
  );
}
