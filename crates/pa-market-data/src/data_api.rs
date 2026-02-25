use alloy::primitives::{Address, B256, U256};
use anyhow::{Context, Result};
use rust_decimal::Decimal;
use polymarket_client_sdk::data::Client as DataApiClient;
use polymarket_client_sdk::data::types::request::PositionsRequest;

/// A position loaded from the Polymarket Data API.
pub struct ApiPosition {
    pub token_id: U256,
    pub size: Decimal,
    pub avg_price: Decimal,
    pub condition_id: B256,
}

/// A redeemable position (market resolved, tokens can be claimed).
pub struct RedeemablePosition {
    pub condition_id: B256,
    pub token_id: U256,
    pub size: Decimal,
    pub title: String,
    pub neg_risk: bool,
}

/// Loads positions from the Polymarket Data API (no authentication required).
pub struct PositionLoader {
    client: DataApiClient,
    wallet: Address,
}

impl PositionLoader {
    pub fn new(wallet: Address) -> Result<Self> {
        Ok(Self {
            client: DataApiClient::default(),
            wallet,
        })
    }

    /// Load all open positions for the configured wallet.
    ///
    /// Automatically paginates through results (limit=500 per page).
    /// Sets size_threshold=0 to include all positions (default=1 would miss small ones).
    pub async fn load_positions(&self) -> Result<Vec<ApiPosition>> {
        let mut all_positions = Vec::new();
        let mut offset = 0i32;
        let limit = 500i32;

        loop {
            let req = PositionsRequest::builder()
                .user(self.wallet)
                .size_threshold(Decimal::ZERO)
                .limit(limit)?
                .offset(offset)?
                .build();

            let page = self.client.positions(&req).await
                .context("Data API positions request failed")?;

            let page_len = page.len();

            for pos in page {
                all_positions.push(ApiPosition {
                    token_id: pos.asset,
                    size: pos.size,
                    avg_price: pos.avg_price,
                    condition_id: pos.condition_id,
                });
            }

            // If we got fewer than the limit, we've reached the end
            if (page_len as i32) < limit {
                break;
            }

            offset += limit;

            // Safety: don't paginate beyond 10000
            if offset >= 10000 {
                tracing::error!(
                    offset,
                    loaded = all_positions.len(),
                    "Data API pagination hard limit reached — risk manager exposure may be understated"
                );
                break;
            }
        }

        tracing::info!(
            wallet = %self.wallet,
            positions = all_positions.len(),
            "Positions loaded from Data API"
        );

        Ok(all_positions)
    }

    /// Find positions that are redeemable (market resolved).
    ///
    /// Uses the `redeemable=true` API filter to only fetch resolved positions.
    pub async fn find_redeemable(&self) -> Result<Vec<RedeemablePosition>> {
        let mut redeemable = Vec::new();
        let mut offset = 0i32;
        let limit = 500i32;

        loop {
            let req = PositionsRequest::builder()
                .user(self.wallet)
                .redeemable(true)
                .size_threshold(Decimal::ZERO)
                .limit(limit)?
                .offset(offset)?
                .build();

            let page = self.client.positions(&req).await
                .context("Data API redeemable check failed")?;

            let page_len = page.len();

            for pos in page {
                if pos.size > Decimal::ZERO {
                    redeemable.push(RedeemablePosition {
                        condition_id: pos.condition_id,
                        token_id: pos.asset,
                        size: pos.size,
                        title: pos.title,
                        neg_risk: pos.negative_risk,
                    });
                }
            }

            if (page_len as i32) < limit {
                break;
            }
            offset += limit;
            if offset >= 10000 {
                break;
            }
        }

        Ok(redeemable)
    }
}
