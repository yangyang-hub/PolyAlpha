import { useEffect, useState, useCallback } from "react";
import { fetchLRStatus, fetchSection, updateSection, LrRuntimeStatus } from "../api";

interface LrConfig {
  enabled: boolean;
  max_markets: number;
  max_position_per_market: number;
  max_total_exposure: number;
  market_refresh_secs: number;
  quote_refresh_secs: number;
  spread_fraction: number;
  min_order_size: number;
  min_daily_rate: number;
  market_mode: string;
  manual_markets: ManualMarket[];
  allow_neg_risk: boolean;
  failed_cooldown_secs: number;
  verify_scoring: boolean;
  quote_yes: boolean;
  quote_no: boolean;
  [key: string]: unknown;
}

interface ManualMarket {
  condition_id: string;
  max_position_per_market?: number | null;
  spread_fraction?: number | null;
  quote_yes?: boolean | null;
  quote_no?: boolean | null;
  order_depth_level?: number | null;
}

export default function LRMarkets() {
  const [status, setStatus] = useState<LrRuntimeStatus | null>(null);
  const [config, setConfig] = useState<LrConfig | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [newCid, setNewCid] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [s, c] = await Promise.all([
        fetchLRStatus().catch(() => null),
        fetchSection("liquidity_rewards"),
      ]);
      setStatus(s);
      setConfig(c as unknown as LrConfig);
    } catch (e) {
      setError(String(e));
    }
    setLoading(false);
  }, []);

  useEffect(() => {
    load();
    const interval = setInterval(load, 15000);
    return () => clearInterval(interval);
  }, [load]);

  const saveConfig = async (updates: Partial<LrConfig>) => {
    if (!config) return;
    setSaving(true);
    setError(null);
    setSuccess(null);
    try {
      const merged = { ...config, ...updates };
      await updateSection("liquidity_rewards", merged as unknown as Record<string, unknown>);
      setConfig(merged);
      setSuccess("saved");
      setTimeout(() => setSuccess(null), 2000);
    } catch (e) {
      setError(String(e));
    }
    setSaving(false);
  };

  const setMarketMode = (mode: string) => saveConfig({ market_mode: mode });

  const addManualMarket = () => {
    if (!newCid.trim() || !config) return;
    const exists = config.manual_markets.some((m) => m.condition_id === newCid.trim());
    if (exists) {
      setError("already exists");
      return;
    }
    saveConfig({
      manual_markets: [
        ...config.manual_markets,
        { condition_id: newCid.trim() },
      ],
    });
    setNewCid("");
  };

  const removeManualMarket = (cid: string) => {
    if (!config) return;
    saveConfig({
      manual_markets: config.manual_markets.filter((m) => m.condition_id !== cid),
    });
  };

  if (loading) return <span className="loading loading-spinner loading-lg" />;

  return (
    <div>
      <h1 className="text-2xl font-bold mb-6">LR</h1>

      {error && (
        <div className="alert alert-error mb-4">
          <span>{error}</span>
          <button className="btn btn-sm btn-ghost" onClick={() => setError(null)}>x</button>
        </div>
      )}
      {success && (
        <div className="alert alert-success mb-4">
          <span>{success}</span>
        </div>
      )}

      {/* Runtime Status */}
      <div className="card bg-base-200 mb-6">
        <div className="card-body">
          <h2 className="card-title">Status</h2>
          {status && !status.error ? (
            <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
              <div className="stat bg-base-100 rounded-lg p-3">
                <div className="stat-title">market</div>
                <div className="stat-value text-lg">{status.active_markets.length}</div>
              </div>
              <div className="stat bg-base-100 rounded-lg p-3">
                <div className="stat-title">exposure</div>
                <div className="stat-value text-lg">${Number(status.total_exposure).toFixed(2)}</div>
              </div>
              <div className="stat bg-base-100 rounded-lg p-3">
                <div className="stat-title">balance</div>
                <div className="stat-value text-lg">${Number(status.cached_balance).toFixed(2)}</div>
              </div>
              <div className="stat bg-base-100 rounded-lg p-3">
                <div className="stat-title">mode</div>
                <div className="stat-value text-lg">{status.market_mode}</div>
              </div>
            </div>
          ) : (
            <p className="text-base-content/60">LR not running</p>
          )}
        </div>
      </div>

      {/* Market Mode */}
      {config && (
        <div className="card bg-base-200 mb-6">
          <div className="card-body">
            <h2 className="card-title">market select</h2>
            <div className="flex gap-2">
              {["auto", "manual", "hybrid"].map((m) => (
                <button
                  key={m}
                  className={`btn btn-sm ${config.market_mode === m ? "btn-primary" : "btn-outline"}`}
                  onClick={() => setMarketMode(m)}
                  disabled={saving}
                >
                  {m}
                </button>
              ))}
            </div>
            <p className="text-sm text-base-content/60 mt-1">
              {config.market_mode === "auto"
                ? "auto discover by reward density"
                : config.market_mode === "manual"
                  ? "manual market list only"
                  : "auto discover + manual markets always included"}
            </p>
          </div>
        </div>
      )}

      {/* Manual Markets */}
      {config && (config.market_mode === "manual" || config.market_mode === "hybrid") && (
        <div className="card bg-base-200 mb-6">
          <div className="card-body">
            <h2 className="card-title">manual market</h2>

            {/* Add market */}
            <div className="flex gap-2 mb-4">
              <input
                type="text"
                className="input input-bordered input-sm flex-1"
                placeholder="condition_id (0x...)"
                value={newCid}
                onChange={(e) => setNewCid(e.target.value)}
              />
              <button
                className="btn btn-sm btn-primary"
                onClick={addManualMarket}
                disabled={saving || !newCid.trim()}
              >
                +
              </button>
            </div>

            {/* List */}
            {config.manual_markets.length === 0 ? (
              <p className="text-sm text-base-content/60">no manual market</p>
            ) : (
              <div className="overflow-x-auto">
                <table className="table table-sm">
                  <thead>
                    <tr>
                      <th>Condition ID</th>
                      <th>max_position</th>
                      <th>spread</th>
                      <th></th>
                    </tr>
                  </thead>
                  <tbody>
                    {config.manual_markets.map((m) => (
                      <tr key={m.condition_id}>
                        <td className="font-mono text-xs">
                          {m.condition_id.length > 20
                            ? `${m.condition_id.slice(0, 10)}...${m.condition_id.slice(-8)}`
                            : m.condition_id}
                        </td>
                        <td>{m.max_position_per_market ?? "-"}</td>
                        <td>{m.spread_fraction ?? "-"}</td>
                        <td>
                          <button
                            className="btn btn-xs btn-ghost text-error"
                            onClick={() => removeManualMarket(m.condition_id)}
                            disabled={saving}
                          >
                            x
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </div>
        </div>
      )}

      {/* Active Markets Table */}
      {status && status.active_markets && status.active_markets.length > 0 && (
        <div className="card bg-base-200 mb-6">
          <div className="card-body">
            <h2 className="card-title">active market ({status.active_markets.length})</h2>
            <div className="overflow-x-auto">
              <table className="table table-sm">
                <thead>
                  <tr>
                    <th>Market</th>
                    <th>Daily Rate</th>
                    <th>Orders</th>
                    <th>Condition ID</th>
                  </tr>
                </thead>
                <tbody>
                  {status.active_markets.map((m) => (
                    <tr key={m.condition_id}>
                      <td className="max-w-xs truncate">{m.question}</td>
                      <td>${Number(m.daily_rate).toFixed(2)}</td>
                      <td>{m.outstanding_orders}</td>
                      <td className="font-mono text-xs">
                        {m.condition_id.length > 20
                          ? `${m.condition_id.slice(0, 10)}...${m.condition_id.slice(-8)}`
                          : m.condition_id}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {status.last_refresh && (
              <p className="text-xs text-base-content/40 mt-2">
                last update: {new Date(status.last_refresh).toLocaleString()}
              </p>
            )}
          </div>
        </div>
      )}

      {/* Quick Config Toggles */}
      {config && (
        <div className="card bg-base-200 mb-6">
          <div className="card-body">
            <h2 className="card-title">config</h2>
            <div className="grid grid-cols-2 md:grid-cols-3 gap-4">
              <label className="flex items-center gap-2">
                <input
                  type="checkbox"
                  className="toggle toggle-sm"
                  checked={config.allow_neg_risk}
                  onChange={(e) => saveConfig({ allow_neg_risk: e.target.checked })}
                />
                <span className="text-sm">NegRisk</span>
              </label>
              <label className="flex items-center gap-2">
                <input
                  type="checkbox"
                  className="toggle toggle-sm"
                  checked={config.verify_scoring}
                  onChange={(e) => saveConfig({ verify_scoring: e.target.checked })}
                />
                <span className="text-sm">scoring check</span>
              </label>
              <label className="flex items-center gap-2">
                <input
                  type="checkbox"
                  className="toggle toggle-sm"
                  checked={config.quote_yes}
                  onChange={(e) => saveConfig({ quote_yes: e.target.checked })}
                />
                <span className="text-sm">YES</span>
              </label>
              <label className="flex items-center gap-2">
                <input
                  type="checkbox"
                  className="toggle toggle-sm"
                  checked={config.quote_no}
                  onChange={(e) => saveConfig({ quote_no: e.target.checked })}
                />
                <span className="text-sm">NO</span>
              </label>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
