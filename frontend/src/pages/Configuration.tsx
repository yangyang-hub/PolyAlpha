import { useState, useEffect } from "react";
import { fetchSection } from "../api";
import ConfigSection from "../components/ConfigSection";
import HistoryModal from "../components/HistoryModal";

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
  open: boolean;
}

export default function Configuration() {
  const [sections, setSections] = useState<SectionState[]>(
    SECTIONS.map((s) => ({ ...s, data: null, open: false })),
  );
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [historySection, setHistorySection] = useState<string | null>(null);

  function load() {
    setLoading(true);
    Promise.all(SECTIONS.map((s) => fetchSection(s.key)))
      .then((results) => {
        setSections((prev) =>
          prev.map((s, i) => ({ ...s, data: results[i] })),
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
                onSaved={load}
                onHistory={() => setHistorySection(s.key)}
              />
            )}
          </div>
        </div>
      ))}

      {historySection && (
        <HistoryModal
          section={historySection}
          open={true}
          onClose={() => setHistorySection(null)}
        />
      )}
    </div>
  );
}
