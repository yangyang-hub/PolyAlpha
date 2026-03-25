import { useCallback, useState } from "react";
import {
  applySmartMoneyLeaderRouteTemplate,
  blockSmartMoneyLeader,
  degradeSmartMoneyLeader,
  fetchPositions,
  fetchSection,
  fetchSmartMoneyAudit,
  fetchSmartMoneyLeaders,
  fetchStrategyTrades,
  fetchStatus,
  promoteSmartMoneyLeader,
  restoreSmartMoneyLeader,
  type ApplySmartMoneyLeaderRouteTemplateResponse,
  type BlockSmartMoneyLeaderResponse,
  type CryptoTradeEntry,
  type DegradeSmartMoneyLeaderResponse,
  type PromoteSmartMoneyLeaderResponse,
  type RestoreSmartMoneyLeaderResponse,
  type SmartMoneyAuditEntry,
  type PositionEntry,
  type SmartMoneyLeaderCandidate,
  type StatusResponse,
} from "../api";
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
  const leadersFetcher = useCallback(() => fetchSmartMoneyLeaders(), []);
  const tradesFetcher = useCallback(() => fetchStrategyTrades("smart_money", 50), []);
  const auditFetcher = useCallback(() => fetchSmartMoneyAudit(20), []);
  const { data: positions, loading: posLoading } = usePolling<PositionEntry[]>(posFetcher, 15000);
  const { data: config } = usePolling<Record<string, unknown>>(configFetcher, 60000);
  const { data: status } = usePolling<StatusResponse>(statusFetcher, 15000);
  const { data: leaderCandidates } = usePolling<SmartMoneyLeaderCandidate[]>(leadersFetcher, 30000);
  const { data: trades } = usePolling<CryptoTradeEntry[]>(tradesFetcher, 15000);
  const { data: auditEntries } = usePolling<SmartMoneyAuditEntry[]>(auditFetcher, 30000);
  const strategyAccounts = status?.accounts.filter((account) => account.strategies.includes("smart_money")) ?? [];
  const wallets: Wallet[] = Array.isArray(config?.wallets) ? (config.wallets as Wallet[]) : [];

  const totalCost = (positions ?? []).reduce((s, p) => s + Number(p.cost_basis), 0);
  const totalPnl = (positions ?? []).reduce((s, p) => s + Number(p.unrealized_pnl ?? 0), 0);
  const marketCount = new Set((positions ?? []).map((p) => p.condition_id).filter(Boolean)).size;
  const signalSummary = status?.smart_money_signal_summary;
  const rejectSummary = status?.smart_money_gate_reject_summary;
  const exitSummary = status?.smart_money_exit_summary;
  const routeSummary = status?.smart_money_route_summary;
  const walletScores = status?.smart_money_wallet_scores ?? [];
  const recentDecisions = status?.smart_money_recent_decisions ?? [];
  const recentExits = status?.smart_money_recent_exits ?? [];
  const discoverySummary = status?.smart_money_leader_discovery_summary;
  const leaderAttribution = status?.smart_money_leader_attribution_summary?.top_leaders ?? [];
  const leaderPnlAttribution = status?.smart_money_leader_pnl_attribution_summary?.top_leaders ?? [];
  const leaderTradeAttribution = status?.smart_money_trade_attribution_summary?.top_leaders ?? [];
  const leaderHealthSummary = status?.smart_money_leader_health_summary?.top_leaders ?? [];
  const reviewQueueSummary = status?.smart_money_review_queue_summary;
  const reviewQueue = reviewQueueSummary?.top_actions ?? [];
  const topLeaderCandidates = leaderCandidates?.length ? leaderCandidates : (discoverySummary?.top_candidates ?? []);
  const [promotionResult, setPromotionResult] = useState<PromoteSmartMoneyLeaderResponse | null>(null);
  const [actionResult, setActionResult] = useState<
    BlockSmartMoneyLeaderResponse | DegradeSmartMoneyLeaderResponse | RestoreSmartMoneyLeaderResponse | ApplySmartMoneyLeaderRouteTemplateResponse | null
  >(null);
  const [promotionError, setPromotionError] = useState<string | null>(null);
  const [promotingAddress, setPromotingAddress] = useState<string | null>(null);
  const [routeTemplates, setRouteTemplates] = useState<Record<string, string>>({});
  const acceptRate = signalSummary?.recent_entry_attempts
    ? (signalSummary.recent_entry_accepted / signalSummary.recent_entry_attempts) * 100
    : 0;

  function inferRouteTemplate(candidate: SmartMoneyLeaderCandidate): string {
    if (!(candidate.route_categories?.length || candidate.route_question_keywords?.length || candidate.route_event_title_keywords?.length)) {
      return "clear";
    }
    if (candidate.route_categories?.includes("crypto")) return "crypto";
    if (candidate.route_categories?.includes("politics")) return "politics";
    if (candidate.route_categories?.includes("sports")) return "sports";
    if (candidate.route_categories?.includes("weather")) return "weather";
    return "clear";
  }

  function resolveCandidateForLeader(leader: string): SmartMoneyLeaderCandidate | undefined {
    return topLeaderCandidates.find(
      (candidate) =>
        candidate.address.toLowerCase() === leader.toLowerCase() ||
        (candidate.label && candidate.label.toLowerCase() === leader.toLowerCase()),
    );
  }

  const handlePromote = useCallback(async (address: string) => {
    try {
      setPromotionError(null);
      setActionResult(null);
      setPromotingAddress(address);
      const result = await promoteSmartMoneyLeader(address);
      setPromotionResult(result);
    } catch (error) {
      setPromotionError(error instanceof Error ? error.message : String(error));
    } finally {
      setPromotingAddress(null);
    }
  }, []);

  const handleBlock = useCallback(async (address: string) => {
    try {
      setPromotionError(null);
      setPromotionResult(null);
      setPromotingAddress(address);
      const result = await blockSmartMoneyLeader(address);
      setActionResult(result);
    } catch (error) {
      setPromotionError(error instanceof Error ? error.message : String(error));
    } finally {
      setPromotingAddress(null);
    }
  }, []);

  const handleDegrade = useCallback(async (address: string) => {
    try {
      setPromotionError(null);
      setPromotionResult(null);
      setPromotingAddress(address);
      const result = await degradeSmartMoneyLeader(address);
      setActionResult(result);
    } catch (error) {
      setPromotionError(error instanceof Error ? error.message : String(error));
    } finally {
      setPromotingAddress(null);
    }
  }, []);

  const handleRestore = useCallback(async (address: string) => {
    try {
      setPromotionError(null);
      setPromotionResult(null);
      setPromotingAddress(address);
      const result = await restoreSmartMoneyLeader(address);
      setActionResult(result);
    } catch (error) {
      setPromotionError(error instanceof Error ? error.message : String(error));
    } finally {
      setPromotingAddress(null);
    }
  }, []);

  const handleApplyRouteTemplate = useCallback(async (address: string, template: string) => {
    try {
      setPromotionError(null);
      setPromotionResult(null);
      setPromotingAddress(address);
      const result = await applySmartMoneyLeaderRouteTemplate(address, template);
      setActionResult(result);
    } catch (error) {
      setPromotionError(error instanceof Error ? error.message : String(error));
    } finally {
      setPromotingAddress(null);
    }
  }, []);

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

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <h2 className="card-title text-base">Leader 路由</h2>
          <div className="grid grid-cols-2 gap-3 text-sm">
            <div>
              <div className="opacity-60 text-xs">已配置路由</div>
              <div className="text-lg font-semibold">{routeSummary?.configured_routes ?? 0}</div>
            </div>
            <div>
              <div className="opacity-60 text-xs">最近 route mismatch</div>
              <div className="text-lg font-semibold">{routeSummary?.route_mismatch_rejections ?? 0}</div>
            </div>
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

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <h2 className="card-title text-base">Leader 信号归因</h2>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>Leader</th>
                  <th>信号数</th>
                  <th>放行</th>
                  <th>拒绝</th>
                  <th>放行率</th>
                </tr>
              </thead>
              <tbody>
                {leaderAttribution.map((leader) => (
                  <tr key={leader.leader}>
                    <td>{leader.leader}</td>
                    <td>{leader.signals}</td>
                    <td>{leader.accepted}</td>
                    <td>{leader.rejected}</td>
                    <td>{(leader.accept_rate * 100).toFixed(0)}%</td>
                  </tr>
                ))}
                {leaderAttribution.length === 0 && (
                  <tr><td colSpan={5} className="text-center opacity-50">暂无 leader 归因数据</td></tr>
                )}
              </tbody>
            </table>
          </div>
        </div>
      </div>

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <h2 className="card-title text-base">最近操作审计</h2>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>时间</th>
                  <th>来源</th>
                  <th>版本</th>
                  <th>钱包数</th>
                  <th>候选数</th>
                  <th>降权</th>
                  <th>拉黑</th>
                  <th>路由</th>
                </tr>
              </thead>
              <tbody>
                {(auditEntries ?? []).map((entry) => (
                  <tr key={`${entry.version}-${entry.created_at}`}>
                    <td>{entry.created_at}</td>
                    <td>{entry.changed_by}</td>
                    <td>{entry.version}</td>
                    <td>{entry.wallet_count}</td>
                    <td>{entry.auto_discover_candidate_count}</td>
                    <td>{entry.degraded_wallet_count}</td>
                    <td>{entry.blocked_wallet_count}</td>
                    <td>{entry.route_count}</td>
                  </tr>
                ))}
                {!(auditEntries ?? []).length && (
                  <tr><td colSpan={8} className="text-center opacity-50">暂无 smart-money 操作审计</td></tr>
                )}
              </tbody>
            </table>
          </div>
          <div className="text-xs opacity-60 mt-2">
            这里展示最近写入 `smart_money` config store 的操作快照，方便回看谁在什么时候改了候选池、路由或运行时覆盖。
          </div>
        </div>
      </div>

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <div className="flex items-center justify-between gap-4">
            <h2 className="card-title text-base">待审队列</h2>
            <div className="text-sm opacity-60">
              待处理 {reviewQueueSummary?.pending_count ?? 0}
            </div>
          </div>
          {!!reviewQueueSummary?.action_counts?.length && (
            <div className="flex flex-wrap gap-2 text-xs opacity-70">
              {reviewQueueSummary.action_counts.map((entry) => (
                <span key={entry.label} className="badge badge-outline badge-sm">
                  {entry.label}: {entry.count}
                </span>
              ))}
            </div>
          )}
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>Leader</th>
                  <th>建议</th>
                  <th>当前状态</th>
                  <th>执行</th>
                  <th>原因</th>
                </tr>
              </thead>
              <tbody>
                {reviewQueue.map((entry) => {
                  const candidate = entry.address ? resolveCandidateForLeader(entry.address) : resolveCandidateForLeader(entry.leader);
                  const actionDisabled = !entry.actionable || !candidate || promotingAddress === candidate.address;
                  return (
                    <tr key={`${entry.leader}-${entry.suggested_action}`}>
                      <td>{entry.label ?? entry.leader}</td>
                      <td>
                        <span className={
                          entry.suggested_action === "block_candidate"
                            ? "badge badge-error badge-sm"
                            : entry.suggested_action === "degrade"
                              ? "badge badge-warning badge-sm"
                              : "badge badge-success badge-sm"
                        }>
                          {entry.suggested_action}
                        </span>
                      </td>
                      <td>{entry.current_state}</td>
                      <td>
                        {entry.suggested_action === "block_candidate" && (
                          <button className="btn btn-xs btn-error" disabled={actionDisabled} onClick={() => candidate && void handleBlock(candidate.address)}>
                            {actionDisabled && candidate ? "..." : "拉黑"}
                          </button>
                        )}
                        {entry.suggested_action === "degrade" && (
                          <button className="btn btn-xs btn-warning" disabled={actionDisabled} onClick={() => candidate && void handleDegrade(candidate.address)}>
                            {actionDisabled && candidate ? "..." : "降权50%"}
                          </button>
                        )}
                        {entry.suggested_action === "keep_or_promote" && (
                          candidate?.blocked || (candidate?.degrade_multiplier && Number(candidate.degrade_multiplier) < 1) ? (
                            <button className="btn btn-xs btn-outline" disabled={actionDisabled} onClick={() => candidate && void handleRestore(candidate.address)}>
                              {actionDisabled && candidate ? "..." : "恢复"}
                            </button>
                          ) : (
                            <button className="btn btn-xs btn-success" disabled={actionDisabled} onClick={() => candidate && void handlePromote(candidate.address)}>
                              {actionDisabled && candidate ? "..." : "晋升"}
                            </button>
                          )
                        )}
                        {!entry.actionable && <span className="opacity-50 text-xs">无需动作</span>}
                      </td>
                      <td className="max-w-xl">{entry.rationale}</td>
                    </tr>
                  );
                })}
                {!reviewQueue.length && (
                  <tr><td colSpan={5} className="text-center opacity-50">暂无待审建议</td></tr>
                )}
              </tbody>
            </table>
          </div>
          <div className="text-xs opacity-60 mt-2">
            待审队列只展示后端判定为需要 promote / degrade / block / restore 的建议，并结合当前 candidate 状态去重。
          </div>
        </div>
      </div>

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <h2 className="card-title text-base">Leader 健康建议</h2>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>Leader</th>
                  <th>接受率</th>
                  <th>成交 PnL</th>
                  <th>估算 PnL</th>
                  <th>建议</th>
                  <th>执行</th>
                  <th>原因</th>
                </tr>
              </thead>
              <tbody>
                {leaderHealthSummary.map((leader) => {
                  const candidate = resolveCandidateForLeader(leader.leader);
                  const actionDisabled = !candidate || promotingAddress === candidate.address;
                  return (
                    <tr key={leader.leader}>
                      <td>{leader.leader}</td>
                      <td>{(leader.accept_rate * 100).toFixed(0)}%</td>
                      <td className={Number(leader.actual_realized_profit) >= 0 ? "text-success" : "text-error"}>
                        ${Number(leader.actual_realized_profit).toFixed(2)}
                      </td>
                      <td className={Number(leader.estimated_realized_pnl) >= 0 ? "text-success" : "text-error"}>
                        ${Number(leader.estimated_realized_pnl).toFixed(2)}
                      </td>
                      <td>
                        <span className={
                          leader.suggested_action === "block_candidate"
                            ? "badge badge-error badge-sm"
                            : leader.suggested_action === "degrade"
                              ? "badge badge-warning badge-sm"
                              : leader.suggested_action === "keep_or_promote"
                                ? "badge badge-success badge-sm"
                                : "badge badge-ghost badge-sm"
                        }>
                          {leader.suggested_action}
                        </span>
                      </td>
                      <td>
                        {leader.suggested_action === "block_candidate" && (
                          <button
                            className="btn btn-xs btn-error"
                            disabled={actionDisabled}
                            onClick={() => candidate && void handleBlock(candidate.address)}
                          >
                            {actionDisabled && candidate ? "..." : "拉黑"}
                          </button>
                        )}
                        {leader.suggested_action === "degrade" && (
                          <button
                            className="btn btn-xs btn-warning"
                            disabled={actionDisabled}
                            onClick={() => candidate && void handleDegrade(candidate.address)}
                          >
                            {actionDisabled && candidate ? "..." : "降权50%"}
                          </button>
                        )}
                        {leader.suggested_action === "keep_or_promote" && (
                          candidate?.blocked || (candidate?.degrade_multiplier && Number(candidate.degrade_multiplier) < 1) ? (
                            <button
                              className="btn btn-xs btn-outline"
                              disabled={actionDisabled}
                              onClick={() => candidate && void handleRestore(candidate.address)}
                            >
                              {actionDisabled && candidate ? "..." : "恢复"}
                            </button>
                          ) : !candidate?.promoted ? (
                            <button
                              className="btn btn-xs btn-success"
                              disabled={actionDisabled}
                              onClick={() => candidate && void handlePromote(candidate.address)}
                            >
                              {actionDisabled && candidate ? "..." : "晋升"}
                            </button>
                          ) : (
                            <span className="opacity-50 text-xs">已晋升</span>
                          )
                        )}
                        {leader.suggested_action === "observe" && (
                          <span className="opacity-50 text-xs">
                            {candidate ? "观察" : "未匹配候选"}
                          </span>
                        )}
                      </td>
                      <td className="max-w-xl">{leader.rationale}</td>
                    </tr>
                  );
                })}
                {leaderHealthSummary.length === 0 && (
                  <tr><td colSpan={7} className="text-center opacity-50">暂无 leader 健康建议</td></tr>
                )}
              </tbody>
            </table>
          </div>
          <div className="text-xs opacity-60 mt-2">
            建议动作由后端根据最近接受率、机会级估算收益和成交级真实收益生成，适合用来辅助 degrade / block / promote。
          </div>
        </div>
      </div>

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <h2 className="card-title text-base">Leader 收益归因（成交）</h2>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>Leader</th>
                  <th>真实成交量</th>
                  <th>真实费用</th>
                  <th>真实已实现 PnL</th>
                  <th>成交次数</th>
                </tr>
              </thead>
              <tbody>
                {leaderTradeAttribution.map((leader) => (
                  <tr key={leader.leader}>
                    <td>{leader.leader}</td>
                    <td>{Number(leader.actual_filled_size).toFixed(1)}</td>
                    <td>${Number(leader.actual_fee).toFixed(2)}</td>
                    <td className={Number(leader.actual_realized_profit) >= 0 ? "text-success" : "text-error"}>
                      ${Number(leader.actual_realized_profit).toFixed(2)}
                    </td>
                    <td>{leader.trade_count}</td>
                  </tr>
                ))}
                {leaderTradeAttribution.length === 0 && (
                  <tr><td colSpan={5} className="text-center opacity-50">暂无成交归因数据</td></tr>
                )}
              </tbody>
            </table>
          </div>
          <div className="text-xs opacity-60 mt-2">
            基于持久化 trade 记录里的 filled size、fee 和卖出侧 realized profit 做聚合，比机会级估算更接近真实执行结果。
          </div>
        </div>
      </div>

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <h2 className="card-title text-base">Leader 收益归因（估算）</h2>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>Leader</th>
                  <th>估算持仓</th>
                  <th>估算已退出</th>
                  <th>估算已实现 PnL</th>
                  <th>退出次数</th>
                </tr>
              </thead>
              <tbody>
                {leaderPnlAttribution.map((leader) => (
                  <tr key={leader.leader}>
                    <td>{leader.leader}</td>
                    <td>{Number(leader.estimated_open_size).toFixed(1)}</td>
                    <td>{Number(leader.estimated_exited_size).toFixed(1)}</td>
                    <td className={Number(leader.estimated_realized_pnl) >= 0 ? "text-success" : "text-error"}>
                      ${Number(leader.estimated_realized_pnl).toFixed(2)}
                    </td>
                    <td>{leader.estimated_exit_count}</td>
                  </tr>
                ))}
                {leaderPnlAttribution.length === 0 && (
                  <tr><td colSpan={5} className="text-center opacity-50">暂无收益归因数据</td></tr>
                )}
              </tbody>
            </table>
          </div>
          <div className="text-xs opacity-60 mt-2">
            基于 smart-money 已接受的跟单机会做估算归因，不等同于成交回报账本。
          </div>
        </div>
      </div>

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <h2 className="card-title text-base">最近成交归因</h2>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>时间</th>
                  <th>方向</th>
                  <th>价格</th>
                  <th>成交数量</th>
                  <th>实际收益</th>
                  <th>Leader 成交归因</th>
                </tr>
              </thead>
              <tbody>
                {(trades ?? []).slice(0, 12).map((trade) => (
                  <tr key={trade.trade_id}>
                    <td>{trade.executed_at ?? trade.created_at}</td>
                    <td>{trade.side}</td>
                    <td>{trade.price}</td>
                    <td>{trade.filled_size ?? trade.size}</td>
                    <td className={Number(trade.actual_profit ?? 0) >= 0 ? "text-success" : "text-error"}>
                      {trade.actual_profit ? `$${Number(trade.actual_profit).toFixed(2)}` : "-"}
                    </td>
                    <td className="max-w-xl">
                      {trade.smart_money_trade_attribution?.length
                        ? trade.smart_money_trade_attribution
                            .map((slice) => `${slice.leader}: $${Number(slice.actual_realized_profit).toFixed(2)}`)
                            .join(", ")
                        : trade.smart_money_attribution?.length
                          ? trade.smart_money_attribution
                              .map((slice) => `${slice.leader}: est $${Number(slice.estimated_profit).toFixed(2)}`)
                              .join(", ")
                          : "-"}
                    </td>
                  </tr>
                ))}
                {!(trades ?? []).length && (
                  <tr><td colSpan={6} className="text-center opacity-50">暂无 smart-money 成交记录</td></tr>
                )}
              </tbody>
            </table>
          </div>
          <div className="text-xs opacity-60 mt-2">
            优先展示基于真实 filled size/fee/realized profit 的成交归因；若旧记录尚未带 trade 级字段，则回退显示机会级估算归因。
          </div>
        </div>
      </div>

      <div className="card bg-base-200 shadow-sm">
        <div className="card-body p-4">
          <div className="flex items-center justify-between gap-4">
            <h2 className="card-title text-base">发现候选 Leader</h2>
            <div className="text-sm opacity-60">
              候选池 {discoverySummary?.candidate_count ?? leaderCandidates?.length ?? 0} 个
            </div>
          </div>
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th>地址</th>
                  <th>标签</th>
                  <th>分数</th>
                  <th>路由</th>
                  <th>来源</th>
                  <th>榜单</th>
                  <th>已平仓收益</th>
                  <th>链上活跃</th>
                  <th>操作</th>
                </tr>
              </thead>
              <tbody>
                {topLeaderCandidates.slice(0, 12).map((candidate) => {
                  const metadata = candidate.metadata as { onchain_transfer_count?: number } | null | undefined;
                  const selectedTemplate = routeTemplates[candidate.address] ?? inferRouteTemplate(candidate);
                  return (
                    <tr key={candidate.address}>
                      <td className="font-mono text-xs">{candidate.address.slice(0, 10)}...{candidate.address.slice(-6)}</td>
                      <td>{candidate.label || "-"}</td>
                      <td>{Number(candidate.discovery_score).toFixed(3)}</td>
                      <td className="max-w-64">
                        <div className="flex flex-wrap gap-1">
                          {(candidate.route_categories ?? []).slice(0, 2).map((category) => (
                            <span key={`cat-${category}`} className="badge badge-outline badge-xs">
                              cat:{category}
                            </span>
                          ))}
                          {(candidate.route_question_keywords ?? []).slice(0, 2).map((keyword) => (
                            <span key={`q-${keyword}`} className="badge badge-outline badge-xs">
                              q:{keyword}
                            </span>
                          ))}
                          {(candidate.route_event_title_keywords ?? []).slice(0, 2).map((keyword) => (
                            <span key={`e-${keyword}`} className="badge badge-outline badge-xs">
                              event:{keyword}
                            </span>
                          ))}
                          {!(candidate.route_categories?.length || candidate.route_question_keywords?.length || candidate.route_event_title_keywords?.length) && (
                            <span className="opacity-50 text-xs">all</span>
                          )}
                        </div>
                        <div className="mt-2 flex gap-2 items-center">
                          <select
                            className="select select-bordered select-xs"
                            value={selectedTemplate}
                            onChange={(event) => {
                              const value = event.target.value;
                              setRouteTemplates((current) => ({ ...current, [candidate.address]: value }));
                            }}
                            disabled={promotingAddress === candidate.address}
                          >
                            <option value="clear">All</option>
                            <option value="crypto">Crypto</option>
                            <option value="politics">Politics</option>
                            <option value="sports">Sports</option>
                            <option value="weather">Weather</option>
                          </select>
                          <button
                            className="btn btn-xs btn-outline"
                            onClick={() => void handleApplyRouteTemplate(candidate.address, selectedTemplate)}
                            disabled={promotingAddress === candidate.address}
                          >
                            {promotingAddress === candidate.address ? "..." : "应用"}
                          </button>
                        </div>
                      </td>
                      <td className="max-w-64">
                        <div className="flex flex-wrap gap-1">
                          {(candidate.source_tags ?? []).slice(0, 3).map((tag) => (
                            <span key={tag} className="badge badge-outline badge-xs">{tag}</span>
                          ))}
                        </div>
                      </td>
                      <td>{candidate.leaderboard_rank ? `#${candidate.leaderboard_rank}` : "-"}</td>
                      <td className={Number(candidate.closed_realized_pnl) >= 0 ? "text-success" : "text-error"}>
                        ${Number(candidate.closed_realized_pnl).toFixed(0)}
                      </td>
                      <td>{metadata?.onchain_transfer_count ?? 0}</td>
                      <td>
                        <div className="flex flex-wrap gap-1">
                          {candidate.promoted && (
                            <span className="badge badge-success badge-sm">promoted</span>
                          )}
                          {candidate.blocked && (
                            <span className="badge badge-error badge-sm">blocked</span>
                          )}
                          {!candidate.blocked && candidate.degrade_multiplier && Number(candidate.degrade_multiplier) < 1 && (
                            <span className="badge badge-warning badge-sm">x{Number(candidate.degrade_multiplier).toFixed(2)}</span>
                          )}
                          {(candidate.blocked || (candidate.degrade_multiplier && Number(candidate.degrade_multiplier) < 1)) && (
                            <button
                              className="btn btn-xs btn-outline"
                              onClick={() => void handleRestore(candidate.address)}
                              disabled={promotingAddress === candidate.address}
                            >
                              {promotingAddress === candidate.address ? "..." : "恢复"}
                            </button>
                          )}
                          {!candidate.promoted && !candidate.blocked && (
                            <button
                              className="btn btn-xs btn-outline"
                              onClick={() => void handlePromote(candidate.address)}
                              disabled={promotingAddress === candidate.address}
                            >
                              {promotingAddress === candidate.address ? "..." : "晋升"}
                            </button>
                          )}
                          {!candidate.blocked && (
                            <button
                              className="btn btn-xs btn-outline"
                              onClick={() => void handleDegrade(candidate.address)}
                              disabled={promotingAddress === candidate.address}
                            >
                              {promotingAddress === candidate.address ? "..." : "降权50%"}
                            </button>
                          )}
                          {!candidate.blocked && (
                            <button
                              className="btn btn-xs btn-outline btn-error"
                              onClick={() => void handleBlock(candidate.address)}
                              disabled={promotingAddress === candidate.address}
                            >
                              {promotingAddress === candidate.address ? "..." : "拉黑"}
                            </button>
                          )}
                        </div>
                      </td>
                    </tr>
                  );
                })}
                {topLeaderCandidates.length === 0 && (
                  <tr><td colSpan={9} className="text-center opacity-50">暂无候选池数据，需要先运行 smart_money_discover_leaders</td></tr>
                )}
              </tbody>
            </table>
          </div>
          {(promotionResult || actionResult || promotionError) && (
            <div className="mt-4 space-y-2">
              {promotionError && <div className="alert alert-error py-2 text-sm">{promotionError}</div>}
              {actionResult && (
                <div className="alert alert-info py-2 text-sm">{actionResult.note}</div>
              )}
              {promotionResult && (
                <div className="space-y-2">
                  <div className="alert alert-info py-2 text-sm">{promotionResult.note}</div>
                  <div>
                    <div className="opacity-60 text-xs mb-1">`[[smart_money.wallets]]` 片段</div>
                    <pre className="bg-base-300 rounded-box p-3 text-xs overflow-x-auto">{promotionResult.wallets_toml}</pre>
                  </div>
                  <div>
                    <div className="opacity-60 text-xs mb-1">`auto_discover_candidates` 条目</div>
                    <pre className="bg-base-300 rounded-box p-3 text-xs overflow-x-auto">{promotionResult.auto_discover_candidate}</pre>
                  </div>
                </div>
              )}
            </div>
          )}
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
                    <th>Leader</th>
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
                      <td className="text-xs max-w-48 truncate" title={decision.leader_labels?.join(", ") || decision.leader_addresses.join(", ")}>
                        {decision.leader_labels?.join(", ") || decision.leader_addresses.join(", ") || "-"}
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
                    <tr><td colSpan={6} className="text-center opacity-50">暂无决策记录</td></tr>
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
                      <td>
                        <div>{Number(exit.size).toFixed(1)}</div>
                        <div className={`text-xs ${Number(exit.estimated_profit) >= 0 ? "text-success" : "text-error"}`}>
                          ${Number(exit.estimated_profit).toFixed(2)}
                        </div>
                        <div className="text-xs opacity-60 truncate" title={exit.attributed_leaders.map((leader) => `${leader.leader}:${Number(leader.estimated_profit).toFixed(2)}`).join(", ")}>
                          {(exit.attributed_leaders ?? []).slice(0, 2).map((leader) => leader.leader).join(", ") || "-"}
                        </div>
                      </td>
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
