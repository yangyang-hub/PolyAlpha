use alloy::primitives::{B256, U256};
use alloy::signers::local::PrivateKeySigner;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::auth::Normal;
use polymarket_client_sdk::clob::types::{OrderType, Side, SignatureType};
use polymarket_client_sdk::clob::types::request::{BalanceAllowanceRequest, OrdersRequest};
use polymarket_client_sdk::clob::types::response::PostOrderResponse;

/// CLOB order executor using the Polymarket SDK.
///
/// Handles order construction, signing, and submission via the CLOB API.
/// Requires an authenticated CLOB client and a local signer for order signing.
pub struct ClobExecutor {
    client: polymarket_client_sdk::clob::Client<Authenticated<Normal>>,
    signer: PrivateKeySigner,
}

impl ClobExecutor {
    /// Create a new ClobExecutor with an already-authenticated CLOB client and signer.
    pub fn new(
        client: polymarket_client_sdk::clob::Client<Authenticated<Normal>>,
        signer: PrivateKeySigner,
    ) -> Self {
        Self { client, signer }
    }

    /// Authenticate with the CLOB API and create an executor.
    ///
    /// # Arguments
    /// * `host` - CLOB API host (e.g. "https://clob.polymarket.com")
    /// * `signer` - Local wallet signer with chain_id set (137 for Polygon)
    /// * `signature_type` - 0=EOA, 1=Proxy, 2=GnosisSafe
    pub async fn connect(host: &str, signer: PrivateKeySigner, signature_type: u8) -> anyhow::Result<Self> {
        let unauth_client = polymarket_client_sdk::clob::Client::new(
            host,
            polymarket_client_sdk::clob::Config::default(),
        )?;

        let sig_type = match signature_type {
            1 => SignatureType::Proxy,
            2 => SignatureType::GnosisSafe,
            _ => SignatureType::Eoa,
        };

        let client = unauth_client
            .authentication_builder(&signer)
            .signature_type(sig_type)
            .authenticate()
            .await?;

        tracing::info!(
            signature_type = signature_type,
            sig_type = ?sig_type,
            "CLOB authenticated with signature type"
        );

        Ok(Self { client, signer })
    }

    /// Compute the step size (in hundredths) for valid cost-precision-compliant sizes at a given price.
    ///
    /// Returns `(s_step, numer, denom)` where valid sizes are multiples of `s_step / 100`.
    fn cost_precision_step(price: Decimal) -> (u64, u64, u64) {
        let scale = price.scale();
        let denom = 10u64.pow(scale);
        let numer = (price * Decimal::from(denom))
            .round()
            .to_u64()
            .unwrap_or(1);
        if numer == 0 {
            return (1, 0, denom);
        }
        let g = gcd(numer, denom);
        let s_step = denom / g;
        (s_step, numer, denom)
    }

    /// Adjust order size so that `price * size` (USDC cost) has at most 2 decimal places.
    ///
    /// The Polymarket CLOB API requires maker_amount (USDC) to have max 2dp precision.
    /// Without this, orders like price=0.984 * size=22.67 = 22.30728 (5dp) get rejected.
    ///
    /// Uses GCD-based arithmetic to find the largest valid size ≤ the input.
    /// For price = N / 10^scale, we need `N * S / 10^scale` to be integer (where S = size * 100).
    /// This requires S to be a multiple of `10^scale / gcd(N, 10^scale)`.
    fn adjust_size_for_cost_precision(price: Decimal, size: Decimal) -> Decimal {
        let cost = price * size;
        if cost == cost.round_dp(2) {
            return size;
        }

        let (s_step, numer, _) = Self::cost_precision_step(price);
        if numer == 0 {
            return size;
        }

        // Round S down to nearest multiple of s_step
        let s_val = (size * Decimal::from(100u64))
            .round()
            .to_u64()
            .unwrap_or(0);

        if s_step == 0 || s_val < s_step {
            return Decimal::ZERO;
        }

        let s_rounded = (s_val / s_step) * s_step;
        Decimal::new(s_rounded as i64, 2)
    }

