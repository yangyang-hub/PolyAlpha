use std::collections::HashMap;
use std::str::FromStr;

use alloy::primitives::{B256, U256};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use pa_core::types::{MarketInfo, OrderBook, Outcome, PriceLevel, TokenInfo};
use pa_storage::models::OrderBookSnapshotRow;
use pa_storage::repository::Repository;

/// A single time-step frame containing order book snapshots for multiple tokens.
#[derive(Debug, Clone)]
pub struct SnapshotFrame {
    pub timestamp: DateTime<Utc>,
    pub books: HashMap<U256, OrderBook>,
}

/// Loads historical data from PostgreSQL for backtest replay.
pub struct DataLoader {
    repo: Repository,
}

impl DataLoader {
    pub fn new(repo: Repository) -> Self {
        Self { repo }
    }

    /// Load order book snapshots and group them into time-ordered frames.
    ///
    /// Snapshots with the same timestamp (truncated to seconds) are grouped
    /// into a single `SnapshotFrame`.
    pub async fn load_snapshots(
        &self,
        token_ids: &[String],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<SnapshotFrame>> {
        tracing::info!(
            tokens = token_ids.len(),
            from = %from,
            to = %to,
            "Loading snapshots from database"
        );

        let rows = self.repo.load_snapshots(token_ids, from, to).await?;

        tracing::info!(rows = rows.len(), "Raw snapshot rows loaded");

        let frames = Self::group_into_frames(rows);

        tracing::info!(frames = frames.len(), "Snapshot frames created");

        Ok(frames)
    }

    /// Load all markets and their tokens from the database, converting to `MarketInfo`.
    pub async fn load_all_markets(&self) -> anyhow::Result<Vec<MarketInfo>> {
        let market_rows = self.repo.load_all_markets().await?;
        let token_rows = self.repo.load_all_tokens().await?;

        // Group tokens by condition_id
        let mut tokens_by_condition: HashMap<Vec<u8>, Vec<pa_storage::models::TokenRow>> =
            HashMap::new();
        for t in token_rows {
            tokens_by_condition
                .entry(t.condition_id.clone())
                .or_default()
                .push(t);
        }

        let mut markets = Vec::with_capacity(market_rows.len());
        for m in market_rows {
            let tokens = tokens_by_condition
                .get(&m.condition_id)
                .cloned()
                .unwrap_or_default();

            if tokens.len() != 2 {
                continue; // Skip non-binary markets
            }

            let token_infos: Vec<TokenInfo> = tokens
                .iter()
                .map(|t| TokenInfo {
                    token_id: U256::from_str(&t.token_id).unwrap_or(U256::ZERO),
                    outcome: if t.outcome == "Yes" {
                        Outcome::Yes
                    } else {
                        Outcome::No
                    },
                    complement_id: U256::from_str(&t.complement_id).unwrap_or(U256::ZERO),
                })
                .collect();

            markets.push(MarketInfo {
                condition_id: B256::from_slice(&m.condition_id),
                question_id: B256::from_slice(&m.question_id),
                question: m.question,
                neg_risk: m.neg_risk,
                neg_risk_market_id: m.neg_risk_market_id.map(|id| B256::from_slice(&id)),
                tokens: token_infos,
                tick_size: m.tick_size,
                fee_rate_bps: m.fee_rate_bps as u32,
                active: m.active,
                liquidity: Decimal::ZERO,
                event_title: None,
            });
        }

        tracing::info!(markets = markets.len(), "Markets loaded from database");
        Ok(markets)
    }

    /// Get all token IDs that have snapshots in the given time range.
    pub async fn snapshot_token_ids(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<String>> {
        self.repo.snapshot_token_ids(from, to).await
    }

    /// Group raw snapshot rows into chronologically ordered frames.
    ///
    /// Rows with the same timestamp are placed in the same frame.
    fn group_into_frames(rows: Vec<OrderBookSnapshotRow>) -> Vec<SnapshotFrame> {
        if rows.is_empty() {
            return vec![];
        }

        let mut frames: Vec<SnapshotFrame> = Vec::new();
        let mut current_ts = rows[0].timestamp;
        let mut current_books: HashMap<U256, OrderBook> = HashMap::new();

        for row in rows {
            // If timestamp changed, flush the current frame
            if row.timestamp != current_ts {
                if !current_books.is_empty() {
                    frames.push(SnapshotFrame {
                        timestamp: current_ts,
                        books: std::mem::take(&mut current_books),
                    });
                }
                current_ts = row.timestamp;
            }

            if let Some(book) = Self::parse_snapshot(&row) {
                current_books.insert(book.token_id, book);
            }
        }

        // Flush last frame
        if !current_books.is_empty() {
            frames.push(SnapshotFrame {
                timestamp: current_ts,
                books: current_books,
            });
        }

        frames
    }

    /// Parse a single snapshot row into an `OrderBook`.
    fn parse_snapshot(row: &OrderBookSnapshotRow) -> Option<OrderBook> {
        let token_id = U256::from_str(&row.token_id).ok()?;

        let bids = Self::parse_levels(&row.bids)?;
        let asks = Self::parse_levels(&row.asks)?;

        // Bids sorted descending, asks sorted ascending
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

        Some(OrderBook {
            token_id,
            bids: bid_levels,
            asks: ask_levels,
            timestamp: row.timestamp,
        })
    }

    /// Parse JSONB bid/ask levels: expected format is `[[price, size], ...]`
    fn parse_levels(json: &serde_json::Value) -> Option<Vec<(Decimal, Decimal)>> {
        let arr = json.as_array()?;
        let mut levels = Vec::with_capacity(arr.len());

        for item in arr {
            let pair = item.as_array()?;
            if pair.len() != 2 {
                continue;
            }
            let price = parse_decimal_value(&pair[0])?;
            let size = parse_decimal_value(&pair[1])?;
            levels.push((price, size));
        }

        Some(levels)
    }
}

/// Parse a JSON value into a Decimal (handles both string and number).
fn parse_decimal_value(val: &serde_json::Value) -> Option<Decimal> {
    match val {
        serde_json::Value::String(s) => Decimal::from_str(s).ok(),
        serde_json::Value::Number(n) => {
            let s = n.to_string();
            Decimal::from_str(&s).ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use serde_json::json;

    #[test]
    fn test_parse_levels_array_format() {
        let json = json!([["0.47", "100"], ["0.48", "200"]]);
        let levels = DataLoader::parse_levels(&json).unwrap();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0], (dec!(0.47), dec!(100)));
        assert_eq!(levels[1], (dec!(0.48), dec!(200)));
    }

    #[test]
    fn test_parse_levels_number_format() {
        let json = json!([[0.47, 100], [0.48, 200]]);
        let levels = DataLoader::parse_levels(&json).unwrap();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].0, dec!(0.47));
    }

