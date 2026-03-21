import { Fragment, useCallback, useState } from "react";
import {
  fetchCryptoCandidateDecisions,
  fetchCryptoAlphaConfig,
  fetchCryptoExitDecisions,
  fetchPositions,
  fetchStatus,
  type CryptoCandidateDecisionEntry,
  type CryptoAlphaConfigSection,
  type CryptoExitDecisionEntry,
  type PositionEntry,
  type StatusResponse,
} from "../api";
import { usePolling } from "../hooks/usePolling";

function directionLabel(direction: string | null): string {
  switch (direction) {
    case "up":
      return "Up";
    case "down":
      return "Down";
    case "inside_range":
      return "InsideRange";
    case "outside_range":
      return "OutsideRange";
    default:
      return "Unknown";
  }
}

function decisionReasonLabel(reason: string): string {
  switch (reason) {
    case "execution_quality":
      return "执行质量";
    case "profit_retention":
      return "利润保真";
    case "size_retention":
      return "数量保真";
    case "slippage":
      return "滑点";
    case "depth_buffer":
      return "Depth";
    case "executable_efficiency":
      return "执行效率";
    case "estimated_profit":
      return "利润";
    case "static_efficiency":
      return "静态效率";
    case "cost":
      return "成本";
    case "spread":
      return "价差";
    case "size":
      return "数量";
    case "scaled_for_size_retention":
      return "预缩量-数量保真";
    case "scaled_for_depth_buffer":
      return "预缩量-Depth";
    case "seed":
      return "初始入桶";
    default:
      return reason;
  }
}

function gateRejectHint(reason: string): string {
  switch (reason) {
    case "edge_below_threshold":
      return "优先检查 min_edge_bps 和概率校准是否过严";
    case "spread_too_wide":
      return "优先检查 max_spread_bps，或确认市场本身流动性不足";
    case "insufficient_depth_buffer":
      return "优先检查 min_entry_depth_ratio 和 event-aware depth tuning";
    case "insufficient_size_retention":
      return "优先检查 min_size_retention_ratio 和 size_retention multiplier";
    case "asset_exposure_cap":
      return "优先检查单资产/单方向敞口上限是否过紧";
    case "min_order_or_budget":
      return "优先检查最小下单约束、可用预算和 sizing 是否过小";
    default:
      return "优先回看对应 entry gate 和 event-aware override";
  }
}

function gateScaleHint(reason: string): string {
  switch (reason) {
    case "scaled_for_size_retention":
      return "候选能做，但当前盘口只能保留较少目标数量，优先检查 size_retention 与 sizing。";
    case "scaled_for_depth_buffer":
      return "候选能做，但为满足 depth buffer 被前置缩量，优先检查 depth ratio 与 event-aware depth tuning。";
    default:
      return "候选没有被拒，但为了执行质量被前置裁小。";
  }
}

function gateRejectPrioritySummary(
  topReason: string | null,
  topAssetEntry: [string, number] | undefined,
  topSubtypeEntry: [string, number] | undefined,
): string | null {
  if (!topReason) {
    return null;
  }
  const subtypeText =
    topSubtypeEntry && topSubtypeEntry[0] !== "generic"
      ? ` 当前主要集中在 ${topSubtypeEntry[0]} 事件。`
      : "";
  if (topReason === "asset_exposure_cap" && topAssetEntry && topAssetEntry[1] >= 2) {
    return `优先看风控上限，不是 entry 参数：${topAssetEntry[0]} 最近反复撞单资产/单方向限额。${subtypeText}`;
  }
  if (topReason === "min_order_or_budget") {
    return `优先看预算和 sizing，而不是继续放宽 edge/spread。${subtypeText}`;
  }
  if (topReason === "spread_too_wide") {
    return `优先确认市场流动性是否足够，其次再考虑放宽 max_spread_bps。${subtypeText}`;
  }
  if (topReason === "edge_below_threshold") {
    return `优先检查 min_edge_bps 和概率校准是否偏严。${subtypeText}`;
  }
  if (topReason === "insufficient_depth_buffer" || topReason === "insufficient_size_retention") {
    return `优先检查 depth/size retention 相关门槛，而不是先动 Kelly 或敞口。${subtypeText}`;
  }
  return `优先回看对应 gate 的最近集中资产和事件类型，再决定调 entry 还是风控。${subtypeText}`;
}

