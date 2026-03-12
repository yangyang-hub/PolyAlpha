use alloy::primitives::{Address, B256, U256};
use anyhow::{Context, Result};
use polymarket_client_sdk::data::Client as DataApiClient;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use rust_decimal::Decimal;

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
    /// Which outcome this position is (0=YES, 1=NO for binary; 0..N for multi-outcome).
    pub outcome_index: i32,
}

/// Raw position response from Data API with flexible `endDate` handling.
/// The SDK's deserialization fails when `endDate=""` (empty string), so we use a fallback.
#[derive(Debug, serde::Deserialize)]
struct RawPosition {
    #[serde(rename = "asset")]
    pub asset: U256,
    #[serde(rename = "size")]
    pub size: Decimal,
    #[serde(rename = "avgPrice")]
    pub avg_price: Decimal,
    #[serde(rename = "conditionId")]
    pub condition_id: B256,
    #[serde(rename = "title")]
    pub title: String,
    #[serde(rename = "negativeRisk")]
    #[serde(default)]
    pub negative_risk: bool,
    #[serde(rename = "outcomeIndex")]
    pub outcome_index: i32,
}

/// Fallback raw response wrapper that tolerates empty endDate strings.
#[derive(Debug, serde::Deserialize)]
struct RawPositionsResponse {
    #[serde(rename = "positions")]
    pub positions: Vec<RawPosition>,
}

/// Loads positions from the Polymarket Data API (no authentication required).
pub struct PositionLoader {
    client: DataApiClient,
    wallet: Address,
    /// Polymarket Data API base URL (for fallback raw requests).
    api_base: String,
}

impl PositionLoader {
    pub fn new(wallet: Address) -> Result<Self> {
        Ok(Self {
            client: DataApiClient::default(),
            wallet,
            api_base: "https://data-api.polymarket.com".to_string(),
        })
    }

    /// Load all open positions for the configured wallet.
    ///
    /// Automatically paginates through results (limit=500 per page).
    /// Sets size_threshold=0 to include all positions (default=1 would miss small ones).
    ///
    /// Falls back to raw HTTP requests if SDK deserialization fails (e.g., empty endDate).
    pub async fn load_positions(&self) -> Result<Vec<ApiPosition>> {
        match self.try_load_positions_sdk().await {
            Ok(positions) => Ok(positions),
            Err(e) => {
                // Check if this is a deserialization error (likely empty endDate).
                // Use {:#} to include the full error chain, not just the outermost .context().
                let error_chain = format!("{:#}", e).to_lowercase();
                if error_chain.contains("deserialization")
                    || error_chain.contains("premature end")
                    || error_chain.contains("invalid type")
                    || error_chain.contains("enddate")
                {
                    tracing::info!(
                        error = %e,
                        wallet = %self.wallet,
                        "Data API SDK hit known empty endDate bug, using raw HTTP fallback"
                    );
                    self.load_positions_fallback().await
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Try to load positions using the SDK client (normal path).
    async fn try_load_positions_sdk(&self) -> Result<Vec<ApiPosition>> {
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

            let page = self
                .client
                .positions(&req)
                .await
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

    /// Fallback: Load positions using raw HTTP requests with lenient JSON parsing.
    /// This bypasses the SDK's strict deserialization that fails on empty endDate strings.
    async fn load_positions_fallback(&self) -> Result<Vec<ApiPosition>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .context("Failed to create HTTP client for fallback")?;

        let mut all_positions = Vec::new();
        let mut offset = 0i32;
        let limit = 500i32;

        loop {
            let url = format!(
                "{}/positions?user={}&sizeThreshold=0&limit={}&offset={}",
                self.api_base, self.wallet, limit, offset
            );

            let resp = client
                .get(&url)
                .send()
                .await
                .context("Fallback HTTP request failed")?;

            if !resp.status().is_success() {
                return Err(anyhow::anyhow!(
                    "Fallback HTTP request failed with status {}",
                    resp.status()
                ));
            }

            let raw: Vec<RawPosition> =
                resp.json().await.context("Fallback JSON parsing failed")?;

            let page_len = raw.len();

            for pos in raw {
                all_positions.push(ApiPosition {
                    token_id: pos.asset,
                    size: pos.size,
                    avg_price: pos.avg_price,
                    condition_id: pos.condition_id,
                });
            }

            if (page_len as i32) < limit {
                break;
            }

            offset += limit;
            if offset >= 10000 {
                tracing::error!(
                    offset,
                    loaded = all_positions.len(),
                    "Fallback pagination hard limit reached"
                );
                break;
            }
        }

        tracing::info!(
            wallet = %self.wallet,
            positions = all_positions.len(),
            "Positions loaded from Data API (fallback)"
        );

        Ok(all_positions)
    }

    /// Find positions that are redeemable (market resolved).
    ///
    /// Uses the `redeemable=true` API filter to only fetch resolved positions.
    ///
    /// Falls back to raw HTTP requests if SDK deserialization fails.
    pub async fn find_redeemable(&self) -> Result<Vec<RedeemablePosition>> {
        match self.try_find_redeemable_sdk().await {
            Ok(positions) => Ok(positions),
            Err(e) => {
                let error_chain = format!("{:#}", e).to_lowercase();
                if error_chain.contains("deserialization")
                    || error_chain.contains("premature end")
                    || error_chain.contains("invalid type")
                    || error_chain.contains("enddate")
                {
                    tracing::info!(
                        error = %e,
                        wallet = %self.wallet,
                        "Data API SDK redeemable path hit known empty endDate bug, using raw HTTP fallback"
                    );
                    self.find_redeemable_fallback().await
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Try to find redeemable positions using the SDK client.
    async fn try_find_redeemable_sdk(&self) -> Result<Vec<RedeemablePosition>> {
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

            let page = self
                .client
                .positions(&req)
                .await
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
                        outcome_index: pos.outcome_index,
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

    /// Fallback: Find redeemable positions using raw HTTP requests.
    async fn find_redeemable_fallback(&self) -> Result<Vec<RedeemablePosition>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .context("Failed to create HTTP client for fallback")?;

        let mut redeemable = Vec::new();
        let mut offset = 0i32;
        let limit = 500i32;

        loop {
            let url = format!(
                "{}/positions?user={}&redeemable=true&sizeThreshold=0&limit={}&offset={}",
                self.api_base, self.wallet, limit, offset
            );

            let resp = client
                .get(&url)
                .send()
                .await
                .context("Fallback redeemable HTTP request failed")?;

            if !resp.status().is_success() {
                return Err(anyhow::anyhow!(
                    "Fallback redeemable request failed with status {}",
                    resp.status()
                ));
            }

            let raw: RawPositionsResponse = resp
                .json()
                .await
                .context("Fallback redeemable JSON parsing failed")?;

            let page_len = raw.positions.len();

            for pos in raw.positions {
                if pos.size > Decimal::ZERO {
                    redeemable.push(RedeemablePosition {
                        condition_id: pos.condition_id,
                        token_id: pos.asset,
                        size: pos.size,
                        title: pos.title,
                        neg_risk: pos.negative_risk,
                        outcome_index: pos.outcome_index,
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
