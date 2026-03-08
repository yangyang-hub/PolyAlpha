import { useEffect, useState } from "react";
import { fetchSection } from "../api";
import ConfigSection from "../components/ConfigSection";
import HistoryModal from "../components/HistoryModal";

export default function RiskConfig() {
  const [data, setData] = useState<Record<string, unknown> | null>(null);
  const [loading, setLoading] = useState(true);

  const load = async () => {
    setLoading(true);
    try {
      setData(await fetchSection("risk"));
    } catch {
      setData(null);
    }
    setLoading(false);
  };

  useEffect(() => { load(); }, []);

  if (loading) return <span className="loading loading-spinner loading-lg" />;
  if (!data) return <div className="alert alert-warning">Could not load risk config</div>;

  return (
    <div>
      <div className="flex items-center justify-between mb-6">
        <h1 className="text-2xl font-bold">Risk Configuration</h1>
        <HistoryModal section="risk" />
      </div>
      <ConfigSection section="risk" data={data} onSaved={load} />
    </div>
  );
}