    #[test]
    fn test_parse_snapshot() {
        let row = OrderBookSnapshotRow {
            id: 1,
            token_id: "12345".to_string(),
            timestamp: Utc::now(),
            bids: json!([["0.48", "200"], ["0.45", "100"]]),
            asks: json!([["0.52", "150"], ["0.55", "300"]]),
            best_bid: Some(dec!(0.48)),
            best_ask: Some(dec!(0.52)),
            midpoint: Some(dec!(0.50)),
        };

        let book = DataLoader::parse_snapshot(&row).unwrap();
        assert_eq!(book.token_id, U256::from(12345));
        assert_eq!(book.best_bid().unwrap().price, dec!(0.48));
        assert_eq!(book.best_ask().unwrap().price, dec!(0.52));
    }

    #[test]
    fn test_group_into_frames() {
        let ts1 = Utc::now();
        let ts2 = ts1 + chrono::Duration::seconds(1);

        let rows = vec![
            OrderBookSnapshotRow {
                id: 1,
                token_id: "100".to_string(),
                timestamp: ts1,
                bids: json!([["0.48", "200"]]),
                asks: json!([["0.52", "150"]]),
                best_bid: None,
                best_ask: None,
                midpoint: None,
            },
            OrderBookSnapshotRow {
                id: 2,
                token_id: "200".to_string(),
                timestamp: ts1,
                bids: json!([["0.45", "100"]]),
                asks: json!([["0.55", "300"]]),
                best_bid: None,
                best_ask: None,
                midpoint: None,
            },
            OrderBookSnapshotRow {
                id: 3,
                token_id: "100".to_string(),
                timestamp: ts2,
                bids: json!([["0.49", "250"]]),
                asks: json!([["0.51", "175"]]),
                best_bid: None,
                best_ask: None,
                midpoint: None,
            },
        ];

        let frames = DataLoader::group_into_frames(rows);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].books.len(), 2); // ts1: 2 tokens
        assert_eq!(frames[1].books.len(), 1); // ts2: 1 token
    }
}
