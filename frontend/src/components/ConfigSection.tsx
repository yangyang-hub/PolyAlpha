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
    max_slippage_bps: "最大滑点（基点）：执行 freshness 允许的向上重报价预算",
    min_profit_retention_ratio: "最小利润保真比例：重报价后需保留的原始预期利润比例",
    min_size_retention_ratio: "最小数量保真比例：执行 freshness 缩量后需保留的原始下单量比例",
    execution_quality_profit_weight: "执行质量分数里的利润保真权重",
    execution_quality_size_weight: "执行质量分数里的数量保真权重",
    execution_quality_slippage_weight: "执行质量分数里的滑点质量权重",
    min_order_usdc: "最小订单金额（USDC），低于此值跳过",
    min_profit_usdc: "最小利润要求（USDC）",
    max_exposure_per_strategy: "单策略最大敞口（USDC）",
    max_markets_per_strategy: "单策略最大持仓市场数",
  },
  market_filter: {
    min_liquidity: "最小市场流动性（USDC）",
    min_volume_24h: "最小 24h 交易量（USDC）",
    max_markets: "最大发现市场数",
    ws_max_instruments: "WebSocket 最大订阅数（建议 ≤ 350）",
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
    relative_stop_loss_ratio: "相对止损比率：买入后若 bid 跌破持仓成本 × 此比率则卖出",
    noaa_user_agent: "NOAA API 的 User-Agent 头",
    kma_api_key: "KMA API Hub Key（首尔/KMA 审计与回放）",
    met_office_api_key: "Met Office Weather DataHub Key（伦敦/Met Office 审计与回放）",
    met_office_obs_api_key: "Met Office Land Observations Key（伦敦实际值审计）",
    target_cities: "目标城市列表，只扫描这些城市的天气市场。",
  },
  crypto_alpha: {
    min_edge_bps: "最小 edge（GBM模型概率 - 市场价，基点）",
    max_position_pct: "仓位占余额比例上限（0-1）",
    kelly_fraction: "Kelly 公式分数上限（0-1）",
    refresh_interval_secs: "兼容旧配置的共享刷新间隔（秒）",
    spot_refresh_interval_secs: "现价刷新间隔（秒）",
    history_refresh_interval_secs: "30日历史收盘价刷新间隔（秒）",
    iv_refresh_interval_secs: "隐含波动率刷新间隔（秒）",
    coingecko_api_key: "CoinGecko Demo API Key（留空则禁用备用源）",
    min_entry_depth_ratio: "最小入场深度倍数：盘口可成交深度 / 目标下单量",
    gate_scale_feedback_lookback: "自适应预缩量反馈查看的最近 gate_scale 条数",
    gate_scale_feedback_trigger_count: "触发自适应预缩量反馈所需的最少 gate_scale 次数",
    gate_scale_feedback_step_multiplier: "每一档 gate_scale 反馈对目标下单量施加的乘数",
    gate_scale_feedback_max_steps: "自适应预缩量反馈最多叠加的档位数",
    discovery_search_terms: "额外加密市场搜索词：在内置 crypto 发现词表基础上追加",
    exit_buffer_bps: "模型反转退出缓冲（基点）",
    capital_efficiency_threshold: "资金效率退出阈值（0-1）",
    drift_decay: "漂移衰减：0=风险中性(Black-Scholes)，1=完全历史漂移",
    max_spread_bps: "最大买卖价差（基点），超过则跳过",
    relative_stop_loss_ratio: "相对止损比率：bid 跌破持仓成本 × 此比率时退出",
    max_exposure_per_asset_pct: "单资产总敞口上限，占余额比例（0-1）",
    max_exposure_per_asset_direction_pct: "单资产单方向敞口上限，占余额比例（0-1）",
    low_event_min_edge_multiplier: "低影响事件最小 edge 乘数",
    medium_event_min_edge_multiplier: "中影响事件最小 edge 乘数（兼容旧 event_min_edge_multiplier）",
    high_event_min_edge_multiplier: "高影响事件最小 edge 乘数",
    low_event_max_spread_multiplier: "低影响事件最大价差乘数",
    medium_event_max_spread_multiplier: "中影响事件最大价差乘数（兼容旧 event_max_spread_multiplier）",
    high_event_max_spread_multiplier: "高影响事件最大价差乘数",
    low_event_sigma_multiplier: "低影响事件 sigma 乘数",
    medium_event_sigma_multiplier: "中影响事件 sigma 乘数",
    high_event_sigma_multiplier: "高影响事件 sigma 乘数",
    macro_event_sigma_multiplier: "宏观事件额外 sigma 乘数",
    crypto_event_sigma_multiplier: "加密事件额外 sigma 乘数",
    low_event_size_multiplier: "低影响事件仓位乘数",
    medium_event_size_multiplier: "中影响事件仓位乘数",
    high_event_size_multiplier: "高影响事件仓位乘数",
    macro_event_size_multiplier: "宏观事件额外仓位乘数",
    crypto_event_size_multiplier: "加密事件额外仓位乘数",
    btc_probability_calibration: "BTC 概率校准收缩系数",
    eth_probability_calibration: "ETH 概率校准收缩系数",
    alt_probability_calibration: "其他币种概率校准收缩系数",
    binary_probability_calibration: "二元市场概率校准收缩系数",
    range_probability_calibration: "区间市场概率校准收缩系数",
    override_probability_blend: "运行时 override 概率混合系数",
    override_probability_max_delta_bps: "运行时 override 概率最大偏移(bps)",
    override_multiplier_blend: "运行时 override 乘数混合系数",
    override_multiplier_max_delta_bps: "运行时 override 乘数最大偏移(bps)",
    calibration_overrides: "表驱动校准覆盖：按 asset/asset_class/horizon/market_type/event_subtype 精细覆盖默认校准",
    short_horizon_max_days: "短期期限桶最大天数",
    medium_horizon_max_days: "中期期限桶最大天数",
    max_entry_days: "新开仓允许的最大到期天数",
    same_day_probability_calibration: "当日期限概率校准收缩系数",
    short_horizon_probability_calibration: "短期期限概率校准收缩系数",
    medium_horizon_probability_calibration: "中期期限概率校准收缩系数",
    same_day_execution_quality_profit_weight_multiplier:
      "当日期限执行质量里的利润保真权重乘数",
    same_day_execution_quality_size_weight_multiplier:
      "当日期限执行质量里的数量保真权重乘数",
    same_day_execution_quality_slippage_weight_multiplier:
      "当日期限执行质量里的滑点质量权重乘数",
    short_execution_quality_profit_weight_multiplier:
      "次日期限执行质量里的利润保真权重乘数",
    short_execution_quality_size_weight_multiplier:
      "次日期限执行质量里的数量保真权重乘数",
    short_execution_quality_slippage_weight_multiplier:
      "次日期限执行质量里的滑点质量权重乘数",
    same_day_size_multiplier: "当日期限仓位乘数",
    short_horizon_size_multiplier: "短期期限仓位乘数",
    medium_horizon_size_multiplier: "中期期限仓位乘数",
    same_day_min_edge_multiplier: "当日期限最小 edge 乘数",
    short_horizon_min_edge_multiplier: "短期期限最小 edge 乘数",
    medium_horizon_min_edge_multiplier: "中期期限最小 edge 乘数",
    same_day_max_spread_multiplier: "当日期限最大价差乘数",
    short_horizon_max_spread_multiplier: "短期期限最大价差乘数",
    medium_horizon_max_spread_multiplier: "中期期限最大价差乘数",
    same_day_capital_efficiency_threshold: "当日期限资金效率止盈阈值",
    short_horizon_capital_efficiency_threshold: "短期期限资金效率止盈阈值",
    medium_horizon_capital_efficiency_threshold: "中期期限资金效率止盈阈值",
    same_day_exit_buffer_multiplier: "当日期限模型反转退出缓冲乘数",
    short_horizon_exit_buffer_multiplier: "短期期限模型反转退出缓冲乘数",
    medium_horizon_exit_buffer_multiplier: "中期期限模型反转退出缓冲乘数",
    hold_min_edge_bps: "持仓继续保留所需的最小 edge（基点）",
    same_day_hold_edge_multiplier: "当日期限持仓最小 edge 乘数",
    short_horizon_hold_edge_multiplier: "短期期限持仓最小 edge 乘数",
    medium_horizon_hold_edge_multiplier: "中期期限持仓最小 edge 乘数",
    edge_decay_exit_fraction: "edge 衰减退出的基础减仓比例",
    edge_decay_exit_fraction_step: "每多一次连续确认时增加的减仓比例",
    edge_decay_moderate_gap_bps: "进入中度 edge 衰减档位所需的额外薄 edge 基点",
    edge_decay_severe_gap_bps: "进入重度 edge 衰减档位所需的额外薄 edge 基点",
    edge_decay_moderate_exit_multiplier: "中度 edge 衰减档位的减仓比例乘数",
    edge_decay_severe_exit_multiplier: "重度 edge 衰减档位的减仓比例乘数",
    edge_decay_moderate_cooldown_multiplier: "中度 edge 衰减档位的冷却乘数",
    edge_decay_severe_cooldown_multiplier: "重度 edge 衰减档位的冷却乘数",
    same_day_edge_decay_exit_multiplier: "当日期限 edge 衰减减仓比例乘数",
    short_horizon_edge_decay_exit_multiplier: "短期期限 edge 衰减减仓比例乘数",
    medium_horizon_edge_decay_exit_multiplier: "中期期限 edge 衰减减仓比例乘数",
    edge_decay_cooldown_secs: "同一 token 的 edge 衰减退出冷却秒数",
    edge_decay_confirmation_scans: "edge 衰减退出所需的连续确认次数",
    same_day_edge_decay_confirmation_scans: "当日期限 edge 衰减确认次数",
    short_horizon_edge_decay_confirmation_scans: "短期期限 edge 衰减确认次数",
    medium_horizon_edge_decay_confirmation_scans: "中期期限 edge 衰减确认次数",
    edge_decay_moderate_confirmation_scan_multiplier: "中度 edge 衰减档位的确认次数乘数",
    edge_decay_severe_confirmation_scan_multiplier: "重度 edge 衰减档位的确认次数乘数",
    edge_decay_confirmation_window_secs: "连续确认之间允许的最大间隔秒数",
    same_day_edge_decay_confirmation_window_multiplier: "当日期限连续确认窗口乘数",
    short_horizon_edge_decay_confirmation_window_multiplier: "短期期限连续确认窗口乘数",
    medium_horizon_edge_decay_confirmation_window_multiplier: "中期期限连续确认窗口乘数",
    edge_decay_moderate_confirmation_window_multiplier: "中度 edge 衰减档位的确认窗口乘数",
    edge_decay_severe_confirmation_window_multiplier: "重度 edge 衰减档位的确认窗口乘数",
    same_day_edge_decay_cooldown_multiplier: "当日期限 edge 衰减冷却乘数",
    short_horizon_edge_decay_cooldown_multiplier: "短期期限 edge 衰减冷却乘数",
    medium_horizon_edge_decay_cooldown_multiplier: "中期期限 edge 衰减冷却乘数",
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
    auto_discover_candidates: "自动发现候选钱包列表（JSON）",
    auto_discover_interval_secs: "自动发现复评间隔（秒）",
    min_wallet_score: "自动发现最低钱包评分阈值",
    min_wallet_volume_usdc: "信任钱包 profile score 前要求的最小累计成交量（USDC）",
    max_wallets: "最大跟踪钱包数量",
    wallet_profile_blend: "profile score 对基础钱包权重的影响强度",
    wallet_signal_bonus_per_event: "每个近期有效信号给动态权重增加的奖励",
    wallet_signal_bonus_cap: "近期信号奖励的最大累计值",
    wallet_underperform_decay_step: "钱包低于最低评分时的固定降权步长",
    wallet_min_effective_weight: "动态钱包权重下限",
    wallet_max_effective_weight: "动态钱包权重上限",
    wallet_signal_lookback_secs: "统计近期钱包信号活跃度的回看窗口（秒）",
    min_signal_notional_usdc: "最小跟单信号名义金额（USDC）",
    min_signal_delta_shares: "最小仓位变化股数",
    min_wallet_weight: "允许信号的最低钱包权重",
    min_consensus_wallets: "允许开仓的最少共识钱包数",
    max_signal_age_secs: "最大信号年龄（秒），更老的信号直接丢弃",
    max_entry_price: "最高允许跟单入场价",
    max_spread_bps: "允许的最大买一卖一价差（基点）",
    min_top_level_depth_usdc: "买一档最小可吃深度（USDC）",
    min_market_liquidity: "最小市场流动性（USD）",
    confirm_onchain_with_data_api: "链上信号是否必须由下一次 Data API 快照确认",
    dedup_window_secs: "同钱包同 token 重复信号去重窗口（秒）",
    consensus_bonus_per_wallet: "每多一个同向 leader 给 sizing 增加的加成",
    consensus_bonus_cap: "共识加成上限",
    freshness_half_life_secs: "信号过期时 sizing 衰减半衰期（秒）",
    leader_delta_ratio_floor: "leader 小幅加仓时仍保留的最小 sizing 比例",
    position_concentration_soft_cap_usdc: "已有仓位超过该名义规模后开始缩小新单",
    position_concentration_min_multiplier: "集中度惩罚的最小保留倍率",
    leader_exit_min_delta_ratio: "leader 部分减仓低于该比例时不跟随退出",
    max_hold_secs: "跟单持仓最长保留时间（秒）",
    profit_protect_min_gain_bps: "达到该盈利幅度后启用 profit-protect",
    profit_protect_drawdown_bps: "profit-protect 激活后允许从峰值回撤的幅度",
    max_drawdown_bps: "相对成本的最大容忍回撤",
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
        {section === "crypto_alpha" && <CryptoOverrideCoverageSummary data={data} />}
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

type CalibrationOverrideRow = {
  asset?: string;
  asset_class?: string;
  horizon?: string;
  market_type?: string;
  event_subtype?: string;
  probability_calibration?: number | string | null;
  sigma_multiplier?: number | string | null;
  size_multiplier?: number | string | null;
  depth_ratio_multiplier?: number | string | null;
  min_edge_multiplier?: number | string | null;
  max_spread_multiplier?: number | string | null;
  hold_edge_multiplier?: number | string | null;
  edge_decay_exit_multiplier?: number | string | null;
  edge_decay_confirmation_scan_multiplier?: number | string | null;
  edge_decay_confirmation_window_multiplier?: number | string | null;
  edge_decay_cooldown_multiplier?: number | string | null;
  capital_efficiency_multiplier?: number | string | null;
  model_reversal_buffer_multiplier?: number | string | null;
  profit_retention_multiplier?: number | string | null;
  slippage_multiplier?: number | string | null;
  size_retention_multiplier?: number | string | null;
};

function isCalibrationOverrideRow(value: unknown): value is CalibrationOverrideRow {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function renderCalibrationCell(value: number | string | null | undefined) {
  if (value === null || value === undefined || value === "") {
    return "—";
  }
  return String(value);
}

function CalibrationScopeBadge({ value }: { value: string }) {
  const isWildcard = value === "*" || value === "any";
  return (
    <span
      className={`badge badge-sm ${isWildcard ? "badge-warning badge-outline" : "badge-ghost"}`}
    >
      {value}
    </span>
  );
}

function calibrationScopeScore(row: CalibrationOverrideRow) {
  const asset = row.asset || "*";
  const assetClass = row.asset_class || "any";
  const horizon = row.horizon || "any";
  const marketType = row.market_type || "any";
  const eventSubtype = row.event_subtype || "any";
  const assetScore = asset === "*" ? 0 : 1;
  const assetClassScore = assetClass === "any" ? 0 : 1;
  const horizonScore = horizon === "any" ? 0 : horizon === "long" ? 1 : 2;
  const marketTypeScore = marketType === "any" ? 0 : 1;
  const eventSubtypeScore = eventSubtype === "any" ? 0 : 1;
  return assetScore * 1000 + assetClassScore * 100 + horizonScore * 10 + marketTypeScore + eventSubtypeScore;
}

function calibrationMarketTypeGroupLabel(value: string) {
  switch (value) {
    case "binary":
      return "Binary";
    case "range":
      return "Range";
    case "any":
      return "Any";
    default:
      return value;
  }
}

function CryptoOverrideCoverageSummary({ data }: { data: Record<string, unknown> }) {
  const rawOverrides = Array.isArray(data.calibration_overrides)
    ? data.calibration_overrides.filter((entry) => isCalibrationOverrideRow(entry))
    : [];

  const rows = [
    { scope: "major", subtype: "unlock" },
    { scope: "major", subtype: "upgrade" },
    { scope: "major", subtype: "regulatory" },
    { scope: "alt", subtype: "unlock" },
    { scope: "alt", subtype: "upgrade" },
    { scope: "alt", subtype: "regulatory" },
  ].map((target) => {
    const matching = rawOverrides.filter((row) => {
      const assetClass = (row.asset_class || "any").toLowerCase();
      const eventSubtype = (row.event_subtype || "any").toLowerCase();
      const marketType = (row.market_type || "any").toLowerCase();
      const horizon = (row.horizon || "any").toLowerCase();
      const classMatch = assetClass === "any" || assetClass === target.scope;
      const subtypeMatch = eventSubtype === "any" || eventSubtype === target.subtype;
      const scopeMatch = classMatch && subtypeMatch;
      const strategyRelevant = marketType === "any" || marketType === "binary" || marketType === "range";
      const horizonRelevant = horizon === "any" || horizon === "short" || horizon === "medium" || horizon === "long";
      return scopeMatch && strategyRelevant && horizonRelevant;
    });

    const sigmaCovered = matching.some((row) => row.sigma_multiplier !== undefined && row.sigma_multiplier !== null);
    const sizeCovered = matching.some((row) => row.size_multiplier !== undefined && row.size_multiplier !== null);

    return {
      ...target,
      sigmaCovered,
      sizeCovered,
      minEdgeCovered: matching.some(
        (row) => row.min_edge_multiplier !== undefined && row.min_edge_multiplier !== null,
      ),
      maxSpreadCovered: matching.some(
        (row) => row.max_spread_multiplier !== undefined && row.max_spread_multiplier !== null,
      ),
      ruleCount: matching.length,
    };
  });
  const fullyMigratedCount = rows.filter(
    (row) =>
      row.sigmaCovered &&
      row.sizeCovered &&
      row.minEdgeCovered &&
      row.maxSpreadCovered,
  ).length;
  const partiallyStatic = rows
    .filter(
      (row) =>
        !(
          row.sigmaCovered &&
          row.sizeCovered &&
          row.minEdgeCovered &&
          row.maxSpreadCovered
        ),
    )
    .map((row) => `${row.scope}/${row.subtype}`);

  return (
    <div className="mt-4 rounded-box border border-base-300 bg-base-100 p-3 sm:col-span-2">
      <div className="text-xs opacity-70">override coverage</div>
      <div className="mt-1 text-xs opacity-50">
        当前 crypto subtype tuning 以 calibration table 为准；这张表只看 six buckets 的 override 覆盖情况。
      </div>
      <div className="mt-2 flex flex-wrap gap-2 text-xs">
        <span className="badge badge-success badge-outline">
          fully migrated {fullyMigratedCount}/{rows.length}
        </span>
        {partiallyStatic.length > 0 && (
          <span className="badge badge-warning badge-outline">
            remaining static: {partiallyStatic.join(", ")}
          </span>
        )}
      </div>
      <div className="mt-3 overflow-x-auto">
        <table className="table table-xs">
          <thead>
            <tr>
              <th>Scope</th>
              <th>Subtype</th>
              <th>Sigma</th>
              <th>Size</th>
              <th>Min Edge</th>
              <th>Max Spread</th>
              <th>Rules</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={`${row.scope}-${row.subtype}`}>
                <td>
                  <span
                    className={`badge badge-sm ${row.scope === "major" ? "badge-info badge-outline" : "badge-warning badge-outline"}`}
                  >
                    {row.scope}
                  </span>
                </td>
                <td className="capitalize">{row.subtype}</td>
                <td>
                  <span className={`badge badge-sm ${row.sigmaCovered ? "badge-success badge-outline" : "badge-ghost"}`}>
                    {row.sigmaCovered ? "override" : "static"}
                  </span>
                </td>
                <td>
                  <span className={`badge badge-sm ${row.sizeCovered ? "badge-success badge-outline" : "badge-ghost"}`}>
                    {row.sizeCovered ? "override" : "static"}
                  </span>
                </td>
                <td>
                  <span className={`badge badge-sm ${row.minEdgeCovered ? "badge-success badge-outline" : "badge-ghost"}`}>
                    {row.minEdgeCovered ? "override" : "static"}
                  </span>
                </td>
                <td>
                  <span className={`badge badge-sm ${row.maxSpreadCovered ? "badge-success badge-outline" : "badge-ghost"}`}>
                    {row.maxSpreadCovered ? "override" : "static"}
                  </span>
                </td>
                <td className="font-mono">{row.ruleCount}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
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
    const isCalibrationOverrides =
      section === "crypto_alpha" &&
      fieldKey === "calibration_overrides" &&
      value.every((entry) => isCalibrationOverrideRow(entry));
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
                      {provider === "open_meteo" ? "Open-Meteo" : provider === "kma" ? "KMA" : provider === "met_office" ? "Met Office" : "NOAA"}
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
                          {provider === "open_meteo" ? "Open-Meteo" : provider === "kma" ? "KMA" : provider === "met_office" ? "Met Office" : "NOAA"}
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

    if (isCalibrationOverrides) {
      const rows = [...(value as CalibrationOverrideRow[])].sort((a, b) => {
        const scoreDiff = calibrationScopeScore(b) - calibrationScopeScore(a);
        if (scoreDiff !== 0) return scoreDiff;
        return `${a.asset ?? "*"}-${a.horizon ?? "any"}-${a.market_type ?? "any"}`.localeCompare(
          `${b.asset ?? "*"}-${b.horizon ?? "any"}-${b.market_type ?? "any"}`,
        );
      });
      const groupedRows = rows.reduce(
        (acc, row) => {
          const key = row.market_type || "any";
          if (!acc[key]) {
            acc[key] = [];
          }
          acc[key].push(row);
          return acc;
        },
        {} as Record<string, CalibrationOverrideRow[]>,
      );
      const orderedGroupKeys = Object.keys(groupedRows).sort((a, b) => {
        const aScore = a === "any" ? 0 : 1;
        const bScore = b === "any" ? 0 : 1;
        if (aScore !== bScore) {
          return bScore - aScore;
        }
        return calibrationMarketTypeGroupLabel(a).localeCompare(calibrationMarketTypeGroupLabel(b));
      });
      return (
        <div className="rounded-box border border-base-300 bg-base-100 p-3 sm:col-span-2">
          <div className="text-xs opacity-70">{label}</div>
          {hint && <div className="mt-1 text-xs opacity-40">{hint}</div>}
          {rows.length === 0 ? (
            <div className="mt-2 text-sm opacity-60">未配置覆盖规则</div>
          ) : (
            <div className="mt-3 space-y-4">
              {orderedGroupKeys.map((groupKey) => (
                <div key={groupKey} className="space-y-2">
                  <div className="flex items-center gap-2 text-xs">
                    <span className="font-medium opacity-70">market_type</span>
                    <CalibrationScopeBadge value={calibrationMarketTypeGroupLabel(groupKey)} />
                  </div>
                  <div className="overflow-x-auto">
                    <table className="table table-xs">
                      <thead>
                        <tr>
                          <th>asset</th>
                          <th>class</th>
                          <th>horizon</th>
                          <th>market_type</th>
                          <th>event</th>
                          <th>probability</th>
                          <th>sigma</th>
                          <th>size</th>
                          <th>depth_ratio</th>
                          <th>min_edge</th>
                          <th>max_spread</th>
                          <th>hold_edge</th>
                          <th>edge_decay_exit</th>
                          <th>confirm_scan</th>
                          <th>confirm_window</th>
                          <th>cooldown</th>
                          <th>capital_eff</th>
                          <th>model_buffer</th>
                          <th>profit_retention</th>
                          <th>slippage</th>
                          <th>size_retention</th>
                        </tr>
                      </thead>
                      <tbody>
                        {groupedRows[groupKey].map((row, index) => (
                          <tr
                            key={`${row.asset ?? "*"}-${row.asset_class ?? "any"}-${row.horizon ?? "any"}-${row.market_type ?? "any"}-${row.event_subtype ?? "any"}-${index}`}
                          >
                            <td className="font-mono">
                              <CalibrationScopeBadge value={renderCalibrationCell(row.asset || "*")} />
                            </td>
                            <td className="font-mono">
                              <CalibrationScopeBadge value={renderCalibrationCell(row.asset_class || "any")} />
                            </td>
                            <td className="font-mono">
                              <CalibrationScopeBadge value={renderCalibrationCell(row.horizon || "any")} />
                            </td>
                            <td className="font-mono">
                              <CalibrationScopeBadge value={renderCalibrationCell(row.market_type || "any")} />
                            </td>
                            <td className="font-mono">
                              <CalibrationScopeBadge value={renderCalibrationCell(row.event_subtype || "any")} />
                            </td>
                            <td className="font-mono">{renderCalibrationCell(row.probability_calibration)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.sigma_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.size_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.depth_ratio_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.min_edge_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.max_spread_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.hold_edge_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.edge_decay_exit_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.edge_decay_confirmation_scan_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.edge_decay_confirmation_window_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.edge_decay_cooldown_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.capital_efficiency_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.model_reversal_buffer_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.profit_retention_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.slippage_multiplier)}</td>
                            <td className="font-mono">{renderCalibrationCell(row.size_retention_multiplier)}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </div>
              ))}
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