    /// Find the smallest cost-precision-valid size where `price * size >= $1.00`.
    ///
    /// When `adjust_size_for_cost_precision` rounds a size DOWN below the $1.00 CLOB minimum,
    /// this function computes the next valid size UP that meets the minimum.
    /// Returns `Decimal::ZERO` if no valid size can be found within reasonable bounds.
    fn min_cost_adjusted_size(price: Decimal) -> Decimal {
        if price <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let (s_step, numer, denom) = Self::cost_precision_step(price);
        if numer == 0 || s_step == 0 {
            return Decimal::ZERO;
        }

        // We need: price * (s / 100) >= 1.00
        // i.e.  numer * s / (denom * 100) >= 1
        // i.e.  s >= (denom * 100) / numer   (ceiling)
        let target = denom * 100;
        let min_s = (target + numer - 1) / numer; // ceiling division

        // Round up to next multiple of s_step
        let s = ((min_s + s_step - 1) / s_step) * s_step;

        // Safety cap: don't produce absurdly large orders (>50 shares)
        if s > 5000 {
            return Decimal::ZERO;
        }

        Decimal::new(s as i64, 2)
    }

    /// Place a FOK (Fill-Or-Kill) buy order for a given token.
    ///
    /// Builds a limit order via the SDK's order builder, signs it, and posts it.
    /// FOK ensures the order is either fully filled immediately or cancelled entirely,
    /// preventing single-side exposure in arbitrage.
    pub async fn buy_fok(
        &self,
        token_id: U256,
        price: Decimal,
        size: Decimal,
    ) -> anyhow::Result<OrderResult> {
        // Round size to 2 decimal places (Polymarket CLOB lot size constraint)
        let requested_size = size.round_dp(2);
        // Ensure price * size (USDC cost) has at most 2 decimal places
        let mut size = Self::adjust_size_for_cost_precision(price, requested_size);
        if size <= Decimal::ZERO {
            // Size too small for this price's lot step (e.g. price=0.942 needs
            // size >= 5.00).  Try bumping to the minimum valid lot size, but cap
            // at 5× the requested cost to avoid oversized orders.
            let min_size = Self::min_cost_adjusted_size(price);
            let max_cost = requested_size * price * Decimal::from(5u32);
            if min_size > Decimal::ZERO && min_size * price <= max_cost {
                tracing::info!(
                    token_id = %token_id, price = %price,
                    requested_size = %requested_size, bumped_size = %min_size,
                    "Size below lot step — bumping to minimum valid lot size"
                );
                size = min_size;
            } else {
                anyhow::bail!("Order size too small after rounding to lot size");
            }
        }
        // Polymarket minimum marketable order cost is $1.00
        // If precision adjustment reduced size below minimum, bump up to the
        // smallest valid size that meets $1.00 — but cap at the requested size
        // to avoid bypassing risk controls.
        let original_size = size;
        if price * size < Decimal::ONE {
            let bumped = Self::min_cost_adjusted_size(price);
            if bumped > Decimal::ZERO && bumped <= requested_size.max(original_size) {
                size = bumped;
            }
        }
        let cost = price * size;
        if cost < Decimal::ONE {
            tracing::debug!(
                token_id = %token_id, price = %price, size = %size, cost = %cost,
                original_size = %original_size,
                "Order cost below $1.00 minimum after precision adjustment, skipping"
            );
            return Ok(OrderResult {
                order_id: String::new(),
                filled_size: Decimal::ZERO,
                avg_price: price,
                status: OrderFillStatus::Rejected,
                tx_hashes: vec![],
            });
        }

        tracing::info!(
            token_id = %token_id,
            price = %price,
            size = %size,
            "Placing FOK buy order"
        );

        let signable = self
            .client
            .limit_order()
            .token_id(token_id)
            .side(Side::Buy)
            .price(price)
            .size(size)
            .order_type(OrderType::FOK)
            .build()
            .await?;

        let signed = self.client.sign(&self.signer, signable).await?;
        let response = self.client.post_order(signed).await?;

        tracing::info!(
            order_id = %response.order_id,
            status = ?response.status,
            success = response.success,
            "FOK buy order posted"
        );

        Ok(Self::parse_response(response, price, size))
    }

    /// Place a FOK sell order for a given token.
    pub async fn sell_fok(
        &self,
        token_id: U256,
        price: Decimal,
        size: Decimal,
    ) -> anyhow::Result<OrderResult> {
        // Round size to 2 decimal places (Polymarket CLOB lot size constraint)
        let size = size.round_dp(2);
        let size = Self::adjust_size_for_cost_precision(price, size);
        if size <= Decimal::ZERO {
            anyhow::bail!("Order size too small after rounding to lot size");
        }
        // No $1.00 minimum for sell orders — exiting positions should always be allowed.
        // The CLOB may still reject very small sells, but we let it decide.

        tracing::info!(
            token_id = %token_id,
            price = %price,
            size = %size,
            "Placing FOK sell order"
        );

        let signable = self
            .client
            .limit_order()
            .token_id(token_id)
            .side(Side::Sell)
            .price(price)
            .size(size)
            .order_type(OrderType::FOK)
            .build()
            .await?;

        let signed = self.client.sign(&self.signer, signable).await?;
        let response = self.client.post_order(signed).await?;

        tracing::info!(
            order_id = %response.order_id,
            status = ?response.status,
            success = response.success,
            "FOK sell order posted"
        );

        Ok(Self::parse_response(response, price, size))
    }

