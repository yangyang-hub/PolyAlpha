import { useCallback } from "react";
import { fetchPositions, fetchMetrics, type PositionEntry } from "../api";
import { usePolling } from "../hooks/usePolling";
import { parseMetrics } from "../lib/metrics";

export default function CryptoMarkets() {
  const posFetcher = useCallback(() => fetchPositions("crypto_alpha"), []);
  const metricsFetcher = useCallback(() => fetchMetrics(), []);
  const { data: positions, loading } = usePolling<PositionEntry[]>(posFetcher, 15000);
  const { data: metricsRaw } = usePolling<string>(metricsFetcher, 15000);

  const metrics = metricsRaw ? parseMetrics(metricsRaw) : null;

  const totalCost = (positions ?? []).reduce((s, p) => s + Number(p.cost_basis), 0);
  const totalPnl = (positions ?? []).reduce((s, p) => s + Number(p.unrealized_pnl ?? 0), 0);
  const marketCount = new Set((positions ?? []).map((p) => p.condition_id).filter(Boolean)).size;

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

      {/* Account overview */}
      {metrics && (
        <div className="grid grid-cols-2 sm:grid-cols-4 gap-4">
          <div className="stat bg-base-200 rounded-box p-4">
            <div className="stat-title text-xs">USDC 余额</div>
            <div className="stat-value text-lg">
              {(metrics.get("usdc_balance") ?? 0).toFixed(2)}
              <span className="text-sm font-normal opacity-60 ml-1">USD</span>
            </div>
          </div>
          <div className="stat bg-base-200 rounded-box p-4">
            <div className="stat-title text-xs">总敞口</div>
            <div className="stat-value text-lg">
              {(metrics.get("total_exposure_usd") ?? 0).toFixed(2)}
              <span className="text-sm font-normal opacity-60 ml-1">USD</span>
            </div>
          </div>
          <div className="stat bg-base-200 rounded-box p-4">
            <div className="stat-title text-xs">已实现收益</div>
            <div className={`stat-value text-lg ${(metrics.get("realized_pnl_usd") ?? 0) >= 0 ? "text-success" : "text-error"}`}>
              {(metrics.get("realized_pnl_usd") ?? 0).toFixed(2)}
              <span className="text-sm font-normal opacity-60 ml-1">USD</span>
            </div>
          </div>
          <div className="stat bg-base-200 rounded-box p-4">
            <div className="stat-title text-xs">熔断器</div>
            <div className={`stat-value text-lg ${(metrics.get("circuit_breaker_active") ?? 0) > 0 ? "text-error" : "text-success"}`}>
              {(metrics.get("circuit_breaker_active") ?? 0) > 0 ? "已触发" : "正常"}
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
        <div className="grid grid-cols-2 sm:grid-cols-4 gap-4">
          <MetricCard label="机会检测" value={metrics.get("opportunities_detected_total")} />
          <MetricCard label="执行次数" value={metrics.get("executions_total")} />
          <MetricCard label="退出交易" value={metrics.get("exit_trades_total")} />
          <MetricCard label="深度拒绝" value={metrics.get("depth_validation_rejected_total")} />
        </div>
      )}

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
