import { useEffect, useState } from "react";
import { fetchSection } from "../api";
import ConfigSection from "../components/ConfigSection";
import HistoryModal from "../components/HistoryModal";

const sections = ["monitor", "database"];

const sectionLabels: Record<string, string> = {
  monitor: "监控设置",
  database: "数据库",
};

export default function MonitorConfig() {
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
      <h1 className="text-2xl font-bold mb-6">监控配置</h1>
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
