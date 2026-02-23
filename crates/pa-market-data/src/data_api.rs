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
    /// Uses API default size_threshold (1.0 shares) to skip dust positions.
    pub async fn load_positions(&self) -> Result<Vec<ApiPosition>> {
        let mut all_positions = Vec::new();
        let mut offset = 0i32;
        let limit = 500i32;

        loop {
            let req = PositionsRequest::builder()
                .user(self.wallet)
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
}
