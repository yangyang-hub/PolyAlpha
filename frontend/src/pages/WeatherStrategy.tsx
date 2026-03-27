import { useCallback } from "react";
import { fetchPositions, fetchMetrics, fetchStatus, type PositionEntry, type StatusResponse } from "../api";
import { usePolling } from "../hooks/usePolling";
import { metricSeriesByName, parseMetrics } from "../lib/metrics";

const REJECTION_LABELS: Record<string, string> = {
  unsupported_city: "城市不在当前天气源覆盖范围",
  geocode_failed: "城市坐标解析失败",
  forecast_fetch_failed: "天气预报拉取失败",
  forecast_not_fresh: "预报变化不显著",
  invalid_token_count: "市场 token 结构异常",
  missing_both_books: "双边 orderbook 都缺失",
  missing_yes_ask: "YES 卖盘缺失",
  missing_no_ask: "NO 卖盘缺失",
  missing_yes_bid: "YES 买盘缺失",
  missing_no_bid: "NO 买盘缺失",
  spread_too_wide: "价差过宽",
  price_above_max_entry: "价格高于入场上限",
  no_positive_edge: "模型没有正 edge",
  no_tradable_side: "没有可交易方向",
  edge_too_small: "edge 低于阈值",
  size_below_min_order: "下单金额低于最小要求",
  non_positive_profit: "预期净利润不为正",
};

const FRESHNESS_REJECTION_LABELS: Record<string, string> = {
  missing_orderbook: "执行前缺少 orderbook",
  missing_ask: "执行前无卖盘",
  ask_above_limit: "执行前 ask 已高于原限价",
  insufficient_ask_depth: "执行前 ask 深度不足",
  missing_bid: "执行前无买盘",
  insufficient_bid_depth: "执行前 bid 深度不足",
  non_positive_profit: "执行前重算后净利润不再为正",
  buy_lot_size_invalid: "执行前买单低于最小可执行手数",
  sell_lot_size_invalid: "执行前卖单低于最小可执行手数",
};

function weatherPriorityHint(reason: string | undefined): string {
  switch (reason) {
    case "spread_too_wide":
      return "当前主要被盘口价差挡住，优先看高质量城市的 spread 上限是否还偏紧。";
    case "price_above_max_entry":
      return "当前主要被价格上限挡住，优先看高质量城市的 max_entry_price 是否还偏低。";
    case "no_positive_edge":
      return "当前模型给出的有效概率优势不足，先别盲目放宽，优先继续观察高质量城市。";
    case "edge_too_small":
      return "当前更多是 edge 刚好不够，只有在 spread / 价格放宽后仍无单时，再考虑放概率阈值。";
    case "forecast_fetch_failed":
      return "当前有数据源拉取失败，优先排查 provider 限流或数据源稳定性。";
    case "no_tradable_side":
      return "当前不少市场缺少可交易方向，通常说明双边盘口质量仍然偏弱。";
    default:
      return "当前没有明显单一阻塞项，可以继续观察拒单分布是否开始变化。";
  }
}

