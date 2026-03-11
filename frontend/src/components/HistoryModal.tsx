import { useState, useEffect } from "react";
import { fetchHistory, type HistoryEntry } from "../api";

interface Props {
  section: string;
  open: boolean;
  onClose: () => void;
}

export default function HistoryModal({ section, open, onClose }: Props) {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setLoading(true);
    fetchHistory(section)
      .then(setEntries)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  }, [section, open]);

  if (!open) return null;

  return (
    <dialog className="modal modal-open">
      <div className="modal-box max-w-2xl">
        <h3 className="font-bold text-lg mb-4">
          配置变更历史 — {section}
        </h3>
        {loading && <span className="loading loading-spinner loading-sm" />}
        {error && <p className="text-error text-sm">{error}</p>}
        {!loading && entries.length === 0 && (
          <p className="text-sm opacity-60">暂无历史记录</p>
        )}
        {entries.length > 0 && (
          <div className="overflow-x-auto max-h-96">
            <table className="table table-xs">
              <thead>
                <tr>
                  <th>版本</th>
                  <th>时间</th>
                  <th>修改者</th>
                  <th>数据</th>
                </tr>
              </thead>
              <tbody>
                {entries.map((e) => (
                  <tr key={e.version}>
                    <td>{e.version}</td>
                    <td className="whitespace-nowrap">
                      {new Date(e.created_at).toLocaleString("zh-CN")}
                    </td>
                    <td>{e.changed_by || "-"}</td>
                    <td>
                      <details>
                        <summary className="cursor-pointer text-xs text-primary">
                          查看
                        </summary>
                        <pre className="text-xs mt-1 max-w-md overflow-auto bg-base-300 p-2 rounded">
                          {JSON.stringify(e.data, null, 2)}
                        </pre>
                      </details>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        <div className="modal-action">
          <button className="btn btn-sm" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
      <form method="dialog" className="modal-backdrop">
        <button onClick={onClose}>close</button>
      </form>
    </dialog>
  );
}