    /// Place a GTC limit buy order with post-only flag.
    /// The order will be rejected if it would immediately match (cross the spread).
    /// Used by Liquidity Rewards to ensure orders always rest on the book.
    pub async fn buy_limit_post_only(
        &self,
        token_id: U256,
        price: Decimal,
        size: Decimal,
    ) -> anyhow::Result<OrderResult> {
        // Round size to 2 decimal places (Polymarket CLOB lot size constraint)
        let size = size.round_dp(2);
        let mut size = Self::adjust_size_for_cost_precision(price, size);
        if size <= Decimal::ZERO {
            anyhow::bail!("Order size too small after rounding to lot size");
        }
        let original_size = size;
        if price * size < Decimal::ONE {
            let bumped = Self::min_cost_adjusted_size(price);
            if bumped > Decimal::ZERO && bumped <= original_size {
                size = bumped;
            }
        }
        let cost = price * size;
        if cost < Decimal::ONE {
            tracing::debug!(
                token_id = %token_id, price = %price, size = %size, cost = %cost,
                "Post-only buy order cost below $1.00 minimum, skipping"
            );
            return Ok(OrderResult {
                order_id: String::new(),
                filled_size: Decimal::ZERO,
                avg_price: price,
                status: OrderFillStatus::Rejected,
                tx_hashes: vec![],
            });
        }

        tracing::info!(
            token_id = %token_id,
            price = %price,
            size = %size,
            "Placing GTC post-only limit buy order"
        );

        let signable = self
            .client
            .limit_order()
            .token_id(token_id)
            .side(Side::Buy)
            .price(price)
            .size(size)
            .order_type(OrderType::GTC)
            .post_only(true)
            .build()
            .await?;

        let signed = self.client.sign(&self.signer, signable).await?;
        let response = self.client.post_order(signed).await?;

        Ok(Self::parse_response(response, price, size))
    }

    /// Place a GTC limit sell order with post-only flag.
    /// The order will be rejected if it would immediately match (cross the spread).
    /// Used by Liquidity Rewards to ensure orders always rest on the book.
    pub async fn sell_limit_post_only(
        &self,
        token_id: U256,
        price: Decimal,
        size: Decimal,
    ) -> anyhow::Result<OrderResult> {
        // Round size to 2 decimal places (Polymarket CLOB lot size constraint)
        let size = size.round_dp(2);
        let size = Self::adjust_size_for_cost_precision(price, size);
        if size <= Decimal::ZERO {
            anyhow::bail!("Order size too small after rounding to lot size");
        }

        tracing::info!(
            token_id = %token_id,
            price = %price,
            size = %size,
            "Placing GTC post-only limit sell order"
        );

        let signable = self
            .client
            .limit_order()
            .token_id(token_id)
            .side(Side::Sell)
            .price(price)
            .size(size)
            .order_type(OrderType::GTC)
            .post_only(true)
            .build()
            .await?;

        let signed = self.client.sign(&self.signer, signable).await?;
        let response = self.client.post_order(signed).await?;

        Ok(Self::parse_response(response, price, size))
    }

    /// Cancel all outstanding orders.
    pub async fn cancel_all(&self) -> anyhow::Result<()> {
        tracing::info!("Cancelling all orders");
        let response = self.client.cancel_all_orders().await?;
        tracing::info!(
            cancelled = response.canceled.len(),
            not_cancelled = response.not_canceled.len(),
            "Cancel all orders complete"
        );
        Ok(())
    }

    /// Cancel a single order by its ID.
    pub async fn cancel_order(&self, order_id: &str) -> anyhow::Result<()> {
        let response = self.client.cancel_order(order_id).await?;
        if !response.not_canceled.is_empty() {
            tracing::warn!(
                order_id,
                errors = ?response.not_canceled,
                "Order cancel partially failed"
            );
        }
        Ok(())
    }

