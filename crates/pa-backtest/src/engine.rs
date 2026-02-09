/// Backtest engine — replays historical order book data through strategies.
pub struct BacktestEngine;

impl BacktestEngine {
    // TODO: Phase 3 implementation
    // 1. Load orderbook_snapshots from database for time range
    // 2. Replay in chronological order
    // 3. Run strategy.scan() on each snapshot
    // 4. Simulate execution with slippage model
    // 5. Generate PnL report
}
