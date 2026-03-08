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
        <span>Cannot connect to bot API: {error}</span>
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
      <h1 className="text-2xl font-bold mb-6">Dashboard</h1>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-4 mb-6">
        {/* Status card */}
        <div className="stat bg-base-100 shadow-sm rounded-box">
          <div className="stat-title">Status</div>
          <div className={`stat-value text-lg ${health.status === "healthy" ? "text-success" : "text-warning"}`}>
            {health.status}
          </div>
          <div className="stat-desc">
            Uptime: {uptimeHours}h {uptimeMinutes}m
          </div>
        </div>

        {/* Strategies card */}
        <div className="stat bg-base-100 shadow-sm rounded-box">
          <div className="stat-title">Active Strategies</div>
          <div className="stat-value text-lg">{status.enabled_strategies.length}</div>
          <div className="stat-desc">
            {status.enabled_strategies.join(", ") || "None"}
          </div>
        </div>

        {/* Scan interval card */}
        <div className="stat bg-base-100 shadow-sm rounded-box">
          <div className="stat-title">Scan Interval</div>
          <div className="stat-value text-lg">{status.scan_interval_ms}ms</div>
          <div className="stat-desc">
            LR: {status.lr_enabled ? "enabled" : "disabled"} | Calendar: {status.event_calendar_enabled ? "enabled" : "disabled"}
          </div>
        </div>
      </div>

      {/* Health checks */}
      <div className="card bg-base-100 shadow-sm">
        <div className="card-body">
          <h2 className="card-title">Health Checks</h2>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>Check</th>
                  <th>Status</th>
                </tr>
              </thead>
              <tbody>
                {Object.entries(health.checks).map(([name, val]) => (
                  <tr key={name}>
                    <td className="font-mono">{name}</td>
                    <td>
                      <span className={`badge ${val === "ok" ? "badge-success" : "badge-error"}`}>
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
