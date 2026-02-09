use prometheus::{IntCounter, Histogram, Gauge, Registry, histogram_opts};
use std::sync::LazyLock;

/// Global Prometheus metrics registry.
pub static REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::new);

/// Number of arbitrage opportunities detected.
pub static OPPORTUNITIES_DETECTED: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new("opportunities_detected_total", "Total opportunities detected").unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Number of arbitrage opportunities executed.
pub static OPPORTUNITIES_EXECUTED: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new("opportunities_executed_total", "Total opportunities executed").unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Number of opportunities skipped (risk/insufficient).
pub static OPPORTUNITIES_SKIPPED: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new("opportunities_skipped_total", "Total opportunities skipped").unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Trade execution latency (detection to completion).
pub static TRADE_LATENCY: LazyLock<Histogram> = LazyLock::new(|| {
    let hist = Histogram::with_opts(
        histogram_opts!("trade_latency_seconds", "Trade execution latency",
            vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0])
    ).unwrap();
    REGISTRY.register(Box::new(hist.clone())).unwrap();
    hist
});

/// Current realized PnL.
pub static REALIZED_PNL: LazyLock<Gauge> = LazyLock::new(|| {
    let gauge = Gauge::new("realized_pnl_usd", "Total realized PnL in USD").unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

/// Number of active WebSocket subscriptions.
pub static ACTIVE_SUBSCRIPTIONS: LazyLock<Gauge> = LazyLock::new(|| {
    let gauge = Gauge::new("active_ws_subscriptions", "Number of active WebSocket subscriptions").unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

/// Number of monitored markets.
pub static MONITORED_MARKETS: LazyLock<Gauge> = LazyLock::new(|| {
    let gauge = Gauge::new("monitored_markets", "Number of actively monitored markets").unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});