    /// Cancel multiple orders by their IDs.
    pub async fn cancel_orders(&self, order_ids: &[&str]) -> anyhow::Result<()> {
        if order_ids.is_empty() {
            return Ok(());
        }
        let response = self.client.cancel_orders(order_ids).await?;
        tracing::debug!(
            cancelled = response.canceled.len(),
            not_cancelled = response.not_canceled.len(),
            "Batch cancel complete"
        );
        Ok(())
    }

    /// Query available USDC collateral balance from the CLOB API.
    ///
    /// This returns the balance held in the Polymarket proxy wallet,
    /// not the EOA wallet balance.
    pub async fn get_balance(&self) -> anyhow::Result<Decimal> {
        let request = BalanceAllowanceRequest::default(); // AssetType::Collateral
        let response = self.client.balance_allowance(request).await?;
        // CLOB API returns balance in micro-USDC (6 decimal raw on-chain units).
        // Convert to human-readable USDC.
        let balance_usdc = response.balance / Decimal::from(1_000_000u64);
        tracing::debug!(
            raw = %response.balance,
            usdc = %balance_usdc,
            allowances = ?response.allowances,
            "CLOB balance_allowance response"
        );
        Ok(balance_usdc)
    }

    /// Check whether the given orders are currently scoring for liquidity rewards.
    ///
    /// Returns a map of order_id -> is_scoring. Uses the CLOB `are_orders_scoring` endpoint.
    pub async fn are_orders_scoring(&self, order_ids: &[&str]) -> anyhow::Result<std::collections::HashMap<String, bool>> {
        let result = self.client.are_orders_scoring(order_ids).await?;
        Ok(result)
    }

    /// Query all orders for a given market condition_id, paginating to completion.
    ///
    /// Returns a summary of each order's status, useful for detecting fills on
    /// resting GTC orders.
    pub async fn get_orders_by_market(&self, condition_id: B256) -> anyhow::Result<Vec<OpenOrderInfo>> {
        use polymarket_client_sdk::clob::types::OrderStatusType;

        let req = OrdersRequest::builder()
            .market(condition_id)
            .build();

        let mut all_orders = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let page = self.client.orders(&req, cursor).await?;

            for o in &page.data {
                let is_matched = matches!(o.status, OrderStatusType::Matched | OrderStatusType::Delayed);
                let is_buy = matches!(o.side, Side::Buy);
                all_orders.push(OpenOrderInfo {
                    order_id: o.id.clone(),
                    is_matched,
                    is_buy,
                    token_id: o.asset_id,
                    price: o.price,
                    original_size: o.original_size,
                    size_matched: o.size_matched,
                });
            }

            if page.next_cursor.is_empty() || page.next_cursor == "LTE=" {
                break;
            }
            cursor = Some(page.next_cursor);
        }

        Ok(all_orders)
    }

    /// Parse the SDK's PostOrderResponse into our OrderResult.
    fn parse_response(response: PostOrderResponse, price: Decimal, size: Decimal) -> OrderResult {
        use polymarket_client_sdk::clob::types::OrderStatusType;

        let status = if !response.success {
            if let Some(ref msg) = response.error_msg {
                tracing::warn!(error = %msg, "Order rejected");
            }
            OrderFillStatus::Rejected
        } else {
            match response.status {
                OrderStatusType::Matched | OrderStatusType::Delayed => {
                    // Matched = filled synchronously.
                    // Delayed = filled but settlement is async (common for FOK).
                    // Both mean the order was accepted and matched by the CLOB.
                    OrderFillStatus::Filled
                }
                OrderStatusType::Live => {
                    // Order is resting on book (GTC)
                    OrderFillStatus::PartialFill
                }
                _ => OrderFillStatus::NoFill,
            }
        };

        // The SDK returns making_amount and taking_amount in the response.
        // For a buy: taking_amount = USDC spent, making_amount = shares received
        // For FOK: if matched, filled_size = requested size
        let filled_size = if status == OrderFillStatus::Filled {
            size
        } else if status == OrderFillStatus::Rejected || status == OrderFillStatus::NoFill {
            Decimal::ZERO
        } else {
            // Partial fill: estimate from response amounts
            if response.taking_amount > Decimal::ZERO && price > Decimal::ZERO {
                response.taking_amount / price
            } else {
                Decimal::ZERO
            }
        };

        OrderResult {
            order_id: response.order_id,
            filled_size,
            avg_price: price,
            status,
            tx_hashes: response.transaction_hashes,
        }
    }
    
    /// Fetch current liquidity rewards from the CLOB API.
    ///
    /// Returns paginated results of markets with active rewards.
    /// The API expects next_cursor to be a string (like "0", "1", "2") for pagination.
    /// Pass Some("0") for the first page, Some("1") for the second, etc.
    pub async fn current_rewards(
        &self,
        next_cursor: Option<String>,
    ) -> anyhow::Result<polymarket_client_sdk::clob::types::response::Page<polymarket_client_sdk::clob::types::response::CurrentRewardResponse>> {
        Ok(self.client.current_rewards(next_cursor).await?)
    }
}

