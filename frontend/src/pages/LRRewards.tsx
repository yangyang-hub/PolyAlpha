import { useCallback } from "react";
import { fetchLRStatus, fetchPositions, type LrRuntimeStatus, type PositionEntry } from "../api";
import { usePolling } from "../hooks/usePolling";

export default function LRRewards() {
  const statusFetcher = useCallback(() => fetchLRStatus(), []);
  const posFetcher = useCallback(() => fetchPositions("liquidity_rewards"), []);
  const { data: lrStatus, loading: statusLoading } = usePolling<LrRuntimeStatus>(statusFetcher, 15000);
  const { data: positions } = usePolling<PositionEntry[]>(posFetcher, 15000);

  const totalCost = (positions ?? []).reduce((s, p) => s + Number(p.cost_basis), 0);
  const totalPnl = (positions ?? []).reduce((s, p) => s + Number(p.unrealized_pnl ?? 0), 0);

  if (statusLoading && !lrStatus) {
    return (
      <div className="flex justify-center items-center h-64">
        <span className="loading loading-spinner loading-lg" />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-bold">LR 流动性奖励</h1>

      {/* Runtime status */}
      {lrStatus && (
        <div className="grid grid-cols-2 sm:grid-cols-4 gap-4">
          <div className="stat bg-base-200 rounded-box p-4">
            <div className="stat-title text-xs">活跃市场</div>
            <div className="stat-value text-lg">{lrStatus.active_markets.length}</div>
          </div>
          <div className="stat bg-base-200 rounded-box p-4">
            <div className="stat-title text-xs">总敞口</div>
            <div className="stat-value text-lg">{lrStatus.total_exposure} <span className="text-sm font-normal opacity-60">USD</span></div>
          </div>
          <div className="stat bg-base-200 rounded-box p-4">
            <div className="stat-title text-xs">缓存余额</div>
            <div className="stat-value text-lg">{lrStatus.cached_balance} <span className="text-sm font-normal opacity-60">USD</span></div>
          </div>
          <div className="stat bg-base-200 rounded-box p-4">
            <div className="stat-title text-xs">市场模式</div>
            <div className="stat-value text-lg">{lrStatus.market_mode}</div>
          </div>
        </div>
      )}

      {/* Active markets table */}
      {lrStatus && lrStatus.active_markets.length > 0 && (
        <div className="card bg-base-200 shadow-sm">
          <div className="card-body p-4">
            <h2 className="card-title text-base">活跃市场</h2>
            <div className="overflow-x-auto">
              <table className="table table-sm">
                <thead>
                  <tr>
                    <th>市场</th>
                    <th>日费率</th>
                    <th>挂单数</th>
                    <th>YES (买/卖)</th>
                    <th>NO (买/卖)</th>
                  </tr>
                </thead>
                <tbody>
                  {lrStatus.active_markets.map((m) => (
                    <tr key={m.condition_id}>
                      <td className="max-w-xs truncate" title={m.question}>{m.question}</td>
                      <td>{Number(m.daily_rate).toFixed(2)}</td>
                      <td>{m.outstanding_orders}</td>
                      <td>{fmtPrice(m.yes_bid)} / {fmtPrice(m.yes_ask)}</td>
                      <td>{fmtPrice(m.no_bid)} / {fmtPrice(m.no_ask)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>
        </div>
      )}

      {/* Positions */}
      {positions && positions.length > 0 && (
        <div className="card bg-base-200 shadow-sm">
          <div className="card-body p-4">
            <div className="flex items-center justify-between">
              <h2 className="card-title text-base">当前持仓</h2>
              <div className="flex gap-4 text-sm">
                <span>成本: <strong>${totalCost.toFixed(2)}</strong></span>
                <span className={totalPnl >= 0 ? "text-success" : "text-error"}>
                  盈亏: <strong>${totalPnl.toFixed(2)}</strong>
                </span>
              </div>
            </div>
            <div className="overflow-x-auto">
              <table className="table table-sm">
                <thead>
                  <tr>
                    <th>市场</th>
                    <th>方向</th>
                    <th>数量</th>
                    <th>均价</th>
                    <th>当前价</th>
                    <th>盈亏</th>
                  </tr>
                </thead>
                <tbody>
                  {positions.map((p) => (
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
                      <td>{p.current_price ? `$${Number(p.current_price).toFixed(3)}` : "-"}</td>
                      <td className={Number(p.unrealized_pnl ?? 0) >= 0 ? "text-success" : "text-error"}>
                        {p.unrealized_pnl ? `$${Number(p.unrealized_pnl).toFixed(3)}` : "-"}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>
        </div>
      )}

      {lrStatus && lrStatus.last_refresh && (
        <div className="text-xs opacity-50 text-right">
          最后刷新: {new Date(lrStatus.last_refresh).toLocaleString()}
        </div>
      )}
    </div>
  );
}

function fmtPrice(v: string | null): string {
  if (v == null) return "-";
  return Number(v).toFixed(3);
}
