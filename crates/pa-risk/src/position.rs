use alloy::primitives::U256;
use dashmap::DashMap;
use rust_decimal::Decimal;
use std::sync::Arc;

/// Tracks current token positions across all markets.
#[derive(Debug, Clone)]
pub struct PositionTracker {
    /// token_id → (size, avg_cost)
    positions: Arc<DashMap<U256, PositionEntry>>,
}

#[derive(Debug, Clone)]
pub struct PositionEntry {
    pub size: Decimal,
    pub avg_cost: Decimal,
}

impl PositionTracker {
    pub fn new() -> Self {
        Self {
            positions: Arc::new(DashMap::new()),
        }
    }

    /// Update position after a fill.
    pub fn update(&self, token_id: U256, filled_size: Decimal, price: Decimal, is_buy: bool) {
        self.positions
            .entry(token_id)
            .and_modify(|entry| {
                if is_buy {
                    let total_cost = entry.avg_cost * entry.size + price * filled_size;
                    entry.size += filled_size;
                    if entry.size > Decimal::ZERO {
                        entry.avg_cost = total_cost / entry.size;
                    }
                } else {
                    entry.size -= filled_size;
                    if entry.size <= Decimal::ZERO {
                        entry.size = Decimal::ZERO;
                        entry.avg_cost = Decimal::ZERO;
                    }
                }
            })
            .or_insert(PositionEntry {
                size: if is_buy { filled_size } else { Decimal::ZERO },
                avg_cost: price,
            });
    }

    /// Get total exposure across all positions.
    pub fn total_exposure(&self) -> Decimal {
        let mut total = Decimal::ZERO;
        for entry in self.positions.iter() {
            total += entry.value().size * entry.value().avg_cost;
        }
        total
    }

    /// Get position size for a specific token.
    pub fn get_size(&self, token_id: &U256) -> Decimal {
        match self.positions.get(token_id) {
            Some(entry) => entry.value().size,
            None => Decimal::ZERO,
        }
    }

    /// Bulk-load positions from external data (e.g. DB) at startup.
    pub fn load_initial(&self, entries: Vec<(U256, Decimal, Decimal)>) {
        for (token_id, size, avg_cost) in entries {
            if size > Decimal::ZERO {
                self.positions.insert(token_id, PositionEntry { size, avg_cost });
            }
        }
    }

    /// Snapshot all positions for persistence (including zeros, so DB can clean up stale rows).
    pub fn snapshot_all(&self) -> Vec<(U256, PositionEntry)> {
        self.positions
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect()
    }
}

impl Default for PositionTracker {
    fn default() -> Self {
        Self::new()
    }
}
