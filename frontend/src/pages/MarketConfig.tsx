import { useEffect, useState } from "react";
import { fetchSection } from "../api";
import ConfigSection from "../components/ConfigSection";
import HistoryModal from "../components/HistoryModal";

const sections = ["market_filter", "chain", "clob", "gamma"];

const sectionLabels: Record<string, string> = {
  market_filter: "市场过滤",
  chain: "链配置",
  clob: "CLOB 配置",
  gamma: "Gamma API",
};

export default function MarketConfig() {
  const [data, setData] = useState<Record<string, Record<string, unknown>>>({});
  const [loading, setLoading] = useState(true);

  const load = async () => {
    setLoading(true);
    const result: Record<string, Record<string, unknown>> = {};
    for (const s of sections) {
      try {
        result[s] = await fetchSection(s);
      } catch {
        // section may not exist
      }
    }
    setData(result);
    setLoading(false);
  };

  useEffect(() => { load(); }, []);

  if (loading) return <span className="loading loading-spinner loading-lg" />;

  return (
    <div>
      <h1 className="text-2xl font-bold mb-6">市场配置</h1>
      <div className="grid gap-6">
        {sections.map((s) =>
          data[s] ? (
            <div key={s}>
              <div className="flex items-center justify-between mb-2">
                <h3 className="font-semibold">{sectionLabels[s] ?? s}</h3>
                <HistoryModal section={s} />
              </div>
              <ConfigSection section={s} data={data[s]} onSaved={load} />
            </div>
          ) : null
        )}
      </div>
    </div>
  );
}
