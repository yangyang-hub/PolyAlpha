export interface MetricSample {
  name: string;
  labels: Record<string, string>;
  value: number;
}

function parseMetricToken(token: string): { name: string; labels: Record<string, string> } {
  const braceIndex = token.indexOf("{");
  if (braceIndex === -1 || !token.endsWith("}")) {
    return { name: token, labels: {} };
  }

  const name = token.slice(0, braceIndex);
  const labelsRaw = token.slice(braceIndex + 1, -1);
  const labels: Record<string, string> = {};

  for (const entry of labelsRaw.split(",")) {
    const [rawKey, rawValue] = entry.split("=");
    if (!rawKey || !rawValue) continue;
    labels[rawKey.trim()] = rawValue.trim().replace(/^"|"$/g, "");
  }

  return { name, labels };
}

export function parseMetricSamples(text: string): MetricSample[] {
  const samples: MetricSample[] = [];

  for (const line of text.split("\n")) {
    if (line.startsWith("#") || line.trim() === "") continue;
    const parts = line.trim().split(/\s+/);
    if (parts.length < 2) continue;

    const value = parseFloat(parts[1]);
    if (Number.isNaN(value)) continue;

    const { name, labels } = parseMetricToken(parts[0]);
    samples.push({ name, labels, value });
  }

  return samples;
}

/**
 * Parse Prometheus exposition text format into a Map<metric_name, value>.
 * For counters/gauges only (ignores histograms buckets/sum/count suffixes).
 *
 * Labeled metrics use their full token as the key, e.g.
 * `weather_rejections_total{reason="edge_too_small"}`.
 */
export function parseMetrics(text: string): Map<string, number> {
  const result = new Map<string, number>();
  for (const line of text.split("\n")) {
    if (line.startsWith("#") || line.trim() === "") continue;
    const parts = line.split(/\s+/);
    if (parts.length < 2) continue;
    const name = parts[0];
    const value = parseFloat(parts[1]);
    if (!Number.isNaN(value)) {
      result.set(name, value);
    }
  }
  return result;
}

export function metricSeriesByName(text: string, metricName: string): MetricSample[] {
  return parseMetricSamples(text).filter((sample) => sample.name === metricName);
}
