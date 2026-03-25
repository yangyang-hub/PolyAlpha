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
  const signalSummary = status?.smart_money_signal_summary;
  const rejectSummary = status?.smart_money_gate_reject_summary;
  const exitSummary = status?.smart_money_exit_summary;
  const walletScores = status?.smart_money_wallet_scores ?? [];
  const recentDecisions = status?.smart_money_recent_decisions ?? [];
  const recentExits = status?.smart_money_recent_exits ?? [];
  const acceptRate = signalSummary?.recent_entry_attempts
    ? (signalSummary.recent_entry_accepted / signalSummary.recent_entry_attempts) * 100
    : 0;

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

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <div className="card bg-base-200 shadow-sm">
          <div className="card-body p-4">
            <h2 className="card-title text-base">最近信号</h2>
            <div className="grid grid-cols-2 gap-3 text-sm">
              <div>
                <div className="opacity-60 text-xs">总信号</div>
                <div className="text-lg font-semibold">{signalSummary?.recent_signal_count ?? 0}</div>
              </div>
              <div>
                <div className="opacity-60 text-xs">入场放行率</div>
                <div className="text-lg font-semibold">{acceptRate.toFixed(0)}%</div>
              </div>
              <div>
                <div className="opacity-60 text-xs">最近放行</div>
                <div>{signalSummary?.recent_entry_accepted ?? 0}</div>
              </div>
              <div>
                <div className="opacity-60 text-xs">最近拒绝</div>
                <div>{signalSummary?.recent_entry_rejected ?? 0}</div>
              </div>
            </div>
            <div className="mt-3 space-y-2 text-sm">
              <div>
                <div className="opacity-60 text-xs mb-1">共识钱包分布</div>
                <div className="flex flex-wrap gap-2">
                  {(signalSummary?.wallet_counts ?? []).slice(0, 4).map((entry) => (
                    <span key={entry.label} className="badge badge-outline badge-sm">
                      {entry.label}: {entry.count}
                    </span>
                  ))}
                  {!(signalSummary?.wallet_counts?.length) && <span className="opacity-50">暂无</span>}
                </div>
              </div>
              <div>
                <div className="opacity-60 text-xs mb-1">信号来源</div>
                <div className="flex flex-wrap gap-2">
                  {(signalSummary?.source_counts ?? []).map((entry) => (
                    <span key={entry.label} className="badge badge-outline badge-sm">
                      {entry.label}: {entry.count}
                    </span>
                  ))}
                  {!(signalSummary?.source_counts?.length) && <span className="opacity-50">暂无</span>}
                </div>
              </div>
            </div>
          </div>
        </div>

        <div className="card bg-base-200 shadow-sm">
          <div className="card-body p-4">
            <h2 className="card-title text-base">最近拒绝原因</h2>
            <div className="text-sm opacity-60 mb-3">
              最近被 gate 拒绝 {rejectSummary?.total_rejected ?? 0} 条
            </div>
            <div className="space-y-2">
              {(rejectSummary?.reason_counts ?? []).slice(0, 5).map((entry) => (
                <div key={entry.label} className="flex items-center justify-between text-sm">
                  <span>{entry.label}</span>
                  <span className="badge badge-sm badge-outline">{entry.count}</span>
                </div>
              ))}
              {!(rejectSummary?.reason_counts?.length) && (
                <div className="text-sm opacity-50">暂无拒绝记录</div>
              )}
            </div>
          </div>
        </div>
      </div>

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <h2 className="card-title text-base">最近退出原因</h2>
          <div className="text-sm opacity-60 mb-3">
            最近策略退出 {exitSummary?.total_exits ?? 0} 条
          </div>
          <div className="flex flex-wrap gap-2">
            {(exitSummary?.reason_counts ?? []).slice(0, 6).map((entry) => (
              <span key={entry.label} className="badge badge-outline badge-sm">
                {entry.label}: {entry.count}
              </span>
            ))}
            {!(exitSummary?.reason_counts?.length) && (
              <span className="opacity-50 text-sm">暂无退出记录</span>
            )}
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

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <h2 className="card-title text-base">钱包评分</h2>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>地址</th>
                  <th>标签</th>
                  <th>基础权重</th>
                  <th>动态权重</th>
                  <th>Profile Score</th>
                  <th>最近信号</th>
                  <th>来源</th>
                </tr>
              </thead>
              <tbody>
                {walletScores.map((wallet) => (
                  <tr key={wallet.address}>
                    <td className="font-mono text-xs">{wallet.address.slice(0, 10)}...{wallet.address.slice(-6)}</td>
                    <td>{wallet.label || "-"}</td>
                    <td>{Number(wallet.base_weight).toFixed(2)}</td>
                    <td>{Number(wallet.effective_weight).toFixed(2)}</td>
                    <td>{Number(wallet.profile_score).toFixed(3)}</td>
                    <td>{wallet.recent_signal_count}</td>
                    <td>{wallet.auto_discovered ? "auto" : "manual"}</td>
                  </tr>
                ))}
                {walletScores.length === 0 && (
                  <tr><td colSpan={7} className="text-center opacity-50">暂无钱包评分快照</td></tr>
                )}
              </tbody>
            </table>
          </div>
        </div>
      </div>

      <div className="grid grid-cols-1 xl:grid-cols-2 gap-4">
        <div className="card bg-base-200 shadow-sm">
          <div className="card-body p-4">
            <h2 className="card-title text-base">最近决策流水</h2>
            <div className="overflow-x-auto">
              <table className="table table-sm">
                <thead>
                  <tr>
                    <th>时间</th>
                    <th>信号</th>
                    <th>结果</th>
                    <th>钱包数</th>
                    <th>来源</th>
                  </tr>
                </thead>
                <tbody>
                  {recentDecisions.map((decision) => (
                    <tr key={`${decision.recorded_at}-${decision.token_id}-${decision.signal_type}`}>
                      <td className="text-xs whitespace-nowrap">{new Date(decision.recorded_at).toLocaleTimeString()}</td>
                      <td>{decision.signal_type}</td>
                      <td>
                        {decision.accepted ? (
                          <span className="badge badge-success badge-sm">accepted</span>
                        ) : (
                          <span className="badge badge-error badge-sm">{decision.reject_reason ?? "rejected"}</span>
                        )}
                      </td>
                      <td>{decision.wallet_count}</td>
                      <td className="text-xs">
                        {decision.source_data_api ? "data" : ""}
                        {decision.source_data_api && decision.source_onchain ? "+" : ""}
                        {decision.source_onchain ? "chain" : ""}
                      </td>
                    </tr>
                  ))}
                  {recentDecisions.length === 0 && (
                    <tr><td colSpan={5} className="text-center opacity-50">暂无决策记录</td></tr>
                  )}
                </tbody>
              </table>
            </div>
          </div>
        </div>

        <div className="card bg-base-200 shadow-sm">
          <div className="card-body p-4">
            <h2 className="card-title text-base">最近退出流水</h2>
            <div className="overflow-x-auto">
              <table className="table table-sm">
                <thead>
                  <tr>
                    <th>时间</th>
                    <th>原因</th>
                    <th>市场</th>
                    <th>Bid</th>
                    <th>成本</th>
                    <th>数量</th>
                  </tr>
                </thead>
                <tbody>
                  {recentExits.map((exit) => (
                    <tr key={`${exit.recorded_at}-${exit.token_id}-${exit.reason}`}>
                      <td className="text-xs whitespace-nowrap">{new Date(exit.recorded_at).toLocaleTimeString()}</td>
                      <td><span className="badge badge-outline badge-sm">{exit.reason}</span></td>
                      <td className="max-w-xs truncate" title={exit.question}>{exit.question}</td>
                      <td>{Number(exit.best_bid).toFixed(3)}</td>
                      <td>{Number(exit.avg_cost).toFixed(3)}</td>
                      <td>{Number(exit.size).toFixed(1)}</td>
                    </tr>
                  ))}
                  {recentExits.length === 0 && (
                    <tr><td colSpan={6} className="text-center opacity-50">暂无退出记录</td></tr>
                  )}
                </tbody>
              </table>
            </div>
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
