/**
 * Parse Prometheus exposition text format into a Map<metric_name, value>.
 * For counters/gauges only (ignores histograms buckets/sum/count suffixes).
 */
export function parseMetrics(text: string): Map<string, number> {
  const result = new Map<string, number>();
  for (const line of text.split("\n")) {
    if (line.startsWith("#") || line.trim() === "") continue;
    // e.g. "executions_total 42" or "execution_latency_seconds_bucket{le="0.05"} 0"
    const parts = line.split(/\s+/);
    if (parts.length < 2) continue;
    const name = parts[0];
    const value = parseFloat(parts[1]);
    if (!isNaN(value)) {
      result.set(name, value);
    }
  }
  return result;
}