export default function WeatherStrategy() {
  const posFetcher = useCallback(() => fetchPositions("weather"), []);
  const metricsFetcher = useCallback(() => fetchMetrics(), []);
  const statusFetcher = useCallback(() => fetchStatus(), []);
  const { data: positions, loading } = usePolling<PositionEntry[]>(posFetcher, 15000);
  const { data: metricsRaw } = usePolling<string>(metricsFetcher, 15000);
  const { data: status } = usePolling<StatusResponse>(statusFetcher, 15000);

  const strategyCounterValue = (name: string) =>
    metricsRaw
      ? metricSeriesByName(metricsRaw, name)
          .filter((sample) => sample.labels.strategy === "weather")
          .reduce((sum, sample) => sum + sample.value, 0)
      : undefined;

  const metrics = metricsRaw ? parseMetrics(metricsRaw) : null;
  const rejectionRows = metricsRaw
    ? Object.values(
        metricSeriesByName(metricsRaw, "weather_rejections_total").reduce<
          Record<string, { reason: string; value: number }>
        >((acc, sample) => {
          const reason = sample.labels.reason ?? "unknown";
          acc[reason] = {
            reason,
            value: (acc[reason]?.value ?? 0) + sample.value,
          };
          return acc;
        }, {}),
      )
        .filter((row) => row.value > 0)
        .sort((a, b) => b.value - a.value)
    : [];
  const freshnessRows = metricsRaw
    ? metricSeriesByName(metricsRaw, "execution_freshness_rejections_total")
        .filter((sample) => sample.labels.strategy === "weather")
        .map((sample) => ({
          reason: sample.labels.reason ?? "unknown",
          value: sample.value,
        }))
        .filter((row) => row.value > 0)
        .sort((a, b) => b.value - a.value)
    : [];
  const freshnessScaledSell = metricsRaw
    ? metricSeriesByName(metricsRaw, "execution_freshness_scaled_total")
        .filter((sample) => sample.labels.strategy === "weather" && sample.labels.side === "sell")
        .reduce((sum, sample) => sum + sample.value, 0)
    : 0;
  const retainedRows = status?.weather_rejection_summary?.retained_top ?? [];
  const topRejection = retainedRows[0] ? { reason: retainedRows[0].label, value: retainedRows[0].count } : rejectionRows[0];
  const secondRejection = retainedRows[1] ? { reason: retainedRows[1].label, value: retainedRows[1].count } : rejectionRows[1];
  const thirdRejection = retainedRows[2] ? { reason: retainedRows[2].label, value: retainedRows[2].count } : rejectionRows[2];
  const recent1hRows = status?.weather_rejection_summary?.recent_1h.top_reasons ?? [];
  const recent6hRows = status?.weather_rejection_summary?.recent_6h.top_reasons ?? [];
  const recent1hTop = recent1hRows[0];
  const recent6hTop = recent6hRows[0];
  const recent1hSpreadCities = status?.weather_rejection_summary?.recent_1h.top_spread_cities ?? [];
  const recent6hSpreadCities = status?.weather_rejection_summary?.recent_6h.top_spread_cities ?? [];
  const recent1hPriceCities = status?.weather_rejection_summary?.recent_1h.top_price_cities ?? [];
  const recent6hPriceCities = status?.weather_rejection_summary?.recent_6h.top_price_cities ?? [];

  const totalCost = (positions ?? []).reduce((s, p) => s + Number(p.cost_basis), 0);
  const totalPnl = (positions ?? []).reduce((s, p) => s + Number(p.unrealized_pnl ?? 0), 0);
  const marketCount = new Set((positions ?? []).map((p) => p.condition_id).filter(Boolean)).size;
  const strategyAccounts = status?.accounts.filter((account) => account.strategies.includes("weather")) ?? [];

  if (loading && !positions) {
    return (
      <div className="flex justify-center items-center h-64">
        <span className="loading loading-spinner loading-lg" />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-bold">天气策略</h1>

      {/* Strategy-level overview */}
      {metrics && (() => {
        const financials = status?.strategy_financials?.weather;
        const cash = Number(financials?.wallet_balance ?? 0);
        const posValue = Number(financials?.positions_market_value ?? 0);
        const portfolio = cash + posValue;
        const pnl = Number(financials?.realized_pnl ?? 0);
        return (
          <div className="space-y-2">
            <div className="text-xs opacity-60">
              仅统计启用了天气策略的钱包资金与天气持仓市值；“已实现收益”当前为 bot 进程启动以来的天气策略累计值，不是 Polymarket 官网全历史口径。
            </div>
            <div className="grid grid-cols-2 sm:grid-cols-4 gap-4">
            <div className="stat bg-base-200 rounded-box p-4">
              <div className="stat-title text-xs">资产总值</div>
              <div className="stat-value text-lg">
                ${portfolio.toFixed(2)}
              </div>
            </div>
            <div className="stat bg-base-200 rounded-box p-4">
              <div className="stat-title text-xs">可用余额</div>
              <div className="stat-value text-lg">
                ${cash.toFixed(2)}
              </div>
            </div>
            <div className="stat bg-base-200 rounded-box p-4">
              <div className="stat-title text-xs">持仓市值</div>
              <div className="stat-value text-lg">
                ${posValue.toFixed(2)}
              </div>
            </div>
            <div className="stat bg-base-200 rounded-box p-4">
              <div className="stat-title text-xs">已实现收益（进程内）</div>
              <div className={`stat-value text-lg ${pnl >= 0 ? "text-success" : "text-error"}`}>
                ${pnl.toFixed(2)}
              </div>
            </div>
            </div>
          </div>
        );
      })()}

      {status && (
        <div className="card bg-base-200 shadow-sm">
          <div className="card-body p-4">
            <div className="flex items-center justify-between gap-3">
              <h2 className="card-title text-base">运行上下文</h2>
              <span className="text-xs opacity-60">用于解释钱包口径和持仓快照来源</span>
            </div>
            <div className="grid grid-cols-1 sm:grid-cols-4 gap-3 text-sm">
              <div>
                <div className="opacity-60 text-xs">持仓快照更新时间</div>
                <div>{status.positions_snapshot_updated_at ? new Date(status.positions_snapshot_updated_at).toLocaleString("zh-CN") : "-"}</div>
              </div>
              <div>
                <div className="opacity-60 text-xs">账户</div>
                <div>{strategyAccounts.map((account) => account.name).join(", ") || "-"}</div>
              </div>
              <div>
                <div className="opacity-60 text-xs">代理钱包</div>
                <div className="font-mono text-xs break-all">
                  {strategyAccounts.map((account) => account.proxy_wallet || "(EOA)").join(", ") || "-"}
                </div>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Strategy stats */}
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-4">
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
          <div className="stat-title text-xs">未实现盈亏</div>
          <div className={`stat-value text-lg ${totalPnl >= 0 ? "text-success" : "text-error"}`}>
            ${totalPnl.toFixed(2)}
          </div>
        </div>
      </div>

      {/* Activity metrics */}
      {metrics && (
        <div className="card bg-base-200 shadow-sm">
          <div className="card-body p-4 space-y-3">
            <div className="flex items-center justify-between gap-3">
              <h2 className="card-title text-base">当前最常见阻塞</h2>
              <span className="text-xs opacity-60">
                基于后端保留窗口聚合的天气拒单计数
              </span>
            </div>
            <div className="text-sm">
              {weatherPriorityHint(recent1hTop?.label ?? recent6hTop?.label ?? topRejection?.reason)}
            </div>
            <div className="grid grid-cols-1 sm:grid-cols-3 gap-3 text-sm">
              {[topRejection, secondRejection, thirdRejection].map((row, index) => (
                <div key={row?.reason ?? index} className="rounded-lg bg-base-100 p-3">
                  <div className="text-xs opacity-60">Top {index + 1}</div>
                  <div className="font-medium">{row ? (REJECTION_LABELS[row.reason] ?? row.reason) : "-"}</div>
                  <div className="font-mono text-xs opacity-70">{row?.reason ?? "-"}</div>
                  <div className="mt-1 text-sm">{row ? row.value.toLocaleString() : "-"}</div>
                </div>
              ))}
            </div>
            <div className="text-xs opacity-60">
              当前保留窗口: 最近 {status?.weather_rejection_summary?.retained_window_minutes
                ? Math.round(status.weather_rejection_summary.retained_window_minutes / 60)
                : 12} 小时
            </div>
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3 text-sm">
              <div className="rounded-lg bg-base-100 p-3">
                <div className="text-xs opacity-60">最近 1 小时</div>
                <div className="font-medium">
                  {recent1hTop ? (REJECTION_LABELS[recent1hTop.label] ?? recent1hTop.label) : "暂无近期拒单"}
                </div>
                <div className="font-mono text-xs opacity-70">{recent1hTop?.label ?? "-"}</div>
                <div className="mt-1">{recent1hTop ? recent1hTop.count.toLocaleString() : "-"}</div>
              </div>
              <div className="rounded-lg bg-base-100 p-3">
                <div className="text-xs opacity-60">最近 6 小时</div>
                <div className="font-medium">
                  {recent6hTop ? (REJECTION_LABELS[recent6hTop.label] ?? recent6hTop.label) : "暂无近期拒单"}
                </div>
                <div className="font-mono text-xs opacity-70">{recent6hTop?.label ?? "-"}</div>
                <div className="mt-1">{recent6hTop ? recent6hTop.count.toLocaleString() : "-"}</div>
              </div>
            </div>
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3 text-sm">
              <div className="rounded-lg bg-base-100 p-3">
                <div className="text-xs opacity-60">最近 1 小时城市分布</div>
                <div className="space-y-2 mt-2">
                  <div>
                    <div className="text-xs opacity-60">价差过宽 Top 城市</div>
                    <div className="font-medium">
                      {recent1hSpreadCities[0]
                        ? `${recent1hSpreadCities[0].label} (${recent1hSpreadCities[0].count.toLocaleString()})`
                        : "暂无"}
                    </div>
                  </div>
                  <div>
                    <div className="text-xs opacity-60">价格过高 Top 城市</div>
                    <div className="font-medium">
                      {recent1hPriceCities[0]
                        ? `${recent1hPriceCities[0].label} (${recent1hPriceCities[0].count.toLocaleString()})`
                        : "暂无"}
                    </div>
                  </div>
                </div>
              </div>
              <div className="rounded-lg bg-base-100 p-3">
                <div className="text-xs opacity-60">最近 6 小时城市分布</div>
                <div className="space-y-2 mt-2">
                  <div>
                    <div className="text-xs opacity-60">价差过宽 Top 城市</div>
                    <div className="font-medium">
                      {recent6hSpreadCities[0]
                        ? `${recent6hSpreadCities[0].label} (${recent6hSpreadCities[0].count.toLocaleString()})`
                        : "暂无"}
                    </div>
                  </div>
                  <div>
                    <div className="text-xs opacity-60">价格过高 Top 城市</div>
                    <div className="font-medium">
                      {recent6hPriceCities[0]
                        ? `${recent6hPriceCities[0].label} (${recent6hPriceCities[0].count.toLocaleString()})`
                        : "暂无"}
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>
      )}

      {metrics && (
        <div className="grid grid-cols-2 sm:grid-cols-4 gap-4">
          <MetricCard label="机会检测" value={strategyCounterValue("opportunities_detected_by_strategy_total")} />
          <MetricCard label="执行次数" value={strategyCounterValue("executions_by_strategy_total")} />
          <MetricCard label="退出交易" value={strategyCounterValue("exit_trades_by_strategy_total")} />
          <MetricCard label="深度缩量" value={strategyCounterValue("depth_validation_scaled_by_strategy_total")} />
        </div>
      )}

      {metrics && (
        <div className="grid grid-cols-2 sm:grid-cols-4 gap-4">
          <MetricCard label="执行前拦截" value={freshnessRows.reduce((sum, row) => sum + row.value, 0)} />
          <MetricCard label="执行前卖出缩量" value={freshnessScaledSell} />
          <MetricCard label="策略拒单总数" value={rejectionRows.reduce((sum, row) => sum + row.value, 0)} />
          <MetricCard label="执行错误" value={strategyCounterValue("execution_errors_by_strategy_total")} />
        </div>
      )}

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <div className="flex items-center justify-between gap-3">
            <h2 className="card-title text-base">拒单原因</h2>
            <span className="text-xs opacity-60">来源: `/metrics` 中的 `weather_rejections_total`，已按 provider 聚合，累计计数（进程启动以来）</span>
          </div>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>原因</th>
                  <th>指标键</th>
                  <th className="text-right">次数</th>
                </tr>
              </thead>
              <tbody>
                {rejectionRows.map((row) => (
                  <tr key={row.reason}>
                    <td>{REJECTION_LABELS[row.reason] ?? row.reason}</td>
                    <td className="font-mono text-xs opacity-70">{row.reason}</td>
                    <td className="text-right">{row.value}</td>
                  </tr>
                ))}
                {rejectionRows.length === 0 && (
                  <tr>
                    <td colSpan={3} className="text-center opacity-50">
                      暂无拒单统计，或策略尚未完成一次有效扫描
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </div>
      </div>

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <div className="flex items-center justify-between gap-3">
            <h2 className="card-title text-base">执行前拦截</h2>
            <span className="text-xs opacity-60">
              来源: `/metrics` 中的 `execution_freshness_rejections_total`，累计计数（进程启动以来）
            </span>
          </div>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>原因</th>
                  <th>指标键</th>
                  <th className="text-right">次数</th>
                </tr>
              </thead>
              <tbody>
                {freshnessRows.map((row) => (
                  <tr key={row.reason}>
                    <td>{FRESHNESS_REJECTION_LABELS[row.reason] ?? row.reason}</td>
                    <td className="font-mono text-xs opacity-70">{row.reason}</td>
                    <td className="text-right">{row.value}</td>
                  </tr>
                ))}
                {freshnessRows.length === 0 && (
                  <tr>
                    <td colSpan={3} className="text-center opacity-50">
                      暂无执行前拦截统计
                    </td>
                  </tr>
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
                  <tr><td colSpan={7} className="text-center opacity-50">暂无持仓</td></tr>
                )}
              </tbody>
            </table>
          </div>
        </div>
      </div>
    </div>
  );
}

function MetricCard({ label, value }: { label: string; value?: number }) {
  return (
    <div className="stat bg-base-200 rounded-box p-4">
      <div className="stat-title text-xs">{label}</div>
      <div className="stat-value text-lg">{value != null ? value : "-"}</div>
    </div>
  );
}
