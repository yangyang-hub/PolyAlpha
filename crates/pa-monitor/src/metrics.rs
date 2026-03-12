use prometheus::{Gauge, Histogram, IntCounter, Registry, histogram_opts};
use std::sync::LazyLock;

/// Global Prometheus metrics registry.
pub static REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::new);

/// Number of trading opportunities detected.
pub static OPPORTUNITIES_DETECTED: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new(
        "opportunities_detected_total",
        "Total opportunities detected",
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Number of opportunities rejected by risk manager.
pub static OPPORTUNITIES_REJECTED: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new(
        "opportunities_rejected_total",
        "Opportunities rejected by risk checks",
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Number of trading opportunities executed.
pub static EXECUTIONS_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new("executions_total", "Total executions attempted").unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Number of execution errors.
pub static EXECUTION_ERRORS: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new("execution_errors_total", "Total execution errors").unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Execution latency (order submission to confirmation).
pub static EXECUTION_LATENCY: LazyLock<Histogram> = LazyLock::new(|| {
    let hist = Histogram::with_opts(histogram_opts!(
        "execution_latency_seconds",
        "Execution latency",
        vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
    ))
    .unwrap();
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
    let gauge = Gauge::new(
        "active_ws_subscriptions",
        "Number of active WebSocket subscriptions",
    )
    .unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

/// Number of monitored markets.
pub static MONITORED_MARKETS: LazyLock<Gauge> = LazyLock::new(|| {
    let gauge = Gauge::new("monitored_markets", "Number of actively monitored markets").unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

/// WebSocket reconnection attempts.
pub static WS_RECONNECT_COUNT: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new("ws_reconnect_total", "WebSocket reconnection attempts").unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Number of order book snapshots recorded to database.
pub static SNAPSHOTS_RECORDED: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new(
        "snapshots_recorded_total",
        "Order book snapshots saved to database",
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Whether the circuit breaker is currently active (1=triggered, 0=ok).
pub static CIRCUIT_BREAKER_ACTIVE: LazyLock<Gauge> = LazyLock::new(|| {
    let gauge = Gauge::new(
        "circuit_breaker_active",
        "Circuit breaker status (1=triggered, 0=ok)",
    )
    .unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

/// Current total exposure in USD.
pub static TOTAL_EXPOSURE: LazyLock<Gauge> = LazyLock::new(|| {
    let gauge = Gauge::new("total_exposure_usd", "Total position exposure in USD").unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

/// Strategy scan cycle latency.
pub static SCAN_LATENCY: LazyLock<Histogram> = LazyLock::new(|| {
    let hist = Histogram::with_opts(histogram_opts!(
        "scan_latency_seconds",
        "Strategy scan cycle latency",
        vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0]
    ))
    .unwrap();
    REGISTRY.register(Box::new(hist.clone())).unwrap();
    hist
});

/// Positions reduced by event calendar filter.
pub static EVENT_FILTER_APPLIED: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new(
        "event_filter_applied_total",
        "Positions reduced by event calendar filter",
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Total exit trades executed (model reversal / capital efficiency).
pub static EXIT_TRADES: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new("exit_trades_total", "Exit trades executed").unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Current USDC balance in Polymarket proxy wallet.
pub static USDC_BALANCE: LazyLock<Gauge> = LazyLock::new(|| {
    let gauge = Gauge::new("usdc_balance", "USDC balance in Polymarket proxy wallet").unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

/// Market value of all positions (sum of size × current_price).
pub static POSITIONS_MARKET_VALUE: LazyLock<Gauge> = LazyLock::new(|| {
    let gauge = Gauge::new(
        "positions_market_value_usd",
        "Market value of all open positions",
    )
    .unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

/// Opportunities scaled down due to insufficient order book depth.
pub static DEPTH_VALIDATION_SCALED: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new(
        "depth_validation_scaled_total",
        "Opportunities scaled down due to insufficient depth",
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Opportunities rejected due to zero order book depth.
pub static DEPTH_VALIDATION_REJECTED: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new(
        "depth_validation_rejected_total",
        "Opportunities rejected due to zero depth",
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Total liquidity rewards orders placed (bid + ask on YES/NO).
pub static LR_ORDERS_PLACED: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter =
        IntCounter::new("lr_orders_placed_total", "Liquidity rewards orders placed").unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Total liquidity rewards orders cancelled.
pub static LR_ORDERS_CANCELLED: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new(
        "lr_orders_cancelled_total",
        "Liquidity rewards orders cancelled",
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Number of markets actively being quoted for liquidity rewards.
pub static LR_ACTIVE_MARKETS: LazyLock<Gauge> = LazyLock::new(|| {
    let gauge = Gauge::new(
        "lr_active_markets",
        "Number of markets being quoted for liquidity rewards",
    )
    .unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

/// Number of LR order fills detected via CLOB polling.
pub static LR_FILLS_DETECTED: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new(
        "lr_fills_detected_total",
        "LR order fills detected via CLOB polling",
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Number of LR re-quotes triggered by fill detection.
pub static LR_FILL_REQUOTES: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new(
        "lr_fill_requotes_total",
        "LR re-quotes triggered by fill detection",
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Number of LR orders skipped because an identical order already exists.
pub static LR_ORDERS_SKIPPED_DUPLICATE: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new(
        "lr_orders_skipped_duplicate_total",
        "LR orders skipped because identical order already exists",
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

/// Number of LR orders that failed scoring verification.
pub static LR_ORDERS_NOT_SCORING: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new(
        "lr_orders_not_scoring_total",
        "LR orders that failed scoring verification",
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});
