use alloy::primitives::{B256, U256};
use dashmap::DashMap;
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::sync::Arc;
use pa_core::types::StrategyType;

/// Loaded position entry: (token_id, size, avg_cost, strategy_type, condition_id).
pub type LoadedPosition = (U256, Decimal, Decimal, Option<StrategyType>, Option<B256>);

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
    pub strategy_type: Option<StrategyType>,
    pub condition_id: Option<B256>,
}

impl PositionTracker {
    pub fn new() -> Self {
        Self {
            positions: Arc::new(DashMap::new()),
        }
    }

    /// Update position after a fill.
    pub fn update(&self, token_id: U256, filled_size: Decimal, price: Decimal, is_buy: bool, strategy_type: Option<StrategyType>, condition_id: Option<B256>) {
        self.positions
            .entry(token_id)
            .and_modify(|entry| {
                if is_buy {
                    let total_cost = entry.avg_cost * entry.size + price * filled_size;
                    entry.size += filled_size;
                    if entry.size > Decimal::ZERO {
                        entry.avg_cost = total_cost / entry.size;
                    }
                    // Set strategy_type on first buy if not already set
                    if entry.strategy_type.is_none() {
                        entry.strategy_type = strategy_type;
                    }
                    // Set condition_id on first buy if not already set
                    if entry.condition_id.is_none() {
                        entry.condition_id = condition_id;
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
                strategy_type,
                condition_id,
            });

        // Remove zero-size entries to keep the map clean
        if !is_buy {
            if let Some(entry) = self.positions.get(&token_id) {
                if entry.size <= Decimal::ZERO {
                    drop(entry);
                    self.positions.remove(&token_id);
                }
            }
        }
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

    /// Get total share size across all tokens in the same market (by condition_id).
    pub fn size_by_market(&self, condition_id: &B256) -> Decimal {
        let mut total = Decimal::ZERO;
        for entry in self.positions.iter() {
            if entry.value().condition_id.as_ref() == Some(condition_id) && entry.value().size > Decimal::ZERO {
                total += entry.value().size;
            }
        }
        total
    }

    /// Get total USDC exposure for a market (sum of size × avg_cost for all tokens with matching condition_id).
    pub fn exposure_by_market(&self, condition_id: &B256) -> Decimal {
        let mut total = Decimal::ZERO;
        for entry in self.positions.iter() {
            if entry.value().condition_id.as_ref() == Some(condition_id) && entry.value().size > Decimal::ZERO {
                total += entry.value().size * entry.value().avg_cost;
            }
        }
        total
    }

    /// Get total USDC exposure for a strategy (sum of size × avg_cost for all positions by strategy).
    pub fn exposure_by_strategy(&self, st: StrategyType) -> Decimal {
        let mut total = Decimal::ZERO;
        for entry in self.positions.iter() {
            if entry.value().strategy_type == Some(st) && entry.value().size > Decimal::ZERO {
                total += entry.value().size * entry.value().avg_cost;
            }
        }
        total
    }

    /// Get number of distinct markets a strategy holds positions in.
    pub fn market_count_by_strategy(&self, st: StrategyType) -> usize {
        let mut markets: HashSet<B256> = HashSet::new();
        for entry in self.positions.iter() {
            if entry.value().strategy_type == Some(st)
                && entry.value().size > Decimal::ZERO
                && let Some(cid) = entry.value().condition_id
            {
                markets.insert(cid);
            }
        }
        markets.len()
    }

    /// Bulk-load positions from external data (e.g. DB) at startup.
    pub fn load_initial(&self, entries: Vec<LoadedPosition>) {
        for (token_id, size, avg_cost, strategy_type, condition_id) in entries {
            if size > Decimal::ZERO {
                self.positions.insert(token_id, PositionEntry { size, avg_cost, strategy_type, condition_id });
            }
        }
    }

    /// Sync a single position from external data (Data API reconciliation).
    /// Unlike `load_initial`, this handles zero-size entries (removes the position).
    pub fn sync_position(
        &self,
        token_id: U256,
        size: Decimal,
        avg_cost: Decimal,
        strategy_type: Option<StrategyType>,
        condition_id: Option<B256>,
    ) {
        if size > Decimal::ZERO {
            self.positions.entry(token_id)
                .and_modify(|e| {
                    e.size = size;
                    e.avg_cost = avg_cost;
                    if e.strategy_type.is_none() {
                        e.strategy_type = strategy_type;
                    }
                    if e.condition_id.is_none() {
                        e.condition_id = condition_id;
                    }
                })
                .or_insert(PositionEntry { size, avg_cost, strategy_type, condition_id });
        } else {
            // Remove the position entirely
            self.positions.remove(&token_id);
        }
    }

    /// Snapshot all positions for persistence (including zeros, so DB can clean up stale rows).
    pub fn snapshot_all(&self) -> Vec<(U256, PositionEntry)> {
        self.positions
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect()
    }

    /// Get all positions held by a specific strategy.
    /// Returns (token_id, size, avg_cost) for each position.
    pub fn positions_by_strategy(&self, st: StrategyType) -> Vec<(U256, Decimal, Decimal)> {
        self.positions
            .iter()
            .filter(|entry| {
                entry.value().strategy_type == Some(st) && entry.value().size > Decimal::ZERO
            })
            .map(|entry| (*entry.key(), entry.value().size, entry.value().avg_cost))
            .collect()
    }
}

impl Default for PositionTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{B256, U256};
    use rust_decimal_macros::dec;

    fn cid(n: u8) -> B256 {
        let mut bytes = [0u8; 32];
        bytes[31] = n;
        B256::from(bytes)
    }

    #[test]
    fn test_size_by_market() {
        let tracker = PositionTracker::new();
        let market_cid = cid(1);

        // Buy YES token in market 1
        tracker.update(U256::from(10), dec!(50), dec!(0.60), true, Some(StrategyType::Weather), Some(market_cid));
        // Buy NO token in market 1
        tracker.update(U256::from(11), dec!(30), dec!(0.40), true, Some(StrategyType::Weather), Some(market_cid));

        // Total shares in market = 50 + 30 = 80
        assert_eq!(tracker.size_by_market(&market_cid), dec!(80));

        // Different market should be zero
        assert_eq!(tracker.size_by_market(&cid(2)), dec!(0));
    }

    #[test]
    fn test_exposure_by_strategy() {
        let tracker = PositionTracker::new();

        // Weather strategy: 50 shares @ $0.60 = $30, 30 shares @ $0.40 = $12
        tracker.update(U256::from(10), dec!(50), dec!(0.60), true, Some(StrategyType::Weather), Some(cid(1)));
        tracker.update(U256::from(11), dec!(30), dec!(0.40), true, Some(StrategyType::Weather), Some(cid(2)));

        // CryptoAlpha strategy: 100 shares @ $0.50 = $50
        tracker.update(U256::from(20), dec!(100), dec!(0.50), true, Some(StrategyType::CryptoAlpha), Some(cid(3)));

        // Weather exposure = 50*0.60 + 30*0.40 = 30 + 12 = 42
        assert_eq!(tracker.exposure_by_strategy(StrategyType::Weather), dec!(42));
        // CryptoAlpha exposure = 100*0.50 = 50
        assert_eq!(tracker.exposure_by_strategy(StrategyType::CryptoAlpha), dec!(50));
    }

    #[test]
    fn test_market_count_by_strategy() {
        let tracker = PositionTracker::new();
        let market1 = cid(1);
        let market2 = cid(2);
        let market3 = cid(3);

        // Weather in 2 markets (market1 has YES+NO = still 1 market)
        tracker.update(U256::from(10), dec!(50), dec!(0.60), true, Some(StrategyType::Weather), Some(market1));
        tracker.update(U256::from(11), dec!(30), dec!(0.40), true, Some(StrategyType::Weather), Some(market1));
        tracker.update(U256::from(20), dec!(20), dec!(0.70), true, Some(StrategyType::Weather), Some(market2));

        // CryptoAlpha in 1 market
        tracker.update(U256::from(30), dec!(100), dec!(0.50), true, Some(StrategyType::CryptoAlpha), Some(market3));

        assert_eq!(tracker.market_count_by_strategy(StrategyType::Weather), 2);
        assert_eq!(tracker.market_count_by_strategy(StrategyType::CryptoAlpha), 1);
    }
}
