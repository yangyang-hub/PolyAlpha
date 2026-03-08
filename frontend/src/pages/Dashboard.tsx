import { useEffect, useState } from "react";
import { fetchHealth, fetchStatus, type HealthResponse, type StatusResponse } from "../api";

export default function Dashboard() {
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    try {
      const [h, s] = await Promise.all([fetchHealth(), fetchStatus()]);
      setHealth(h);
      setStatus(s);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 10000);
    return () => clearInterval(id);
  }, []);

  if (error) {
    return (
      <div className="alert alert-error">
        <span>无法连接机器人 API: {error}</span>
      </div>
    );
  }

  if (!health || !status) {
    return <span className="loading loading-spinner loading-lg" />;
  }

  const uptimeHours = Math.floor(status.uptime_seconds / 3600);
  const uptimeMinutes = Math.floor((status.uptime_seconds % 3600) / 60);

  return (
    <div>
      <h1 className="text-2xl font-bold mb-6">概览</h1>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-4 mb-6">
        {/* Status card */}
        <div className="stat bg-base-100 shadow-sm rounded-box">
          <div className="stat-title">运行状态</div>
          <div className={`stat-value text-lg ${health.status === "healthy" ? "text-success" : "text-warning"}`}>
            {health.status === "healthy" ? "正常" : health.status}
          </div>
          <div className="stat-desc">
            运行时间: {uptimeHours}小时 {uptimeMinutes}分钟
          </div>
        </div>

        {/* Strategies card */}
        <div className="stat bg-base-100 shadow-sm rounded-box">
          <div className="stat-title">活跃策略</div>
          <div className="stat-value text-lg">{status.enabled_strategies.length}</div>
          <div className="stat-desc">
            {status.enabled_strategies.join(", ") || "无"}
          </div>
        </div>

        {/* Scan interval card */}
        <div className="stat bg-base-100 shadow-sm rounded-box">
          <div className="stat-title">扫描间隔</div>
          <div className="stat-value text-lg">{status.scan_interval_ms}ms</div>
          <div className="stat-desc">
            流动性奖励: {status.lr_enabled ? "已启用" : "已禁用"} | 事件日历: {status.event_calendar_enabled ? "已启用" : "已禁用"}
          </div>
        </div>
      </div>

      {/* Health checks */}
      <div className="card bg-base-100 shadow-sm">
        <div className="card-body">
          <h2 className="card-title">健康检查</h2>
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
                    <td className="font-mono">{name}</td>
                    <td>
                      <span className={`badge ${val === "ok" ? "badge-success" : "badge-error"}`}>
                        {val === "ok" ? "正常" : val}
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
