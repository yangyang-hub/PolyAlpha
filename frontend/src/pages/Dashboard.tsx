import { useCallback } from "react";
import { usePolling } from "../hooks/usePolling";
import { parseMetrics } from "../lib/metrics";
import {
  fetchHealth,
  fetchStatus,
  fetchMetrics,
  type HealthResponse,
  type StatusResponse,
} from "../api";

interface DashboardData {
  health: HealthResponse;
  status: StatusResponse;
  metrics: Map<string, number>;
}

async function fetchAll(): Promise<DashboardData> {
  const [health, status, metricsText] = await Promise.all([
    fetchHealth(),
    fetchStatus(),
    fetchMetrics(),
  ]);
  return { health, status, metrics: parseMetrics(metricsText) };
}

function m(metrics: Map<string, number>, key: string): number {
  return metrics.get(key) ?? 0;
}

function formatUptime(secs: number): string {
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const min = Math.floor((secs % 3600) / 60);
  if (d > 0) return `${d}天 ${h}小时`;
  if (h > 0) return `${h}小时 ${min}分钟`;
  return `${min}分钟`;
}

export default function Dashboard() {
  const fetcher = useCallback(() => fetchAll(), []);
  const { data, error, loading } = usePolling(fetcher, 10000);

  if (loading && !data) {
    return (
      <div className="flex justify-center items-center h-64">
        <span className="loading loading-spinner loading-lg" />
      </div>
    );
  }

  if (error && !data) {
    return (
      <div className="alert alert-error">
        <span>无法连接后端: {error}</span>
      </div>
    );
  }

  if (!data) return null;

  const { health, status, metrics } = data;

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-bold">仪表板</h1>

      {/* Row 1: Status */}
      <div className="grid grid-cols-1 sm:grid-cols-3 gap-4">
        <StatCard
          label="健康状态"
          value={health.status}
          badge={health.status === "healthy" ? "badge-success" : "badge-warning"}
        />
        <StatCard label="运行时间" value={formatUptime(health.uptime_seconds)} />
        <StatCard label="策略数量" value={String(status.enabled_strategies.length)} />
      </div>

      {/* Row 2: Portfolio */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
        <MetricCard
          label="资产总值"
          value={(m(metrics, "usdc_balance") + m(metrics, "positions_market_value_usd")).toFixed(2)}
          unit="USD"
        />
        <MetricCard
          label="可用余额"
          value={m(metrics, "usdc_balance").toFixed(2)}
          unit="USD"
        />
        <MetricCard
          label="持仓市值"
          value={m(metrics, "positions_market_value_usd").toFixed(2)}
          unit="USD"
        />
        <MetricCard
          label="已实现收益"
          value={m(metrics, "realized_pnl_usd").toFixed(2)}
          unit="USD"
          color={m(metrics, "realized_pnl_usd") >= 0 ? "text-success" : "text-error"}
        />
      </div>

      {/* Row 3: Risk */}
      <div className="grid grid-cols-1 sm:grid-cols-3 gap-4">
        <MetricCard
          label="持仓成本"
          value={m(metrics, "total_exposure_usd").toFixed(2)}
          unit="USD"
        />
        <MetricCard
          label="熔断器"
          value={m(metrics, "circuit_breaker_active") > 0 ? "已触发" : "正常"}
          color={m(metrics, "circuit_breaker_active") > 0 ? "text-error" : "text-success"}
        />
        <MetricCard
          label="执行错误"
          value={String(m(metrics, "execution_errors_total"))}
          color={m(metrics, "execution_errors_total") > 0 ? "text-warning" : ""}
        />
      </div>

      {/* Row 4: Activity */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
        <MetricCard label="机会检测" value={String(m(metrics, "opportunities_detected_total"))} />
        <MetricCard label="执行次数" value={String(m(metrics, "executions_total"))} />
        <MetricCard label="退出交易" value={String(m(metrics, "exit_trades_total"))} />
        <MetricCard label="WS 重连" value={String(m(metrics, "ws_reconnect_total"))} />
      </div>

      {/* Row 5: Infrastructure */}
      <div className="grid grid-cols-1 sm:grid-cols-3 gap-4">
        <MetricCard label="WS 订阅" value={String(m(metrics, "active_ws_subscriptions"))} />
        <MetricCard label="监控市场" value={String(m(metrics, "monitored_markets"))} />
        <MetricCard label="WS 重连" value={String(m(metrics, "ws_reconnect_total"))} />
      </div>

      {/* Row 5: LR Metrics */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
        <MetricCard label="LR 挂单" value={String(m(metrics, "lr_orders_placed_total"))} />
        <MetricCard label="LR 取消" value={String(m(metrics, "lr_orders_cancelled_total"))} />
        <MetricCard label="LR 活跃市场" value={String(m(metrics, "lr_active_markets"))} />
        <MetricCard label="LR 成交" value={String(m(metrics, "lr_fills_detected_total"))} />
      </div>

      {/* Row 6: Strategy table */}
      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <h2 className="card-title text-base">启用策略</h2>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>策略</th>
                  <th>状态</th>
                </tr>
              </thead>
              <tbody>
                {status.enabled_strategies.map((s) => (
                  <tr key={s}>
                    <td>{s}</td>
                    <td><span className="badge badge-success badge-sm">启用</span></td>
                  </tr>
                ))}
                <tr>
                  <td>流动性奖励 (LR)</td>
                  <td>
                    <span className={`badge badge-sm ${status.lr_enabled ? "badge-success" : "badge-ghost"}`}>
                      {status.lr_enabled ? "启用" : "禁用"}
                    </span>
                  </td>
                </tr>
                <tr>
                  <td>事件日历</td>
                  <td>
                    <span className={`badge badge-sm ${status.event_calendar_enabled ? "badge-success" : "badge-ghost"}`}>
                      {status.event_calendar_enabled ? "启用" : "禁用"}
                    </span>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>
      </div>

      {/* Row 7: Health checks */}
      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <h2 className="card-title text-base">健康检查</h2>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>检查项</th>
                  <th>状态</th>
                </tr>
              </thead>
              <tbody>
                {Object.entries(health.checks).map(([name, val]) => (
                  <tr key={name}>
                    <td>{name}</td>
                    <td>
                      <span className={`badge badge-sm ${val === "ok" ? "badge-success" : "badge-error"}`}>
                        {val}
                      </span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      </div>
    </div>
  );
}

function StatCard({
  label,
  value,
  badge,
}: {
  label: string;
  value: string;
  badge?: string;
}) {
  return (
    <div className="stat bg-base-200 rounded-box p-4">
      <div className="stat-title text-xs">{label}</div>
      <div className="stat-value text-lg">
        {badge ? (
          <span className={`badge ${badge}`}>{value}</span>
        ) : (
          value
        )}
      </div>
    </div>
  );
}

function MetricCard({
  label,
  value,
  unit,
  color,
}: {
  label: string;
  value: string;
  unit?: string;
  color?: string;
}) {
  return (
    <div className="stat bg-base-200 rounded-box p-4">
      <div className="stat-title text-xs">{label}</div>
      <div className={`stat-value text-lg ${color ?? ""}`}>
        {value}
        {unit && <span className="text-sm font-normal opacity-60 ml-1">{unit}</span>}
      </div>
    </div>
  );
}
