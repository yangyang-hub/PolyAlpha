use alloy::primitives::U256;
use async_trait::async_trait;
use pa_core::config::Settings;
use pa_core::traits::MarketDataFeed;
use pa_core::types::{MarketInfo, OrderBook};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::RwLock;

use crate::cache::OrderBookCache;
use crate::gamma_feed::GammaFeed;
use crate::ws_feed::WebSocketFeed;

/// Composite market data service that implements `MarketDataFeed`.
///
/// Wires together:
/// - `GammaFeed` for market discovery
/// - `WebSocketFeed` for real-time order book streaming
/// - `OrderBookCache` for in-memory order book snapshots
pub struct MarketDataService {
    gamma: GammaFeed,
    ws: RwLock<WebSocketFeed>,
    cache: Arc<OrderBookCache>,
}

impl MarketDataService {
    pub fn new(settings: &Settings) -> anyhow::Result<Self> {
        let cache = Arc::new(OrderBookCache::new());
        let gamma = GammaFeed::new(settings)?;
        let ws = WebSocketFeed::new(settings, Arc::clone(&cache))?;

        Ok(Self {
            gamma,
            ws: RwLock::new(ws),
            cache,
        })
    }

    /// Create with default SDK endpoints.
    pub fn with_defaults(settings: &Settings) -> Self {
        let cache = Arc::new(OrderBookCache::new());
        let gamma = GammaFeed::with_defaults(settings);
        let ws = WebSocketFeed::with_defaults(settings, Arc::clone(&cache));

        Self {
            gamma,
            ws: RwLock::new(ws),
            cache,
        }
    }

    /// Access the underlying order book cache directly.
    pub fn cache(&self) -> &Arc<OrderBookCache> {
        &self.cache
    }

    /// Cancel existing WS subscriptions and subscribe to new tokens.
    pub async fn resubscribe(&self, token_ids: &[U256]) -> pa_core::Result<()> {
        self.ws
            .write()
            .await
            .resubscribe(token_ids)
            .map_err(|e| pa_core::Error::MarketData(e.to_string()))
    }

    /// Access the underlying WebSocket feed for broadcast subscriptions.
    pub async fn ws_feed(&self) -> tokio::sync::RwLockReadGuard<'_, WebSocketFeed> {
        self.ws.read().await
    }

    /// Get the WebSocket connected status flag.
    pub async fn ws_feed_ws_connected(&self) -> Arc<AtomicBool> {
        self.ws.read().await.ws_connected()
    }
}

#[async_trait]
impl MarketDataFeed for MarketDataService {
    async fn subscribe(&self, token_ids: &[U256]) -> pa_core::Result<()> {
        self.ws
            .write()
            .await
            .subscribe(token_ids)
            .map_err(|e| pa_core::Error::MarketData(e.to_string()))
    }

    async fn unsubscribe(&self, token_ids: &[U256]) -> pa_core::Result<()> {
        self.ws
            .read()
            .await
            .unsubscribe(token_ids)
            .map_err(|e| pa_core::Error::MarketData(e.to_string()))
    }

    async fn get_orderbook(&self, token_id: U256) -> pa_core::Result<OrderBook> {
        self.cache
            .get(&token_id)
            .ok_or_else(|| pa_core::Error::MarketData(format!("No order book cached for token {token_id}")))
    }

    async fn discover_markets(&self) -> pa_core::Result<Vec<MarketInfo>> {
        self.gamma
            .discover_markets()
            .await
            .map_err(|e| pa_core::Error::MarketData(e.to_string()))
    }
}
