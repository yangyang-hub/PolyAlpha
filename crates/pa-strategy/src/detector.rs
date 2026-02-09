use pa_core::types::{MarketInfo, OrderBook};
use alloy::primitives::U256;

/// Scans markets and produces arbitrage opportunities using registered strategies.
pub struct ArbitrageDetector {
    /// Callback to get current order book for a token.
    get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
}

impl ArbitrageDetector {
    pub fn new(
        get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    ) -> Self {
        Self { get_orderbook }
    }

    /// Quick check: for a binary market, is the YES+NO ask sum < 1.00
    /// or bid sum > 1.00 by at least `threshold_bps`?
    pub fn quick_check_yesno(
        &self,
        market: &MarketInfo,
        threshold_bps: u32,
    ) -> bool {
        if market.tokens.len() != 2 {
            return false;
        }

        let yes_book = match (self.get_orderbook)(market.tokens[0].token_id) {
            Some(b) => b,
            None => return false,
        };
        let no_book = match (self.get_orderbook)(market.tokens[1].token_id) {
            Some(b) => b,
            None => return false,
        };

        // Check ask side: YES_ask + NO_ask < 1.00
        if let (Some(ya), Some(na)) = (yes_book.best_ask(), no_book.best_ask()) {
            let total = ya.price + na.price;
            let spread_bps = ((rust_decimal::Decimal::ONE - total)
                * rust_decimal::Decimal::from(10000))
                .to_string()
                .parse::<i32>()
                .unwrap_or(0);
            if spread_bps >= threshold_bps as i32 {
                return true;
            }
        }

        // Check bid side: YES_bid + NO_bid > 1.00
        if let (Some(yb), Some(nb)) = (yes_book.best_bid(), no_book.best_bid()) {
            let total = yb.price + nb.price;
            let spread_bps = ((total - rust_decimal::Decimal::ONE)
                * rust_decimal::Decimal::from(10000))
                .to_string()
                .parse::<i32>()
                .unwrap_or(0);
            if spread_bps >= threshold_bps as i32 {
                return true;
            }
        }

        false
    }
}
