import { useState, useCallback } from "react";
import { updateSection } from "../api";

interface Props {
  section: string;
  data: Record<string, unknown>;
  onSaved?: () => void;
}

export default function ConfigSection({ section, data, onSaved }: Props) {
  const [formData, setFormData] = useState<Record<string, unknown>>(data);
  const [saving, setSaving] = useState(false);
  const [toast, setToast] = useState<{ type: "success" | "error"; msg: string } | null>(null);

  const handleChange = useCallback((key: string, value: unknown) => {
    setFormData((prev) => ({ ...prev, [key]: value }));
  }, []);

  const handleSave = async () => {
    setSaving(true);
    setToast(null);
    try {
      const res = await updateSection(section, formData);
      setToast({ type: "success", msg: `已保存 v${res.version}` });
      onSaved?.();
    } catch (e) {
      setToast({ type: "error", msg: String(e) });
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="card bg-base-100 shadow-sm">
      <div className="card-body">
        <div className="grid gap-3 mt-2">
          {Object.entries(formData).map(([key, value]) => (
            <FieldInput
              key={key}
              label={key}
              value={value}
              onChange={(v) => handleChange(key, v)}
            />
          ))}
        </div>

        {toast && (
          <div className={`alert ${toast.type === "success" ? "alert-success" : "alert-error"} mt-3`}>
            <span>{toast.msg}</span>
          </div>
        )}

        <div className="card-actions justify-end mt-4">
          <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
            {saving ? <span className="loading loading-spinner loading-sm" /> : "保存"}
          </button>
        </div>
      </div>
    </div>
  );
}

function FieldInput({
  label,
  value,
  onChange,
}: {
  label: string;
  value: unknown;
  onChange: (v: unknown) => void;
}) {
  if (typeof value === "boolean") {
    return (
      <label className="flex items-center gap-3 cursor-pointer">
        <span className="label-text font-mono text-sm flex-1">{label}</span>
        <input
          type="checkbox"
          className="toggle toggle-primary"
          checked={value}
          onChange={(e) => onChange(e.target.checked)}
        />
      </label>
    );
  }

  if (typeof value === "number") {
    return (
      <label className="form-control w-full">
        <div className="label">
          <span className="label-text font-mono text-sm">{label}</span>
        </div>
        <input
          type="number"
          className="input input-bordered input-sm w-full"
          value={value}
          step="any"
          onChange={(e) => onChange(Number(e.target.value))}
        />
      </label>
    );
  }

  if (typeof value === "string") {
    return (
      <label className="form-control w-full">
        <div className="label">
          <span className="label-text font-mono text-sm">{label}</span>
        </div>
        <input
          type="text"
          className="input input-bordered input-sm w-full"
          value={value}
          onChange={(e) => onChange(e.target.value)}
        />
      </label>
    );
  }

  if (Array.isArray(value)) {
    if (value.every((v) => typeof v === "string")) {
      return (
        <label className="form-control w-full">
          <div className="label">
            <span className="label-text font-mono text-sm">{label}</span>
          </div>
          <input
            type="text"
            className="input input-bordered input-sm w-full"
            value={(value as string[]).join(", ")}
            onChange={(e) =>
              onChange(
                e.target.value
                  .split(",")
                  .map((s) => s.trim())
                  .filter(Boolean)
              )
            }
          />
          <div className="label">
            <span className="label-text-alt">逗号分隔</span>
          </div>
        </label>
      );
    }
  }

  // Nested objects or complex types: render as JSON textarea
  if (typeof value === "object" && value !== null) {
    return (
      <label className="form-control w-full">
        <div className="label">
          <span className="label-text font-mono text-sm">{label}</span>
        </div>
        <textarea
          className="textarea textarea-bordered textarea-sm w-full font-mono text-xs"
          rows={4}
          value={JSON.stringify(value, null, 2)}
          onChange={(e) => {
            try {
              onChange(JSON.parse(e.target.value));
            } catch {
              // ignore parse errors while typing
            }
          }}
        />
      </label>
    );
  }

  return null;
}
