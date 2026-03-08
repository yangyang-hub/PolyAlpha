const API_BASE = "";

export interface StatusResponse {
  uptime_seconds: number;
  enabled_strategies: string[];
  scan_interval_ms: number;
  health_port: number;
  lr_enabled: boolean;
  event_calendar_enabled: boolean;
}

export interface HealthResponse {
  status: string;
  service: string;
  uptime_seconds: number;
  checks: Record<string, string>;
}

export interface HistoryEntry {
  version: number;
  data: Record<string, unknown>;
  changed_by: string;
  created_at: string;
}

export async function fetchConfig(): Promise<Record<string, unknown>> {
  const res = await fetch(`${API_BASE}/api/config`);
  if (!res.ok) throw new Error(`Failed to fetch config: ${res.status}`);
  return res.json();
}

export async function fetchSection(
  section: string
): Promise<Record<string, unknown>> {
  const res = await fetch(`${API_BASE}/api/config/${section}`);
  if (!res.ok) throw new Error(`Failed to fetch section: ${res.status}`);
  return res.json();
}

export async function updateSection(
  section: string,
  data: Record<string, unknown>
): Promise<{ section: string; version: number; status: string }> {
  const res = await fetch(`${API_BASE}/api/config/${section}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
  const body = await res.json();
  if (!res.ok)
    throw new Error(body.error || `Failed to update section: ${res.status}`);
  return body;
}

export async function fetchHistory(section: string): Promise<HistoryEntry[]> {
  const res = await fetch(`${API_BASE}/api/config/history/${section}`);
  if (!res.ok) throw new Error(`Failed to fetch history: ${res.status}`);
  return res.json();
}

export async function fetchStatus(): Promise<StatusResponse> {
  const res = await fetch(`${API_BASE}/api/status`);
  if (!res.ok) throw new Error(`Failed to fetch status: ${res.status}`);
  return res.json();
}

export async function fetchHealth(): Promise<HealthResponse> {
  const res = await fetch(`${API_BASE}/health`);
  if (!res.ok) throw new Error(`Failed to fetch health: ${res.status}`);
  return res.json();
}