export default function CryptoMarkets() {
  const [expandedAssets, setExpandedAssets] = useState<Record<string, boolean>>({});
  const [showOnlyReplacements, setShowOnlyReplacements] = useState(true);
  const [selectedDecisionAsset, setSelectedDecisionAsset] = useState("全部");
  const [selectedDecisionDirection, setSelectedDecisionDirection] = useState("全部");
  const [decisionSortMode, setDecisionSortMode] = useState("最新优先");
  const [activeDecisionFocus, setActiveDecisionFocus] = useState<{
    asset: string;
    direction: string;
  } | null>(null);
  const posFetcher = useCallback(() => fetchPositions("crypto_alpha"), []);
  const configFetcher = useCallback(() => fetchCryptoAlphaConfig(), []);
  const statusFetcher = useCallback(() => fetchStatus(), []);
  const decisionsFetcher = useCallback(() => fetchCryptoCandidateDecisions(), []);
  const exitDecisionsFetcher = useCallback(() => fetchCryptoExitDecisions(), []);
  const { data: positions, loading } = usePolling<PositionEntry[]>(posFetcher, 15000);
  const { data: config } = usePolling<CryptoAlphaConfigSection>(configFetcher, 30000);
  const { data: status } = usePolling<StatusResponse>(statusFetcher, 15000);
  const { data: decisions } = usePolling<CryptoCandidateDecisionEntry[]>(decisionsFetcher, 15000);
  const { data: exitDecisions } = usePolling<CryptoExitDecisionEntry[]>(exitDecisionsFetcher, 15000);

  const totalCost = (positions ?? []).reduce((s, p) => s + Number(p.cost_basis), 0);
  const totalPnl = (positions ?? []).reduce((s, p) => s + Number(p.unrealized_pnl ?? 0), 0);
  const totalMarkValue = (positions ?? []).reduce(
    (sum, p) => sum + Number(p.current_price ?? 0) * Number(p.size),
    0,
  );
  const walletBalance = Number(status?.wallet_balance ?? 0);
  const marketCount = new Set((positions ?? []).map((p) => p.condition_id).filter(Boolean)).size;
  const decisionAssetOptions = ["全部", ...Array.from(new Set((decisions ?? []).map((decision) => decision.asset)))];
  const decisionDirectionOptions = [
    "全部",
    ...Array.from(new Set((decisions ?? []).map((decision) => decision.direction))),
  ];
  const visibleDecisions = (decisions ?? [])
    .filter((decision) => selectedDecisionAsset === "全部" || decision.asset === selectedDecisionAsset)
    .filter(
      (decision) =>
        selectedDecisionDirection === "全部" || decision.direction === selectedDecisionDirection,
    )
    .filter((decision) => !showOnlyReplacements || decision.action === "replace")
    .sort((a, b) => {
      if (decisionSortMode === "效率差值") {
        const aDelta =
          Number(a.selected_executable_efficiency) - Number(a.replaced_executable_efficiency ?? 0);
        const bDelta =
          Number(b.selected_executable_efficiency) - Number(b.replaced_executable_efficiency ?? 0);
        if (bDelta !== aDelta) {
          return bDelta - aDelta;
        }
      }
      return new Date(b.recorded_at).getTime() - new Date(a.recorded_at).getTime();
    })
    .slice(0, 12);
  const visibleReplacements = visibleDecisions.filter((decision) => decision.action === "replace");
  const visibleGateRejects = visibleDecisions.filter((decision) => decision.action === "gate_reject");
  const visibleGateScales = visibleDecisions.filter((decision) => decision.action === "gate_scale");
  const gateRejectBreakdown = Array.from(
    visibleGateRejects.reduce<Map<string, number>>((acc, decision) => {
      const label = decisionReasonLabel(decision.reason);
      acc.set(label, (acc.get(label) ?? 0) + 1);
      return acc;
    }, new Map<string, number>()),
  ).sort((a, b) => b[1] - a[1]);
  const gateRejectReasonBreakdown =
    status?.crypto_gate_reject_summary?.reason_counts?.length
      ? status.crypto_gate_reject_summary.reason_counts.map(
          (entry) => [decisionReasonLabel(entry.label), entry.count] as [string, number],
        )
      : gateRejectBreakdown;
  const gateRejectAssetBreakdown = Array.from(
    visibleGateRejects.reduce<Map<string, number>>((acc, decision) => {
      acc.set(decision.asset, (acc.get(decision.asset) ?? 0) + 1);
      return acc;
    }, new Map<string, number>()),
  )
    .sort((a, b) => b[1] - a[1])
    .slice(0, 3);
  const gateRejectAssetBreakdownView =
    status?.crypto_gate_reject_summary?.asset_counts?.length
      ? status.crypto_gate_reject_summary.asset_counts
          .map((entry) => [entry.label, entry.count] as [string, number])
          .slice(0, 3)
      : gateRejectAssetBreakdown;
  const gateRejectSubtypeBreakdown = Array.from(
    visibleGateRejects.reduce<Map<string, number>>((acc, decision) => {
      const subtype = decision.event_subtype ?? "generic";
      acc.set(subtype, (acc.get(subtype) ?? 0) + 1);
      return acc;
    }, new Map<string, number>()),
  )
    .sort((a, b) => b[1] - a[1])
    .slice(0, 3);
  const gateRejectSubtypeBreakdownView =
    status?.crypto_gate_reject_summary?.subtype_counts?.length
      ? status.crypto_gate_reject_summary.subtype_counts
          .map((entry) => [entry.label, entry.count] as [string, number])
          .slice(0, 3)
      : gateRejectSubtypeBreakdown;
  const gateRejectReasonDetails =
    status?.crypto_gate_reject_summary?.reason_details?.length
      ? status.crypto_gate_reject_summary.reason_details.slice(0, 3)
      : [];
  const gateRejectReasonWindow8 =
    status?.crypto_gate_reject_summary?.reason_windows?.recent_8?.map(
      (entry) => [decisionReasonLabel(entry.label), entry.count] as [string, number],
    ) ?? [];
  const gateRejectReasonWindow24 =
    status?.crypto_gate_reject_summary?.reason_windows?.recent_24?.map(
      (entry) => [decisionReasonLabel(entry.label), entry.count] as [string, number],
    ) ?? [];
  const gateRejectSubtypeWindow8 =
    status?.crypto_gate_reject_summary?.subtype_windows?.recent_8?.map(
      (entry) => [entry.label, entry.count] as [string, number],
    ) ?? [];
  const gateRejectSubtypeWindow24 =
    status?.crypto_gate_reject_summary?.subtype_windows?.recent_24?.map(
      (entry) => [entry.label, entry.count] as [string, number],
    ) ?? [];
  const gateRejectAssetWindow8 =
    status?.crypto_gate_reject_summary?.asset_windows?.recent_8?.map(
      (entry) => [entry.label, entry.count] as [string, number],
    ) ?? [];
  const gateRejectAssetWindow24 =
    status?.crypto_gate_reject_summary?.asset_windows?.recent_24?.map(
      (entry) => [entry.label, entry.count] as [string, number],
    ) ?? [];
  const topGateReject = status?.crypto_gate_reject_summary?.top_reason?.label ?? visibleGateRejects[0]?.reason ?? null;
  const topGateAssetEntry = status?.crypto_gate_reject_summary?.top_asset
    ? [status.crypto_gate_reject_summary.top_asset.label, status.crypto_gate_reject_summary.top_asset.count] as [string, number]
    : gateRejectAssetBreakdown[0];
  const topGateSubtypeEntry = status?.crypto_gate_reject_summary?.top_subtype
    ? [status.crypto_gate_reject_summary.top_subtype.label, status.crypto_gate_reject_summary.top_subtype.count] as [string, number]
    : gateRejectSubtypeBreakdown[0];
  const gatePrioritySummary = gateRejectPrioritySummary(
    topGateReject,
    topGateAssetEntry,
    topGateSubtypeEntry,
  );
  const gateScaleReasonBreakdown =
    status?.crypto_gate_scale_summary?.reason_counts?.length
      ? status.crypto_gate_scale_summary.reason_counts.map(
          (entry) => [decisionReasonLabel(entry.label), entry.count] as [string, number],
        )
      : Array.from(
          visibleGateScales.reduce<Map<string, number>>((acc, decision) => {
            const label = decisionReasonLabel(decision.reason);
            acc.set(label, (acc.get(label) ?? 0) + 1);
            return acc;
          }, new Map<string, number>()),
        ).sort((a, b) => b[1] - a[1]);
  const gateScaleAssetBreakdownView =
    status?.crypto_gate_scale_summary?.asset_counts?.length
      ? status.crypto_gate_scale_summary.asset_counts
          .map((entry) => [entry.label, entry.count] as [string, number])
          .slice(0, 3)
      : [];
  const gateScaleSubtypeBreakdownView =
    status?.crypto_gate_scale_summary?.subtype_counts?.length
      ? status.crypto_gate_scale_summary.subtype_counts
          .map((entry) => [entry.label, entry.count] as [string, number])
          .slice(0, 3)
      : [];
  const topGateScale = status?.crypto_gate_scale_summary?.top_reason?.label ?? visibleGateScales[0]?.reason ?? null;
  const gateScaleReasonDetails =
    status?.crypto_gate_scale_summary?.reason_details?.length
      ? status.crypto_gate_scale_summary.reason_details.slice(0, 3)
      : [];
  const gateScaleRecentCount = status?.crypto_gate_scale_summary?.recent_count ?? visibleGateScales.length;
  const tuningHints = status?.crypto_entry_tuning_hints ?? [];
  const overrideSuggestions = status?.crypto_override_suggestions ?? [];
  const averageEfficiencyDelta =
    visibleReplacements.length > 0
      ? visibleReplacements.reduce(
          (sum, decision) =>
            sum +
            (Number(decision.selected_executable_efficiency) -
              Number(decision.replaced_executable_efficiency ?? 0)),
          0,
        ) / visibleReplacements.length
      : 0;
  const maxEfficiencyDelta =
    visibleReplacements.length > 0
      ? Math.max(
          ...visibleReplacements.map(
            (decision) =>
              Number(decision.selected_executable_efficiency) -
              Number(decision.replaced_executable_efficiency ?? 0),
          ),
        )
      : 0;
  const positionsByAsset = Object.values(
    (positions ?? []).reduce<Record<string, {
      key: string;
      asset: string;
      direction: string;
      positions: number;
      costBasis: number;
      unrealizedPnl: number;
      marketIds: Set<string>;
      rows: PositionEntry[];
    }>>((acc, position) => {
      const asset = position.asset ?? "未识别";
      const direction = directionLabel(position.direction);
      const key = `${asset}::${direction}`;
      const entry = acc[key] ?? {
        key,
        asset,
        direction,
        positions: 0,
        costBasis: 0,
        unrealizedPnl: 0,
        marketIds: new Set<string>(),
        rows: [],
      };
      entry.positions += 1;
      entry.costBasis += Number(position.cost_basis);
      entry.unrealizedPnl += Number(position.unrealized_pnl ?? 0);
      entry.rows.push(position);
      if (position.condition_id) {
        entry.marketIds.add(position.condition_id);
      }
      acc[key] = entry;
      return acc;
    }, {}),
  )
    .map((entry) => ({
      ...entry,
      markets: entry.marketIds.size,
      strategySharePct: totalCost > 0 ? (entry.costBasis / totalCost) * 100 : 0,
      walletSharePct: walletBalance > 0 ? (entry.costBasis / walletBalance) * 100 : 0,
    }))
    .sort((a, b) => {
      const capPct = (config?.max_exposure_per_asset_direction_pct ?? 0) * 100;
      const aCapDistance = capPct > 0 ? a.walletSharePct / capPct : 0;
      const bCapDistance = capPct > 0 ? b.walletSharePct / capPct : 0;
      if (bCapDistance !== aCapDistance) {
        return bCapDistance - aCapDistance;
      }
      if (b.costBasis !== a.costBasis) {
        return b.costBasis - a.costBasis;
      }
      return b.unrealizedPnl - a.unrealizedPnl;
    });

  if (loading && !positions) {
    return (
      <div className="flex justify-center items-center h-64">
        <span className="loading loading-spinner loading-lg" />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-bold">加密市场</h1>

      {status && (
        <div className="card bg-base-200 shadow-sm">
          <div className="card-body p-4">
            <div className="flex items-center justify-between gap-3">
              <h2 className="card-title text-base">运行上下文</h2>
              <span className="text-xs opacity-60">页面完全基于配置接口和持仓快照展示</span>
            </div>
            <div className="grid grid-cols-1 gap-3 text-sm sm:grid-cols-3">
              <div>
                <div className="opacity-60 text-xs">持仓快照更新时间</div>
                <div>
                  {status.positions_snapshot_updated_at
                    ? new Date(status.positions_snapshot_updated_at).toLocaleString("zh-CN")
                    : "-"}
                </div>
              </div>
              <div>
                <div className="opacity-60 text-xs">账户</div>
                <div>{status.accounts.map((account) => account.name).join(", ") || "-"}</div>
              </div>
              <div>
                <div className="opacity-60 text-xs">代理钱包</div>
                <div className="font-mono text-xs break-all">
                  {status.accounts.map((account) => account.proxy_wallet || "(EOA)").join(", ") || "-"}
                </div>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Strategy stats */}
      <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 xl:grid-cols-5">
        <div className="stat bg-base-200 rounded-box p-4">
          <div className="stat-title text-xs">持仓数</div>
          <div className="stat-value text-lg">{positions?.length ?? 0}</div>
        </div>
        <div className="stat bg-base-200 rounded-box p-4">
          <div className="stat-title text-xs">市场数</div>
          <div className="stat-value text-lg">{marketCount}</div>
        </div>
        <div className="stat bg-base-200 rounded-box p-4">
          <div className="stat-title text-xs">策略成本</div>
          <div className="stat-value text-lg">${totalCost.toFixed(2)}</div>
        </div>
        <div className="stat bg-base-200 rounded-box p-4">
          <div className="stat-title text-xs">持仓市值</div>
          <div className="stat-value text-lg">${totalMarkValue.toFixed(2)}</div>
        </div>
        <div className="stat bg-base-200 rounded-box p-4">
          <div className="stat-title text-xs">未实现盈亏</div>
          <div className={`stat-value text-lg ${totalPnl >= 0 ? "text-success" : "text-error"}`}>
            ${totalPnl.toFixed(2)}
          </div>
        </div>
      </div>

      {config && (
        <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <div className="flex items-center justify-between gap-3">
            <h2 className="card-title text-base">策略参数</h2>
            <div className="text-xs opacity-60">当前生效配置</div>
          </div>
            <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
              <MetricCard label="现价刷新" value={`${config.spot_refresh_interval_secs}s`} />
              <MetricCard label="历史刷新" value={`${config.history_refresh_interval_secs}s`} />
              <MetricCard label="IV 刷新" value={`${config.iv_refresh_interval_secs}s`} />
              <MetricCard label="最小 Edge" value={`${config.min_edge_bps} bps`} />
              <MetricCard label="最大价差" value={`${config.max_spread_bps} bps`} />
              <MetricCard label="退出缓冲" value={`${config.exit_buffer_bps} bps`} />
              <MetricCard
                label="相对止损"
                value={`${(config.relative_stop_loss_ratio * 100).toFixed(0)}%`}
              />
              <MetricCard
                label="单资产敞口上限"
                value={`${(config.max_exposure_per_asset_pct * 100).toFixed(0)}%`}
              />
              <MetricCard
                label="单方向敞口上限"
                value={`${(config.max_exposure_per_asset_direction_pct * 100).toFixed(0)}%`}
              />
            </div>
            <div className="mt-4 rounded-box bg-base-100/70 p-4 text-sm leading-6">
              <div className="mb-2 text-xs font-semibold uppercase tracking-wide opacity-60">
                风险解释
              </div>
              <p>
                当前加密策略会优先使用更快的现价刷新，现价每
                {" "}
                <span className="font-medium">{config.spot_refresh_interval_secs} 秒</span>
                {" "}
                更新，30 日历史收盘价每
                {" "}
                <span className="font-medium">{config.history_refresh_interval_secs} 秒</span>
                {" "}
                更新，BTC/ETH 的隐含波动率每
                {" "}
                <span className="font-medium">{config.iv_refresh_interval_secs} 秒</span>
                {" "}
                更新。
              </p>
              <p className="mt-2">
                入场需要至少
                {" "}
                <span className="font-medium">{config.min_edge_bps} bps</span>
                {" "}
                的模型 edge，且盘口价差不能超过
                {" "}
                <span className="font-medium">{config.max_spread_bps} bps</span>
                。
                单个资产的总暴露最多占账户余额的
                {" "}
                <span className="font-medium">
                  {(config.max_exposure_per_asset_pct * 100).toFixed(0)}%
                </span>
                ；单资产单方向的暴露最多占
                {" "}
                <span className="font-medium">
                  {(config.max_exposure_per_asset_direction_pct * 100).toFixed(0)}%
                </span>
                ，避免多个 BTC-Up / ETH-Down 方向仓位叠加过度。
              </p>
              <p className="mt-2">
                持仓后，如果买一价跌破持仓成本的
                {" "}
                <span className="font-medium">
                  {(config.relative_stop_loss_ratio * 100).toFixed(0)}%
                </span>
                ，会触发相对止损；若模型概率低于盘口买价减去
                {" "}
                <span className="font-medium">{config.exit_buffer_bps} bps</span>
                {" "}
                缓冲，也会按模型反转退出。
              </p>
            </div>
          </div>
        </div>
      )}

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <div className="flex items-center justify-between gap-3">
            <h2 className="card-title text-base">按资产聚合</h2>
            <span className="text-xs opacity-60">
              默认按接近方向上限程度排序；点击资产行展开。方向上限高亮按当前余额口径计算，其余集中度按策略成本口径计算
            </span>
          </div>
          <div className="overflow-x-auto">
            <div className="mb-3 flex flex-wrap gap-3 text-xs opacity-70">
              <span className="badge badge-ghost badge-sm">可见决策 {visibleDecisions.length}</span>
              <span className="badge badge-ghost badge-sm">Replace {visibleReplacements.length}</span>
              <span className="badge badge-ghost badge-sm">
                平均效率差值 {visibleReplacements.length > 0 ? averageEfficiencyDelta.toFixed(3) : "-"}
              </span>
              <span className="badge badge-ghost badge-sm">
                最大效率差值 {visibleReplacements.length > 0 ? maxEfficiencyDelta.toFixed(3) : "-"}
              </span>
            </div>
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>资产 / 方向</th>
                  <th>持仓数</th>
                  <th>市场数</th>
                  <th>成本</th>
                  <th>未实现盈亏</th>
                </tr>
              </thead>
              <tbody>
                {positionsByAsset.map((asset) => {
                  const expanded = expandedAssets[asset.key] ?? false;
                  const concentrationTone =
                    config && asset.walletSharePct >= config.max_exposure_per_asset_direction_pct * 100
                      ? "bg-error/10"
                      : config && asset.walletSharePct >= config.max_exposure_per_asset_direction_pct * 100 * 0.8
                        ? "bg-warning/10"
                        : asset.strategySharePct >= 50
                      ? "bg-error/10"
                      : asset.strategySharePct >= 30
                        ? "bg-warning/10"
                        : "";
                  const concentrationBadge =
                    config && asset.walletSharePct >= config.max_exposure_per_asset_direction_pct * 100
                      ? "badge-error"
                      : config && asset.walletSharePct >= config.max_exposure_per_asset_direction_pct * 100 * 0.8
                        ? "badge-warning"
                      : asset.strategySharePct >= 50
                      ? "badge-error"
                      : asset.strategySharePct >= 30
                      ? "badge-warning"
                        : "badge-ghost";
                  const isDecisionFocused =
                    activeDecisionFocus?.asset === asset.asset &&
                    activeDecisionFocus.direction === asset.direction;
                  return (
                    <Fragment key={asset.key}>
                      <tr
                        className={`cursor-pointer hover ${concentrationTone} ${
                          isDecisionFocused ? "ring-1 ring-info bg-info/10" : ""
                        }`}
                        onClick={() =>
                          setExpandedAssets((current) => ({
                            ...current,
                            [asset.key]: !expanded,
                          }))
                        }
                      >
                        <td>
                          <div className="flex items-center gap-2">
                            <span className="text-xs opacity-60">{expanded ? "▼" : "▶"}</span>
                            <span>{asset.asset}</span>
                            <span className="badge badge-outline badge-xs">{asset.direction}</span>
                            <span className={`badge badge-xs ${concentrationBadge}`}>
                              {config && asset.walletSharePct >= config.max_exposure_per_asset_direction_pct * 100
                                ? "近方向上限"
                                : config && asset.walletSharePct >= config.max_exposure_per_asset_direction_pct * 100 * 0.8
                                  ? "接近方向上限"
                                : asset.strategySharePct >= 50
                                ? "高集中"
                                : asset.strategySharePct >= 30
                                  ? "中集中"
                                  : "分散"}
                            </span>
                          </div>
                        </td>
                        <td>{asset.positions}</td>
                        <td>{asset.markets}</td>
                        <td>
                          <div>${asset.costBasis.toFixed(2)}</div>
                          <div className="text-[11px] opacity-60">
                            占策略 {asset.strategySharePct.toFixed(1)}%
                            {walletBalance > 0 ? ` · 占余额 ${asset.walletSharePct.toFixed(1)}%` : ""}
                          </div>
                          {config && walletBalance > 0 && (
                            <div className="text-[11px] opacity-60">
                              已用/上限 ${asset.costBasis.toFixed(2)} / $
                              {(walletBalance * config.max_exposure_per_asset_direction_pct).toFixed(2)}
                            </div>
                          )}
                        </td>
                        <td className={asset.unrealizedPnl >= 0 ? "text-success" : "text-error"}>
                          ${asset.unrealizedPnl.toFixed(2)}
                        </td>
                      </tr>
                      {expanded &&
                        asset.rows.map((position) => (
                          <tr
                            key={`${asset.key}-${position.token_id}`}
                            className={`bg-base-100/50 ${isDecisionFocused ? "bg-info/10" : ""}`}
                          >
                            <td className="pl-8 max-w-xs truncate" title={position.question ?? position.token_id}>
                              <div>{position.question ?? `${position.token_id.slice(0, 14)}...`}</div>
                              <div className="mt-1 text-[11px] opacity-60">
                                资产占比
                                {" "}
                                {asset.costBasis > 0
                                  ? `${((Number(position.cost_basis) / asset.costBasis) * 100).toFixed(1)}%`
                                  : "-"}
                                {" · "}
                                策略占比
                                {" "}
                                {totalCost > 0
                                  ? `${((Number(position.cost_basis) / totalCost) * 100).toFixed(1)}%`
                                  : "-"}
                              </div>
                            </td>
                            <td className="text-xs">
                              <span
                                className={`badge badge-sm ${
                                  position.outcome === "YES" ? "badge-success" : "badge-error"
                                }`}
                              >
                                {position.outcome ?? "-"}
                              </span>
                              <span className="ml-2 opacity-70">{Number(position.size).toFixed(1)}</span>
                            </td>
                            <td className="opacity-50">-</td>
                            <td>${Number(position.cost_basis).toFixed(2)}</td>
                            <td
                              className={
                                Number(position.unrealized_pnl ?? 0) >= 0
                                  ? "text-success"
                                  : "text-error"
                              }
                            >
                              ${Number(position.unrealized_pnl ?? 0).toFixed(2)}
                            </td>
                          </tr>
                        ))}
                    </Fragment>
                  );
                })}
                {positionsByAsset.length === 0 && (
                  <tr><td colSpan={5} className="text-center opacity-50">暂无资产敞口</td></tr>
                )}
              </tbody>
            </table>
          </div>
        </div>
      </div>

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <div className="flex items-center justify-between gap-3">
            <div>
              <h2 className="card-title text-base">最近退出决策</h2>
              <div className="text-xs opacity-60">
                展示最近触发的 crypto exit reason，以及对应的事件上下文
              </div>
            </div>
          </div>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>时间</th>
                  <th>资产</th>
                  <th>退出原因</th>
                  <th>事件</th>
                  <th>市场</th>
                  <th>Bid / 成本</th>
                  <th>数量</th>
                </tr>
              </thead>
              <tbody>
                {(exitDecisions ?? []).slice(0, 8).map((decision) => (
                  <tr key={`${decision.recorded_at}-${decision.question}-${decision.reason}`}>
                    <td className="text-xs opacity-70">
                      {new Date(decision.recorded_at).toLocaleString("zh-CN")}
                    </td>
                    <td>{decision.asset ?? "-"}</td>
                    <td>
                      <span className="badge badge-outline badge-xs">{decision.reason}</span>
                    </td>
                    <td className="text-xs">
                      {decision.event_category ? (
                        <>
                          <div>{decision.event_category}</div>
                          <div className="opacity-60">{decision.event_subtype ?? "-"}</div>
                          <div className="opacity-50">
                            {decision.event_context_source ?? "-"}
                          </div>
                          {decision.event_title ? (
                            <div className="truncate opacity-50" title={decision.event_title}>
                              {decision.event_title}
                            </div>
                          ) : null}
                        </>
                      ) : (
                        <span className="opacity-40">-</span>
                      )}
                    </td>
                    <td className="max-w-xs truncate" title={decision.question}>
                      {decision.question}
                    </td>
                    <td className="text-xs">
                      <div>Bid ${Number(decision.best_bid).toFixed(3)}</div>
                      <div className="opacity-60">成本 ${Number(decision.avg_cost).toFixed(3)}</div>
                    </td>
                    <td>{Number(decision.size).toFixed(2)}</td>
                  </tr>
                ))}
                {(!exitDecisions || exitDecisions.length === 0) && (
                  <tr><td colSpan={7} className="text-center opacity-50">暂无最近退出决策</td></tr>
                )}
              </tbody>
            </table>
          </div>
        </div>
      </div>

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <div className="flex items-center justify-between gap-3">
            <div>
              <h2 className="card-title text-base">最近候选决策</h2>
              <div className="text-xs opacity-60">
                默认只看发生过竞争的 `replace`；关闭筛选后可同时查看 `seed`
              </div>
            </div>
            <div className="flex items-center gap-3">
              <label className="label cursor-pointer gap-2 py-0">
                <span className="label-text text-xs">资产</span>
                <select
                  className="select select-bordered select-xs"
                  value={selectedDecisionAsset}
                  onChange={(event) => setSelectedDecisionAsset(event.target.value)}
                >
                  {decisionAssetOptions.map((asset) => (
                    <option key={asset} value={asset}>
                      {asset}
                    </option>
                  ))}
                </select>
              </label>
              <label className="label cursor-pointer gap-2 py-0">
                <span className="label-text text-xs">方向</span>
                <select
                  className="select select-bordered select-xs"
                  value={selectedDecisionDirection}
                  onChange={(event) => setSelectedDecisionDirection(event.target.value)}
                >
                  {decisionDirectionOptions.map((direction) => (
                    <option key={direction} value={direction}>
                      {direction}
                    </option>
                  ))}
                </select>
              </label>
              <label className="label cursor-pointer gap-2 py-0">
                <span className="label-text text-xs">排序</span>
                <select
                  className="select select-bordered select-xs"
                  value={decisionSortMode}
                  onChange={(event) => setDecisionSortMode(event.target.value)}
                >
                  <option value="最新优先">最新优先</option>
                  <option value="效率差值">效率差值</option>
                </select>
              </label>
              <label className="label cursor-pointer gap-2 py-0">
                <span className="label-text text-xs">仅看 replace</span>
                <input
                  type="checkbox"
                  className="toggle toggle-sm"
                  checked={showOnlyReplacements}
                  onChange={() => setShowOnlyReplacements((current) => !current)}
                />
              </label>
            </div>
          </div>
          {activeDecisionFocus && (
            <div className="mb-3 flex items-center justify-between rounded-box bg-info/10 px-3 py-2 text-xs">
              <span>
                当前聚焦：
                {" "}
                <span className="font-medium">{activeDecisionFocus.asset}</span>
                {" / "}
                <span className="font-medium">{activeDecisionFocus.direction}</span>
              </span>
              <button
                type="button"
                className="btn btn-ghost btn-xs"
                onClick={() => setActiveDecisionFocus(null)}
              >
                清除高亮
              </button>
            </div>
          )}
          {tuningHints.length > 0 && (
            <div className="mb-3 rounded-box bg-info/10 px-3 py-2 text-xs">
              <div className="mb-2 font-medium">当前参数建议</div>
              <div className="grid gap-2">
                {tuningHints.slice(0, 3).map((hint, index) => (
                  <div key={`${hint.kind}-${hint.title}-${index}`} className="rounded-box bg-base-100/60 px-2 py-2">
                    <div className="mb-1 flex items-center gap-2">
                      <span
                        className={`badge badge-sm ${
                          hint.priority === "high"
                            ? "badge-error"
                            : hint.priority === "medium"
                              ? "badge-warning"
                              : "badge-info"
                        }`}
                      >
                        {hint.priority}
                      </span>
                      <span className="font-medium">{hint.title}</span>
                    </div>
                    <div className="opacity-80">{hint.detail}</div>
                  </div>
                ))}
              </div>
            </div>
          )}
          {overrideSuggestions.length > 0 && (
            <div className="mb-3 rounded-box bg-secondary/10 px-3 py-2 text-xs">
              <div className="mb-2 font-medium">建议的 Override 调整</div>
              <div className="grid gap-2">
                {overrideSuggestions.slice(0, 3).map((suggestion, index) => (
                  <div
                    key={`${suggestion.target_field}-${suggestion.scope_label}-${index}`}
                    className="rounded-box bg-base-100/60 px-2 py-2"
                  >
                    <div className="mb-1 flex flex-wrap items-center gap-2">
                      <span
                        className={`badge badge-sm ${
                          suggestion.priority === "high"
                            ? "badge-error"
                            : suggestion.priority === "medium"
                              ? "badge-warning"
                              : "badge-info"
                        }`}
                      >
                        {suggestion.priority}
                      </span>
                      <span className="font-medium">{suggestion.scope_label}</span>
                      <span className="badge badge-sm badge-outline">{suggestion.target_field}</span>
                      <span className="badge badge-sm badge-outline">{suggestion.direction}</span>
                    </div>
                    <div className="mb-1 opacity-80">{suggestion.rationale}</div>
                    <div className="opacity-60">
                      selector: class={suggestion.selector_asset_class}
                      {" · "}
                      event={suggestion.selector_event_subtype}
                      {" · "}
                      source={decisionReasonLabel(suggestion.source_reason)}
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}
          {(status?.crypto_gate_reject_summary?.recent_count ?? visibleGateRejects.length) > 0 && (
            <div className="mb-3 rounded-box bg-error/10 px-3 py-2 text-xs">
              <div className="mb-1 font-medium">
                最近前门拒绝 {status?.crypto_gate_reject_summary?.recent_count ?? visibleGateRejects.length} 条
              </div>
              {topGateReject && (
                <div className="mb-2 opacity-80">
                  当前最常见前门摩擦：
                  {" "}
                  <span className="font-medium">{decisionReasonLabel(topGateReject)}</span>
                  {" · "}
                  {gateRejectHint(topGateReject)}
                </div>
              )}
              {gatePrioritySummary && (
                <div className="mb-2 rounded-box bg-base-100/60 px-2 py-1 opacity-80">
                  {gatePrioritySummary}
                </div>
              )}
              <div className="flex flex-wrap gap-2">
                {gateRejectReasonBreakdown.map(([reason, count]) => (
                  <span key={reason} className="badge badge-sm badge-error badge-outline">
                    {reason} {count}
                  </span>
                ))}
              </div>
              {(gateRejectReasonWindow8.length > 0 || gateRejectReasonWindow24.length > 0) && (
                <div className="mt-2 grid gap-2 sm:grid-cols-2">
                  {gateRejectReasonWindow8.length > 0 && (
                    <div>
                      <div className="mb-1 opacity-70">最近 8 条</div>
                      <div className="flex flex-wrap gap-2">
                        {gateRejectReasonWindow8.map(([reason, count]) => (
                          <span key={`recent8-${reason}`} className="badge badge-sm badge-error badge-outline">
                            {reason} {count}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                  {gateRejectReasonWindow24.length > 0 && (
                    <div>
                      <div className="mb-1 opacity-70">最近 24 条</div>
                      <div className="flex flex-wrap gap-2">
                        {gateRejectReasonWindow24.map(([reason, count]) => (
                          <span key={`recent24-${reason}`} className="badge badge-sm badge-outline">
                            {reason} {count}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                </div>
              )}
              {(gateRejectSubtypeWindow8.length > 0 || gateRejectSubtypeWindow24.length > 0) && (
                <div className="mt-2 grid gap-2 sm:grid-cols-2">
                  {gateRejectSubtypeWindow8.length > 0 && (
                    <div>
                      <div className="mb-1 opacity-70">最近 8 条事件类型</div>
                      <div className="flex flex-wrap gap-2">
                        {gateRejectSubtypeWindow8.map(([subtype, count]) => (
                          <span
                            key={`recent8-subtype-${subtype}`}
                            className="badge badge-sm badge-outline"
                          >
                            {subtype} {count}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                  {gateRejectSubtypeWindow24.length > 0 && (
                    <div>
                      <div className="mb-1 opacity-70">最近 24 条事件类型</div>
                      <div className="flex flex-wrap gap-2">
                        {gateRejectSubtypeWindow24.map(([subtype, count]) => (
                          <span
                            key={`recent24-subtype-${subtype}`}
                            className="badge badge-sm badge-outline"
                          >
                            {subtype} {count}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                </div>
              )}
              {(gateRejectAssetWindow8.length > 0 || gateRejectAssetWindow24.length > 0) && (
                <div className="mt-2 grid gap-2 sm:grid-cols-2">
                  {gateRejectAssetWindow8.length > 0 && (
                    <div>
                      <div className="mb-1 opacity-70">最近 8 条资产</div>
                      <div className="flex flex-wrap gap-2">
                        {gateRejectAssetWindow8.map(([asset, count]) => (
                          <span
                            key={`recent8-asset-${asset}`}
                            className="badge badge-sm badge-outline"
                          >
                            {asset} {count}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                  {gateRejectAssetWindow24.length > 0 && (
                    <div>
                      <div className="mb-1 opacity-70">最近 24 条资产</div>
                      <div className="flex flex-wrap gap-2">
                        {gateRejectAssetWindow24.map(([asset, count]) => (
                          <span
                            key={`recent24-asset-${asset}`}
                            className="badge badge-sm badge-outline"
                          >
                            {asset} {count}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                </div>
              )}
              {gateRejectReasonDetails.length > 0 && (
                <div className="mt-2 grid gap-2">
                  {gateRejectReasonDetails.map((detail) => (
                    <div
                      key={detail.label}
                      className="rounded-box bg-base-100/50 px-2 py-1 opacity-80"
                    >
                      <span className="font-medium">{decisionReasonLabel(detail.label)}</span>
                      {" · "}
                      {detail.count} 次
                      {detail.top_asset ? ` · 主要资产 ${detail.top_asset.label} ${detail.top_asset.count}` : ""}
                      {detail.top_subtype
                        ? ` · 主要事件 ${detail.top_subtype.label} ${detail.top_subtype.count}`
                        : ""}
                    </div>
                  ))}
                </div>
              )}
              {(gateRejectAssetBreakdownView.length > 0 || gateRejectSubtypeBreakdownView.length > 0) && (
                <div className="mt-2 grid gap-2 sm:grid-cols-2">
                  {gateRejectAssetBreakdownView.length > 0 && (
                    <div>
                      <div className="mb-1 opacity-70">主要资产</div>
                      <div className="flex flex-wrap gap-2">
                        {gateRejectAssetBreakdownView.map(([asset, count]) => (
                          <span key={asset} className="badge badge-sm badge-outline">
                            {asset} {count}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                  {gateRejectSubtypeBreakdownView.length > 0 && (
                    <div>
                      <div className="mb-1 opacity-70">主要事件类型</div>
                      <div className="flex flex-wrap gap-2">
                        {gateRejectSubtypeBreakdownView.map(([subtype, count]) => (
                          <span key={subtype} className="badge badge-sm badge-outline">
                            {subtype} {count}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                </div>
              )}
            </div>
          )}
          {gateScaleRecentCount > 0 && (
            <div className="mb-3 rounded-box bg-warning/10 px-3 py-2 text-xs">
              <div className="mb-1 font-medium">
                最近前门缩量 {gateScaleRecentCount} 条
              </div>
              {topGateScale && (
                <div className="mb-2 opacity-80">
                  当前最常见缩量来源：
                  {" "}
                  <span className="font-medium">{decisionReasonLabel(topGateScale)}</span>
                  {" · "}
                  {gateScaleHint(topGateScale)}
                </div>
              )}
              <div className="flex flex-wrap gap-2">
                {gateScaleReasonBreakdown.map(([reason, count]) => (
                  <span key={reason} className="badge badge-sm badge-warning badge-outline">
                    {reason} {count}
                  </span>
                ))}
              </div>
              {gateScaleReasonDetails.length > 0 && (
                <div className="mt-2 grid gap-2">
                  {gateScaleReasonDetails.map((detail) => (
                    <div key={detail.label} className="rounded-box bg-base-100/50 px-2 py-1 opacity-80">
                      <span className="font-medium">{decisionReasonLabel(detail.label)}</span>
                      {" · "}
                      {detail.count} 次
                      {detail.top_asset ? ` · 主要资产 ${detail.top_asset.label} ${detail.top_asset.count}` : ""}
                      {detail.top_subtype
                        ? ` · 主要事件 ${detail.top_subtype.label} ${detail.top_subtype.count}`
                        : ""}
                    </div>
                  ))}
                </div>
              )}
              {(gateScaleAssetBreakdownView.length > 0 || gateScaleSubtypeBreakdownView.length > 0) && (
                <div className="mt-2 grid gap-2 sm:grid-cols-2">
                  {gateScaleAssetBreakdownView.length > 0 && (
                    <div>
                      <div className="mb-1 opacity-70">主要资产</div>
                      <div className="flex flex-wrap gap-2">
                        {gateScaleAssetBreakdownView.map(([asset, count]) => (
                          <span key={`scale-asset-${asset}`} className="badge badge-sm badge-outline">
                            {asset} {count}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                  {gateScaleSubtypeBreakdownView.length > 0 && (
                    <div>
                      <div className="mb-1 opacity-70">主要事件类型</div>
                      <div className="flex flex-wrap gap-2">
                        {gateScaleSubtypeBreakdownView.map(([subtype, count]) => (
                          <span key={`scale-subtype-${subtype}`} className="badge badge-sm badge-outline">
                            {subtype} {count}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                </div>
              )}
            </div>
          )}
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>时间</th>
                  <th>资产 / 方向</th>
                  <th>事件</th>
                  <th>动作</th>
                  <th>保留候选</th>
                  <th>被替换候选</th>
                  <th>利润保真</th>
                  <th>数量保真</th>
                  <th>执行质量</th>
                  <th>可执行效率</th>
                  <th>Depth Buffer</th>
                </tr>
              </thead>
              <tbody>
                {visibleDecisions.map((decision) => {
                  const efficiencyDelta =
                    Number(decision.selected_executable_efficiency) -
                    Number(decision.replaced_executable_efficiency ?? 0);
                  const rowTone =
                    decision.action === "replace" && efficiencyDelta >= 0.2
                      ? "bg-success/10"
                      : decision.action === "replace" && efficiencyDelta >= 0.1
                        ? "bg-warning/10"
                        : "";
                  return (
                  <tr
                    key={`${decision.recorded_at}-${decision.asset}-${decision.selected_question}`}
                    className={`cursor-pointer hover ${rowTone} ${
                      activeDecisionFocus?.asset === decision.asset &&
                      activeDecisionFocus.direction === decision.direction
                        ? "ring-1 ring-info bg-info/10"
                        : ""
                    }`}
                    onClick={() => {
                      setActiveDecisionFocus({
                        asset: decision.asset,
                        direction: decision.direction,
                      });
                      setExpandedAssets((current) => ({
                        ...current,
                        [`${decision.asset}::${decision.direction}`]: true,
                      }));
                    }}
                  >
                    <td className="text-xs opacity-70">
                      {new Date(decision.recorded_at).toLocaleString("zh-CN")}
                    </td>
                    <td>
                      <div>{decision.asset}</div>
                      <div className="text-[11px] opacity-60">{decision.direction}</div>
                    </td>
                    <td className="text-xs">
                      {decision.event_category ? (
                        <>
                          <div>{decision.event_category}</div>
                          <div className="opacity-60">{decision.event_subtype ?? "-"}</div>
                          <div className="opacity-50">
                            {decision.event_context_source ?? "-"}
                          </div>
                          {decision.event_title ? (
                            <div className="truncate opacity-50" title={decision.event_title}>
                              {decision.event_title}
                            </div>
                          ) : null}
                        </>
                      ) : (
                        <span className="opacity-40">-</span>
                      )}
                    </td>
                    <td>
                      <div className="flex flex-col gap-1">
                        <span
                          className={`badge badge-xs ${
                            decision.action === "replace"
                              ? "badge-warning"
                              : decision.action === "reject" || decision.action === "gate_reject"
                                ? "badge-error"
                                : "badge-ghost"
                          }`}
                        >
                          {decision.action}
                        </span>
                        <span className="text-[11px] opacity-60">{decisionReasonLabel(decision.reason)}</span>
                      </div>
                    </td>
                    <td className="max-w-xs truncate" title={decision.selected_question}>
                      <div>{decision.selected_question}</div>
                      <div className="text-[11px] opacity-60">
                        利润 ${Number(decision.selected_estimated_profit).toFixed(2)} · 静态效率{" "}
                        {Number(decision.selected_efficiency).toFixed(3)}
                      </div>
                    </td>
                    <td className="max-w-xs truncate" title={decision.replaced_question ?? "-"}>
                      {decision.replaced_question ? (
                        <>
                          <div>{decision.replaced_question}</div>
                          <div className="text-[11px] opacity-60">
                            利润 ${Number(decision.replaced_estimated_profit ?? 0).toFixed(2)} · 静态效率{" "}
                            {Number(decision.replaced_efficiency ?? 0).toFixed(3)}
                          </div>
                        </>
                      ) : (
                        <span className="opacity-40">-</span>
                      )}
                    </td>
                    <td className="text-xs">
                      <div>
                        保留{" "}
                        {`${(Number(decision.selected_executable_profit_retention) * 100).toFixed(0)}%`}
                      </div>
                      <div className="opacity-60">
                        替换{" "}
                        {decision.replaced_executable_profit_retention
                          ? `${(Number(decision.replaced_executable_profit_retention) * 100).toFixed(0)}%`
                          : "-"}
                      </div>
                    </td>
                    <td className="text-xs">
                      <div>
                        保留{" "}
                        {`${(Number(decision.selected_executable_size_retention) * 100).toFixed(0)}%`}
                      </div>
                      <div className="opacity-60">
                        替换{" "}
                        {decision.replaced_executable_size_retention
                          ? `${(Number(decision.replaced_executable_size_retention) * 100).toFixed(0)}%`
                          : "-"}
                      </div>
                    </td>
                    <td className="text-xs">
                      <div>保留 {Number(decision.selected_executable_quality_score).toFixed(3)}</div>
                      <div className="opacity-60">
                        替换{" "}
                        {decision.replaced_executable_quality_score
                          ? Number(decision.replaced_executable_quality_score).toFixed(3)
                          : "-"}
                      </div>
                    </td>
                    <td className="text-xs">
                      <div>保留 {Number(decision.selected_executable_efficiency).toFixed(3)}</div>
                      <div className="opacity-60">
                        替换{" "}
                        {decision.replaced_executable_efficiency
                          ? Number(decision.replaced_executable_efficiency).toFixed(3)
                          : "-"}
                      </div>
                    </td>
                    <td className="text-xs">
                      <div>保留 {Number(decision.selected_depth_buffer).toFixed(2)}x</div>
                      <div className="opacity-60">
                        替换{" "}
                        {decision.replaced_depth_buffer
                          ? `${Number(decision.replaced_depth_buffer).toFixed(2)}x`
                          : "-"}
                      </div>
                    </td>
                  </tr>
                )})}
                {visibleDecisions.length === 0 && (
                  <tr><td colSpan={11} className="text-center opacity-50">暂无最近候选决策</td></tr>
                )}
              </tbody>
            </table>
          </div>
          <div className="mt-2 text-[11px] opacity-60">
            `replace` 行会按可执行效率差值高亮：差值 ≥ 0.20 为高亮，≥ 0.10 为中等提示。
          </div>
        </div>
      </div>

      {/* Positions table */}
      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <h2 className="card-title text-base">当前持仓</h2>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>市场</th>
                  <th>资产</th>
                  <th>方向</th>
                  <th>数量</th>
                  <th>均价</th>
                  <th>成本</th>
                  <th>当前价</th>
                  <th>盈亏</th>
                </tr>
              </thead>
              <tbody>
                {(positions ?? []).map((p) => (
                  <tr
                    key={p.token_id}
                    className={
                      activeDecisionFocus?.asset === (p.asset ?? "未识别") &&
                      activeDecisionFocus.direction === directionLabel(p.direction)
                        ? "bg-info/10"
                        : ""
                    }
                  >
                    <td className="max-w-xs truncate" title={p.question ?? p.token_id}>
                      {p.question ?? p.token_id.slice(0, 14) + "..."}
                    </td>
                    <td>{p.asset ?? "-"}</td>
                    <td>
                      <span className={`badge badge-sm ${p.outcome === "YES" ? "badge-success" : "badge-error"}`}>
                        {p.outcome ?? "-"}
                      </span>
                    </td>
                    <td>{Number(p.size).toFixed(1)}</td>
                    <td>${Number(p.avg_cost).toFixed(3)}</td>
                    <td>${Number(p.cost_basis).toFixed(2)}</td>
                    <td>{p.current_price ? `$${Number(p.current_price).toFixed(3)}` : "-"}</td>
                    <td className={Number(p.unrealized_pnl ?? 0) >= 0 ? "text-success" : "text-error"}>
                      {p.unrealized_pnl ? `$${Number(p.unrealized_pnl).toFixed(3)}` : "-"}
                    </td>
                  </tr>
                ))}
                {(!positions || positions.length === 0) && (
                  <tr><td colSpan={8} className="text-center opacity-50">暂无持仓</td></tr>
                )}
              </tbody>
            </table>
          </div>
        </div>
      </div>
    </div>
  );
}

function MetricCard({ label, value }: { label: string; value?: number | string }) {
  return (
    <div className="stat bg-base-200 rounded-box p-4">
      <div className="stat-title text-xs">{label}</div>
      <div className="stat-value text-lg">{value != null ? value : "-"}</div>
    </div>
  );
}
