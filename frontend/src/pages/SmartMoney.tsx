import { useCallback } from "react";
import { fetchPositions, fetchSection, fetchStatus, type PositionEntry, type StatusResponse } from "../api";
import { usePolling } from "../hooks/usePolling";

interface Wallet {
  address: string;
  label: string;
  weight: number;
}

export default function SmartMoney() {
  const posFetcher = useCallback(() => fetchPositions("smart_money"), []);
  const configFetcher = useCallback(() => fetchSection("smart_money"), []);
  const statusFetcher = useCallback(() => fetchStatus(), []);
  const { data: positions, loading: posLoading } = usePolling<PositionEntry[]>(posFetcher, 15000);
  const { data: config } = usePolling<Record<string, unknown>>(configFetcher, 60000);
  const { data: status } = usePolling<StatusResponse>(statusFetcher, 15000);
  const strategyAccounts = status?.accounts.filter((account) => account.strategies.includes("smart_money")) ?? [];
  const wallets: Wallet[] = Array.isArray(config?.wallets) ? (config.wallets as Wallet[]) : [];

  const totalCost = (positions ?? []).reduce((s, p) => s + Number(p.cost_basis), 0);
  const totalPnl = (positions ?? []).reduce((s, p) => s + Number(p.unrealized_pnl ?? 0), 0);
  const marketCount = new Set((positions ?? []).map((p) => p.condition_id).filter(Boolean)).size;

  if (posLoading && !positions) {
    return (
      <div className="flex justify-center items-center h-64">
        <span className="loading loading-spinner loading-lg" />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-bold">跟单交易</h1>

      {/* Portfolio overview */}
      {status && (() => {
        const financials = status.strategy_financials?.smart_money;
        const cash = Number(financials?.wallet_balance ?? 0);
        const posValue = Number(financials?.positions_market_value ?? 0);
        const portfolio = Number(financials?.portfolio_value ?? cash + posValue);
        const realizedPnl = Number(financials?.realized_pnl ?? 0);
        return (
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
              <div className={`stat-value text-lg ${realizedPnl >= 0 ? "text-success" : "text-error"}`}>
                ${realizedPnl.toFixed(2)}
              </div>
            </div>
          </div>
        );
      })()}

      {status && (
        <div className="card bg-base-200 shadow-sm">
          <div className="card-body p-4">
            <h2 className="card-title text-base">运行上下文</h2>
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3 text-sm">
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

      {/* Summary stats */}
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
          <div className="stat-title text-xs">总成本</div>
          <div className="stat-value text-lg">${totalCost.toFixed(2)}</div>
        </div>
        <div className="stat bg-base-200 rounded-box p-4">
          <div className="stat-title text-xs">未实现盈亏</div>
          <div className={`stat-value text-lg ${totalPnl >= 0 ? "text-success" : "text-error"}`}>
            ${totalPnl.toFixed(2)}
          </div>
        </div>
      </div>

      {/* Tracked wallets (read-only from config) */}
      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <h2 className="card-title text-base">跟踪钱包</h2>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>地址</th>
                  <th>标签</th>
                  <th>权重</th>
                </tr>
              </thead>
              <tbody>
                {wallets.map((w, i) => (
                  <tr key={i}>
                    <td className="font-mono text-xs">{w.address.slice(0, 10)}...{w.address.slice(-6)}</td>
                    <td>{w.label || "-"}</td>
                    <td>{w.weight}</td>
                  </tr>
                ))}
                {wallets.length === 0 && (
                  <tr><td colSpan={3} className="text-center opacity-50">暂无跟踪钱包</td></tr>
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
