use alloy::primitives::U256;
use chrono::Utc;
use pa_core::types::{OrderBook, PriceLevel};
use rust_decimal::Decimal;

/// Manages a local order book from WebSocket updates.
#[derive(Debug)]
pub struct OrderBookManager;

impl OrderBookManager {
    /// Build an `OrderBook` from raw bid/ask level data.
    ///
    /// Bids are sorted descending (best first), asks sorted ascending (best first).
    pub fn build_orderbook(
        token_id: U256,
        bids: Vec<(Decimal, Decimal)>,
        asks: Vec<(Decimal, Decimal)>,
    ) -> OrderBook {
        let mut bid_levels: Vec<PriceLevel> = bids
            .into_iter()
            .map(|(price, size)| PriceLevel { price, size })
            .collect();
        bid_levels.sort_by(|a, b| b.price.cmp(&a.price));

        let mut ask_levels: Vec<PriceLevel> = asks
            .into_iter()
            .map(|(price, size)| PriceLevel { price, size })
            .collect();
        ask_levels.sort_by(|a, b| a.price.cmp(&b.price));

        OrderBook {
            token_id,
            bids: bid_levels,
            asks: ask_levels,
            timestamp: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_build_orderbook_sorts_correctly() {
        let bids = vec![(dec!(0.45), dec!(100)), (dec!(0.50), dec!(200))];
        let asks = vec![(dec!(0.55), dec!(150)), (dec!(0.52), dec!(300))];

        let book = OrderBookManager::build_orderbook(U256::from(1), bids, asks);

        // Bids sorted descending
        assert_eq!(book.best_bid().unwrap().price, dec!(0.50));
        // Asks sorted ascending
        assert_eq!(book.best_ask().unwrap().price, dec!(0.52));
        // Midpoint
        assert_eq!(book.midpoint().unwrap(), dec!(0.51));
    }
}
