import { Fragment, useCallback, useState } from "react";
import {
  fetchCryptoAlphaConfig,
  fetchPositions,
  fetchStatus,
  type CryptoAlphaConfigSection,
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

export default function CryptoMarkets() {
  const [expandedAssets, setExpandedAssets] = useState<Record<string, boolean>>({});
  const posFetcher = useCallback(() => fetchPositions("crypto_alpha"), []);
  const configFetcher = useCallback(() => fetchCryptoAlphaConfig(), []);
  const statusFetcher = useCallback(() => fetchStatus(), []);
  const { data: positions, loading } = usePolling<PositionEntry[]>(posFetcher, 15000);
  const { data: config } = usePolling<CryptoAlphaConfigSection>(configFetcher, 30000);
  const { data: status } = usePolling<StatusResponse>(statusFetcher, 15000);

  const totalCost = (positions ?? []).reduce((s, p) => s + Number(p.cost_basis), 0);
  const totalPnl = (positions ?? []).reduce((s, p) => s + Number(p.unrealized_pnl ?? 0), 0);
  const totalMarkValue = (positions ?? []).reduce(
    (sum, p) => sum + Number(p.current_price ?? 0) * Number(p.size),
    0,
  );
  const walletBalance = Number(status?.wallet_balance ?? 0);
  const marketCount = new Set((positions ?? []).map((p) => p.condition_id).filter(Boolean)).size;
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
                  return (
                    <Fragment key={asset.key}>
                      <tr
                        className={`cursor-pointer hover ${concentrationTone}`}
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
                          <tr key={`${asset.key}-${position.token_id}`} className="bg-base-100/50">
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
                  <tr key={p.token_id}>
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
