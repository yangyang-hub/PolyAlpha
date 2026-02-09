use rust_decimal::Decimal;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use chrono::{DateTime, Utc};
use pa_core::config::RiskConfig;

/// Circuit breaker that halts trading when loss thresholds are exceeded.
pub struct CircuitBreaker {
    max_daily_loss: Decimal,
    max_consecutive_losses: u32,
    daily_pnl: Mutex<Decimal>,
    consecutive_losses: AtomicU32,
    is_broken: AtomicBool,
    last_reset: Mutex<DateTime<Utc>>,
}

impl CircuitBreaker {
    pub fn new(config: &RiskConfig) -> Self {
        Self {
            max_daily_loss: config.max_daily_loss,
            max_consecutive_losses: config.circuit_breaker_consecutive_losses,
            daily_pnl: Mutex::new(Decimal::ZERO),
            consecutive_losses: AtomicU32::new(0),
            is_broken: AtomicBool::new(false),
            last_reset: Mutex::new(Utc::now()),
        }
    }

    /// Check if circuit breaker is triggered.
    pub fn is_broken(&self) -> bool {
        self.is_broken.load(Ordering::Relaxed)
    }

    /// Record a trade result and check if circuit breaker should trigger.
    pub fn record_trade(&self, pnl: Decimal) {
        if let Ok(mut daily) = self.daily_pnl.lock() {
            *daily += pnl;

            if *daily < -self.max_daily_loss {
                tracing::error!(daily_pnl = %*daily, "Circuit breaker: daily loss limit exceeded");
                self.is_broken.store(true, Ordering::Relaxed);
                return;
            }
        }

        if pnl < Decimal::ZERO {
            let losses = self.consecutive_losses.fetch_add(1, Ordering::SeqCst) + 1;
            if losses >= self.max_consecutive_losses {
                tracing::error!(
                    consecutive = losses,
                    "Circuit breaker: consecutive loss limit exceeded"
                );
                self.is_broken.store(true, Ordering::Relaxed);
            }
        } else {
            self.consecutive_losses.store(0, Ordering::SeqCst);
        }
    }

    /// Reset daily counters (called at midnight UTC).
    pub fn reset_daily(&self) {
        if let Ok(mut daily) = self.daily_pnl.lock() {
            *daily = Decimal::ZERO;
        }
        self.consecutive_losses.store(0, Ordering::SeqCst);
        self.is_broken.store(false, Ordering::Relaxed);
        if let Ok(mut last) = self.last_reset.lock() {
            *last = Utc::now();
        }
        tracing::info!("Circuit breaker reset for new day");
    }
}