#[derive(Debug, Clone)]
pub struct OrderResult {
    pub order_id: String,
    pub filled_size: Decimal,
    pub avg_price: Decimal,
    pub status: OrderFillStatus,
    pub tx_hashes: Vec<alloy::primitives::B256>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderFillStatus {
    Filled,
    PartialFill,
    NoFill,
    Rejected,
}

/// Summary of an order's state from the CLOB API.
#[derive(Debug, Clone)]
pub struct OpenOrderInfo {
    pub order_id: String,
    /// Whether the order is fully matched (status == Matched or Delayed).
    pub is_matched: bool,
    /// Whether this is a buy order.
    pub is_buy: bool,
    /// The token (asset) this order is for.
    pub token_id: U256,
    pub price: Decimal,
    pub original_size: Decimal,
    pub size_matched: Decimal,
}

/// Greatest common divisor (Euclidean algorithm).
fn gcd(a: u64, b: u64) -> u64 {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// Verify that `price * adjusted_size` has at most 2 decimal places.
    fn assert_valid_cost(price: Decimal, original_size: Decimal) {
        let adjusted = ClobExecutor::adjust_size_for_cost_precision(price, original_size);
        let cost = price * adjusted;
        assert_eq!(
            cost,
            cost.round_dp(2),
            "price={} size={} adjusted={} cost={} has >2dp",
            price,
            original_size,
            adjusted,
            cost,
        );
        assert!(
            adjusted <= original_size,
            "adjusted {} > original {}",
            adjusted,
            original_size,
        );
        assert!(
            adjusted > Decimal::ZERO || original_size <= Decimal::ZERO,
            "adjusted is zero for non-trivial original_size={}",
            original_size,
        );
    }

    #[test]
    fn test_cost_precision_already_valid() {
        // 0.50 * 22.66 = 11.33 (exactly 2dp)
        let result = ClobExecutor::adjust_size_for_cost_precision(dec!(0.50), dec!(22.66));
        assert_eq!(result, dec!(22.66));
    }

    #[test]
    fn test_cost_precision_3dp_price() {
        // price=0.984 (3dp), size=22.67 → cost=22.30728 (5dp)
        // Expected: step_s = 1000/gcd(984,1000) = 1000/8 = 125
        // s_val=2267, rounded=2250 → size=22.50
        // check: 0.984 * 22.50 = 22.14 (2dp) ✓
        assert_valid_cost(dec!(0.984), dec!(22.67));
        let result = ClobExecutor::adjust_size_for_cost_precision(dec!(0.984), dec!(22.67));
        assert_eq!(result, dec!(22.50));
    }

    #[test]
    fn test_cost_precision_2dp_prices() {
        // Common convergence prices
        assert_valid_cost(dec!(0.95), dec!(20.00));
        assert_valid_cost(dec!(0.95), dec!(22.67));
        assert_valid_cost(dec!(0.97), dec!(22.67));
        assert_valid_cost(dec!(0.98), dec!(22.67));
        assert_valid_cost(dec!(0.99), dec!(22.67));
        assert_valid_cost(dec!(0.80), dec!(50.00));
        assert_valid_cost(dec!(0.01), dec!(100.00));
    }

    #[test]
    fn test_cost_precision_extreme_prices() {
        assert_valid_cost(dec!(0.50), dec!(10.00));
        assert_valid_cost(dec!(0.25), dec!(10.00));
        assert_valid_cost(dec!(0.10), dec!(10.00));
        assert_valid_cost(dec!(0.01), dec!(10.00));
        assert_valid_cost(dec!(0.99), dec!(10.00));
    }

    #[test]
    fn test_cost_precision_various_3dp() {
        assert_valid_cost(dec!(0.123), dec!(50.00));
        assert_valid_cost(dec!(0.456), dec!(33.33));
        assert_valid_cost(dec!(0.789), dec!(12.34));
        assert_valid_cost(dec!(0.001), dec!(100.00));
        assert_valid_cost(dec!(0.999), dec!(100.00));
    }

    #[test]
    fn test_gcd() {
        assert_eq!(gcd(984, 1000), 8);
        assert_eq!(gcd(50, 100), 50);
        assert_eq!(gcd(97, 100), 1);
        assert_eq!(gcd(0, 100), 100);
        assert_eq!(gcd(100, 0), 100);
        assert_eq!(gcd(12, 18), 6);
    }

    /// Verify min_cost_adjusted_size produces valid costs.
    fn assert_min_cost_valid(price: Decimal) {
        let size = ClobExecutor::min_cost_adjusted_size(price);
        if size > Decimal::ZERO {
            let cost = price * size;
            assert!(
                cost >= Decimal::ONE,
                "price={} min_size={} cost={} < $1.00",
                price, size, cost,
            );
            assert_eq!(
                cost, cost.round_dp(2),
                "price={} min_size={} cost={} has >2dp",
                price, size, cost,
            );
        }
    }

    #[test]
    fn test_min_cost_adjusted_size_common_prices() {
        // price=0.82: s_step=50, min_size=1.50, cost=1.23
        let size = ClobExecutor::min_cost_adjusted_size(dec!(0.82));
        assert_eq!(size, dec!(1.50));
        assert_eq!(dec!(0.82) * size, dec!(1.23));

        // price=0.84: s_step=25, min_size=1.25, cost=1.05
        let size = ClobExecutor::min_cost_adjusted_size(dec!(0.84));
        assert_eq!(size, dec!(1.25));
        assert_eq!(dec!(0.84) * size, dec!(1.05));

        // price=0.97: s_step=100, min_size=2.00, cost=1.94
        let size = ClobExecutor::min_cost_adjusted_size(dec!(0.97));
        assert_eq!(size, dec!(2.00));
        assert_eq!(dec!(0.97) * size, dec!(1.94));

        // price=0.98: s_step=50, min_size=1.50, cost=1.47
        let size = ClobExecutor::min_cost_adjusted_size(dec!(0.98));
        assert_eq!(size, dec!(1.50));
        assert_eq!(dec!(0.98) * size, dec!(1.47));

        // price=0.95: s_step=20, min_size=1.20, cost=1.14
        let size = ClobExecutor::min_cost_adjusted_size(dec!(0.95));
        assert_eq!(size, dec!(1.20));
        assert_eq!(dec!(0.95) * size, dec!(1.14));
    }

    #[test]
    fn test_min_cost_adjusted_size_3dp_prices() {
        // price=0.954: s_step=500, min_size=5.00, cost=4.77
        let size = ClobExecutor::min_cost_adjusted_size(dec!(0.954));
        assert_eq!(size, dec!(5.00));

        // price=0.981: s_step=1000/gcd(981,1000)=1000/1=1000
        // min_s needs to be ≥ 100000/981 ≈ 102, rounded up to 1000
        // size = 10.00, cost = 9.81 — too expensive for small balance
        let size = ClobExecutor::min_cost_adjusted_size(dec!(0.981));
        assert_eq!(size, dec!(10.00));

        // price=0.949: s_step=1000/gcd(949,1000)=1000/1=1000
        // size = 10.00, cost = 9.49
        let size = ClobExecutor::min_cost_adjusted_size(dec!(0.949));
        assert_eq!(size, dec!(10.00));
    }

    #[test]
    fn test_min_cost_adjusted_size_validity() {
        // All common prices should produce valid costs
        for price in [dec!(0.50), dec!(0.75), dec!(0.80), dec!(0.82), dec!(0.84),
                      dec!(0.90), dec!(0.95), dec!(0.97), dec!(0.98), dec!(0.99)] {
            assert_min_cost_valid(price);
        }
    }

    #[test]
    fn test_adjust_then_bump_flow() {
        // Simulate the actual flow: adjust rounds down, bump recovers
        let price = dec!(0.82);
        let size = dec!(1.23); // typical after round_dp(2)

        // Step 1: precision adjustment rounds to 1.00
        let adjusted = ClobExecutor::adjust_size_for_cost_precision(price, size);
        assert_eq!(adjusted, dec!(1.00));
        assert!(price * adjusted < Decimal::ONE); // cost = 0.82 < 1.00

        // Step 2: bump to minimum valid
        let bumped = ClobExecutor::min_cost_adjusted_size(price);
        assert_eq!(bumped, dec!(1.50));
        assert!(price * bumped >= Decimal::ONE); // cost = 1.23 >= 1.00
    }
}
