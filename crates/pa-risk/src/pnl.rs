use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::sync::Mutex;

/// Tracks realized and unrealized PnL.
pub struct PnlTracker {
    realized: Mutex<Decimal>,
    trade_count: Mutex<u64>,
    winning_trades: Mutex<u64>,
    started_at: DateTime<Utc>,
}

impl PnlTracker {
    pub fn new() -> Self {
        Self {
            realized: Mutex::new(Decimal::ZERO),
            trade_count: Mutex::new(0),
            winning_trades: Mutex::new(0),
            started_at: Utc::now(),
        }
    }

    pub fn record(&self, pnl: Decimal) {
        if let Ok(mut r) = self.realized.lock() {
            *r += pnl;
        }
        if let Ok(mut c) = self.trade_count.lock() {
            *c += 1;
        }
        if pnl > Decimal::ZERO {
            if let Ok(mut w) = self.winning_trades.lock() {
                *w += 1;
            }
        }
    }

    pub fn total_realized(&self) -> Decimal {
        self.realized.lock().map(|r| *r).unwrap_or(Decimal::ZERO)
    }

    pub fn trade_count(&self) -> u64 {
        self.trade_count.lock().map(|c| *c).unwrap_or(0)
    }

    pub fn win_rate(&self) -> f64 {
        let total = self.trade_count();
        if total == 0 {
            return 0.0;
        }
        let wins = self.winning_trades.lock().map(|w| *w).unwrap_or(0);
        wins as f64 / total as f64
    }

    pub fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }
}

impl Default for PnlTracker {
    fn default() -> Self {
        Self::new()
    }
}
