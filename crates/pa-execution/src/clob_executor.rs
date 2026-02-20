use alloy::primitives::U256;
use alloy::signers::local::PrivateKeySigner;
use rust_decimal::Decimal;

use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::auth::Normal;
use polymarket_client_sdk::clob::types::{OrderType, Side, SignatureType};
use polymarket_client_sdk::clob::types::request::BalanceAllowanceRequest;
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
        let size = size.round_dp(2);
        if size <= Decimal::ZERO {
            anyhow::bail!("Order size too small after rounding to lot size");
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
        if size <= Decimal::ZERO {
            anyhow::bail!("Order size too small after rounding to lot size");
        }

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

    /// Place a GTC (Good-Til-Cancelled) limit buy order.
    /// Used for remaining inventory after partial arbitrage fills.
    pub async fn buy_limit(
        &self,
        token_id: U256,
        price: Decimal,
        size: Decimal,
    ) -> anyhow::Result<OrderResult> {
        // Round size to 2 decimal places (Polymarket CLOB lot size constraint)
        let size = size.round_dp(2);
        if size <= Decimal::ZERO {
            anyhow::bail!("Order size too small after rounding to lot size");
        }

        tracing::info!(
            token_id = %token_id,
            price = %price,
            size = %size,
            "Placing GTC limit buy order"
        );

        let signable = self
            .client
            .limit_order()
            .token_id(token_id)
            .side(Side::Buy)
            .price(price)
            .size(size)
            .order_type(OrderType::GTC)
            .build()
            .await?;

        let signed = self.client.sign(&self.signer, signable).await?;
        let response = self.client.post_order(signed).await?;

        Ok(Self::parse_response(response, price, size))
    }

    /// Place a GTC limit sell order.
    pub async fn sell_limit(
        &self,
        token_id: U256,
        price: Decimal,
        size: Decimal,
    ) -> anyhow::Result<OrderResult> {
        // Round size to 2 decimal places (Polymarket CLOB lot size constraint)
        let size = size.round_dp(2);
        if size <= Decimal::ZERO {
            anyhow::bail!("Order size too small after rounding to lot size");
        }

        tracing::info!(
            token_id = %token_id,
            price = %price,
            size = %size,
            "Placing GTC limit sell order"
        );

        let signable = self
            .client
            .limit_order()
            .token_id(token_id)
            .side(Side::Sell)
            .price(price)
            .size(size)
            .order_type(OrderType::GTC)
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
        tracing::debug!(
            balance = %response.balance,
            allowances = ?response.allowances,
            "CLOB balance_allowance response"
        );
        Ok(response.balance)
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
                OrderStatusType::Matched => {
                    // For FOK orders, matched means fully filled
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
