import { useState, useEffect } from "react";
import {
  fetchSection,
  fetchSectionMeta,
  fetchStatus,
  type SectionMeta,
  type StatusResponse,
} from "../api";
import ConfigSection from "../components/ConfigSection";

const SECTIONS = [
  { key: "strategy", label: "策略总控" },
  { key: "risk", label: "风控管理" },
  { key: "market_filter", label: "市场过滤" },
  { key: "weather", label: "天气策略" },
  { key: "crypto_alpha", label: "加密策略" },
  { key: "event_calendar", label: "事件日历" },
  { key: "liquidity_rewards", label: "流动性奖励" },
  { key: "smart_money", label: "跟单策略" },
];

interface SectionState {
  key: string;
  label: string;
  data: Record<string, unknown> | null;
  meta: SectionMeta | null;
  open: boolean;
}

export default function Configuration() {
  const [sections, setSections] = useState<SectionState[]>(
    SECTIONS.map((s) => ({ ...s, data: null, meta: null, open: false })),
  );
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<StatusResponse | null>(null);

  function load() {
    setLoading(true);
    Promise.all(
      [
        fetchStatus(),
        ...SECTIONS.map(async (s) => ({
          data: await fetchSection(s.key),
          meta: await fetchSectionMeta(s.key),
        })),
      ],
    )
      .then((results) => {
        const [statusResult, ...sectionResults] = results as [
          StatusResponse,
          ...Array<{ data: Record<string, unknown>; meta: SectionMeta }>
        ];
        setStatus(statusResult);
        setSections((prev) =>
          prev.map((s, i) => ({
            ...s,
            data: sectionResults[i].data,
            meta: sectionResults[i].meta,
          })),
        );
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  function toggleSection(idx: number) {
    setSections((prev) =>
      prev.map((s, i) => (i === idx ? { ...s, open: !s.open } : s)),
    );
  }

  if (loading && sections.every((s) => !s.data)) {
    return (
      <div className="flex justify-center items-center h-64">
        <span className="loading loading-spinner loading-lg" />
      </div>
    );
  }

  if (error && sections.every((s) => !s.data)) {
    return <div className="alert alert-error"><span>{error}</span></div>;
  }

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-bold">系统配置</h1>

      <div className="alert alert-info">
        <span>当前页面为只读展示。配置以 `TOML + 环境变量` 为准，前端不再提供保存和持久化入口。</span>
      </div>

      {status && (
        <div className={`alert ${status.trading_ready ? "alert-success" : "alert-warning"}`}>
          <div className="space-y-2">
            <div className="font-medium">
              {status.trading_ready
                ? `账户配置有效：${status.accounts_ready}/${status.accounts_configured} 个账户可交易`
                : "当前没有可交易账户"}
            </div>
            <div className="text-sm opacity-80">
              系统只支持显式多账户配置；需要通过 `[[accounts]]` 或 `PA_ACCOUNT_&lt;N&gt;_*` 提供账户，并确保对应私钥环境变量存在。
            </div>
            {status.accounts.length > 0 && (
              <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
                {status.accounts.map((account) => (
                  <div key={account.name} className="rounded border border-base-300 bg-base-100 px-3 py-2 text-sm">
                    <div className="font-medium">{account.name}</div>
                    <div className="opacity-80">
                      策略: {account.strategies.length > 0 ? account.strategies.join(", ") : "未绑定"}
                    </div>
                    <div className="opacity-80">
                      私钥环境变量: <code>{account.private_key_env}</code>
                    </div>
                    <div className={account.private_key_present ? "text-success" : "text-warning"}>
                      {account.private_key_present ? "已检测到私钥环境变量" : "未检测到私钥环境变量"}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      )}

      {sections.map((s, idx) => (
        <div key={s.key} className="collapse collapse-arrow bg-base-200">
          <input
            type="checkbox"
            checked={s.open}
            onChange={() => toggleSection(idx)}
          />
          <div className="collapse-title font-medium">{s.label}</div>
          <div className="collapse-content">
            {s.data && s.open && (
              <ConfigSection
                title={s.label}
                section={s.key}
                data={s.data}
                meta={s.meta ?? undefined}
              />
            )}
          </div>
        </div>
      ))}
    </div>
  );
}
