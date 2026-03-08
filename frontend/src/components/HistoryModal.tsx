import { useState } from "react";
import { fetchHistory, type HistoryEntry } from "../api";

interface Props {
  section: string;
}

export default function HistoryModal({ section }: Props) {
  const [open, setOpen] = useState(false);
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [loading, setLoading] = useState(false);

  const loadHistory = async () => {
    setOpen(true);
    setLoading(true);
    try {
      const data = await fetchHistory(section);
      setEntries(data);
    } catch {
      setEntries([]);
    } finally {
      setLoading(false);
    }
  };

  return (
    <>
      <button className="btn btn-ghost btn-sm" onClick={loadHistory}>
        History
      </button>
      {open && (
        <dialog className="modal modal-open">
          <div className="modal-box max-w-2xl">
            <h3 className="font-bold text-lg mb-4">
              History: {section}
            </h3>
            {loading ? (
              <span className="loading loading-spinner" />
            ) : entries.length === 0 ? (
              <p className="text-base-content/60">No history available</p>
            ) : (
              <div className="overflow-x-auto">
                <table className="table table-sm">
                  <thead>
                    <tr>
                      <th>Version</th>
                      <th>Changed By</th>
                      <th>Date</th>
                    </tr>
                  </thead>
                  <tbody>
                    {entries.map((e, i) => (
                      <tr key={i}>
                        <td>v{e.version}</td>
                        <td>{e.changed_by}</td>
                        <td>{new Date(e.created_at).toLocaleString()}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
            <div className="modal-action">
              <button className="btn" onClick={() => setOpen(false)}>
                Close
              </button>
            </div>
          </div>
          <form method="dialog" className="modal-backdrop">
            <button onClick={() => setOpen(false)}>close</button>
          </form>
        </dialog>
      )}
    </>
  );
}
