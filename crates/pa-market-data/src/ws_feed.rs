use alloy::primitives::U256;
use chrono::{DateTime, TimeZone, Utc};
use futures::StreamExt;
use pa_core::config::Settings;
use pa_core::types::{OrderBook, PriceLevel};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::cache::OrderBookCache;

/// Event sent via broadcast channel when an order book is updated.
#[derive(Debug, Clone)]
pub struct OrderBookUpdate {
    pub token_id: U256,
    pub timestamp: DateTime<Utc>,
}

/// Manages WebSocket connections to Polymarket's CLOB for real-time order book data.
pub struct WebSocketFeed {
    ws_client: polymarket_client_sdk::clob::ws::Client,
    cache: Arc<OrderBookCache>,
    max_instruments: usize,
    update_tx: broadcast::Sender<OrderBookUpdate>,
    cancel_token: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
    ws_connected: Arc<AtomicBool>,
}

impl WebSocketFeed {
    pub fn new(settings: &Settings, cache: Arc<OrderBookCache>) -> anyhow::Result<Self> {
        let ws_client = polymarket_client_sdk::clob::ws::Client::new(
            &settings.clob.ws_host,
            polymarket_client_sdk::ws::config::Config::default(),
        )?;

        let (update_tx, _) = broadcast::channel(4096);

        Ok(Self {
            ws_client,
            cache,
            max_instruments: settings.market_filter.ws_max_instruments,
            update_tx,
            cancel_token: CancellationToken::new(),
            tasks: Vec::new(),
            ws_connected: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Create with default WS endpoint.
    pub fn with_defaults(settings: &Settings, cache: Arc<OrderBookCache>) -> Self {
        let (update_tx, _) = broadcast::channel(4096);

        Self {
            ws_client: polymarket_client_sdk::clob::ws::Client::default(),
            cache,
            max_instruments: settings.market_filter.ws_max_instruments,
            update_tx,
            cancel_token: CancellationToken::new(),
            tasks: Vec::new(),
            ws_connected: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Get a receiver for order book update notifications.
    pub fn subscribe_updates(&self) -> broadcast::Receiver<OrderBookUpdate> {
        self.update_tx.subscribe()
    }

    /// Get the WebSocket connected status flag.
    pub fn ws_connected(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.ws_connected)
    }

    /// Subscribe to order book updates for the given token IDs.
    ///
    /// Uses `polymarket_client_sdk::clob::ws::Client::subscribe_orderbook()`.
    /// **Important**: Max 500 instruments per WebSocket connection.
    pub fn subscribe(&mut self, token_ids: &[U256]) -> anyhow::Result<()> {
        if token_ids.is_empty() {
            return Ok(());
        }

        let subscribe_ids: Vec<U256> = if token_ids.len() > self.max_instruments {
            tracing::warn!(
                requested = token_ids.len(),
                max = self.max_instruments,
                "Subscription exceeds max instruments, truncating"
            );
            token_ids[..self.max_instruments].to_vec()
        } else {
            token_ids.to_vec()
        };

        tracing::info!(count = subscribe_ids.len(), "Subscribing to order book updates");

        // Clone the WS client (Arc-backed, cheap) so we can move it into the spawned task.
        // subscribe_orderbook returns a stream that borrows from the client, so both
        // the client and stream must live inside the 'static task.
        let ws_client = self.ws_client.clone();
        let cache = Arc::clone(&self.cache);
        let update_tx = self.update_tx.clone();
        let cancel = self.cancel_token.clone();
        let ws_connected = Arc::clone(&self.ws_connected);

        let handle = tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            let max_backoff = Duration::from_secs(60);
            let mut total_messages: u64 = 0;

            loop {
                let stream = match ws_client.subscribe_orderbook(subscribe_ids.clone()) {
                    Ok(s) => {
                        backoff = Duration::from_secs(1); // Reset on success
                        // NOTE: subscribe_orderbook returning Ok means the stream was created,
                        // NOT that the underlying TCP/WS connection is established. The SDK
                        // connects lazily in a background task.
                        tracing::info!("WebSocket stream created (awaiting first message for data confirmation)");
                        s
                    }
                    Err(e) => {
                        ws_connected.store(false, Ordering::Relaxed);
                        tracing::error!(error = %e, backoff_ms = backoff.as_millis(), "Failed to subscribe, retrying");
                        pa_monitor::metrics::WS_RECONNECT_COUNT.inc();
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            _ = tokio::time::sleep(backoff) => {}
                        }
                        backoff = (backoff * 2).min(max_backoff);
                        continue;
                    }
                };
                let mut stream = Box::pin(stream);
                let mut awaiting_first = true;

                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            tracing::info!("WebSocket feed task cancelled");
                            ws_connected.store(false, Ordering::Relaxed);
                            return;
                        }
                        _ = tokio::time::sleep(Duration::from_secs(15)), if awaiting_first => {
                            tracing::warn!(
                                "WebSocket: no data received after 15s — SDK may be failing to \
                                 connect to the server. Check network/DNS and SDK debug logs."
                            );
                            awaiting_first = false;
                        }
                        item = stream.next() => {
                            awaiting_first = false;
                            match item {
                                Some(Ok(book_update)) => {
                                    total_messages += 1;
                                    if total_messages == 1 {
                                        ws_connected.store(true, Ordering::Relaxed);
                                        tracing::info!(
                                            asset_id = %book_update.asset_id,
                                            bids = book_update.bids.len(),
                                            asks = book_update.asks.len(),
                                            "WebSocket CONNECTED — first message received"
                                        );
                                    } else if total_messages % 5000 == 0 {
                                        tracing::debug!(
                                            total_messages,
                                            cache_size = cache.len(),
                                            "WebSocket message throughput"
                                        );
                                    }

                                    let token_id = book_update.asset_id;
                                    let timestamp = timestamp_ms_to_datetime(book_update.timestamp);

                                    let bids: Vec<PriceLevel> = book_update
                                        .bids
                                        .into_iter()
                                        .map(|level| PriceLevel {
                                            price: level.price,
                                            size: level.size,
                                        })
                                        .collect();

                                    let asks: Vec<PriceLevel> = book_update
                                        .asks
                                        .into_iter()
                                        .map(|level| PriceLevel {
                                            price: level.price,
                                            size: level.size,
                                        })
                                        .collect();

                                    let orderbook = OrderBook {
                                        token_id,
                                        bids,
                                        asks,
                                        timestamp,
                                    };

                                    cache.update(token_id, orderbook);

                                    let _ = update_tx.send(OrderBookUpdate {
                                        token_id,
                                        timestamp,
                                    });
                                }
                                Some(Err(e)) => {
                                    tracing::warn!(error = %e, "WebSocket stream error, reconnecting");
                                    ws_connected.store(false, Ordering::Relaxed);
                                    pa_monitor::metrics::WS_RECONNECT_COUNT.inc();
                                    break; // Break inner loop to reconnect
                                }
                                None => {
                                    tracing::warn!("WebSocket stream ended, reconnecting");
                                    ws_connected.store(false, Ordering::Relaxed);
                                    pa_monitor::metrics::WS_RECONNECT_COUNT.inc();
                                    break; // Break inner loop to reconnect
                                }
                            }
                        }
                    }
                }

                // Wait before reconnecting
                tracing::info!(backoff_ms = backoff.as_millis(), "Reconnecting WebSocket");
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(max_backoff);
            }
        });

        self.tasks.push(handle);
        Ok(())
    }

    /// Cancel existing subscriptions and subscribe to new tokens.
    ///
    /// Cancels all running WS tasks, clears state, then subscribes with new token list.
    /// Used by the periodic market refresh to update WS subscriptions when new markets appear.
    pub fn resubscribe(&mut self, token_ids: &[U256]) -> anyhow::Result<()> {
        self.cancel_token.cancel();
        self.tasks.clear();
        self.cancel_token = CancellationToken::new();
        self.subscribe(token_ids)
    }

    /// Unsubscribe from the given token IDs.
    pub fn unsubscribe(&self, token_ids: &[U256]) -> anyhow::Result<()> {
        tracing::info!(count = token_ids.len(), "Unsubscribing from order book updates");
        self.ws_client.unsubscribe_orderbook(token_ids)?;
        for id in token_ids {
            self.cache.remove(id);
        }
        Ok(())
    }

    /// Gracefully shutdown all WebSocket feed tasks.
    pub async fn shutdown(self) {
        tracing::info!("Shutting down WebSocket feed");
        self.cancel_token.cancel();
        for handle in self.tasks {
            let _ = handle.await;
        }
    }
}

/// Convert millisecond Unix timestamp to DateTime<Utc>.
fn timestamp_ms_to_datetime(ts_ms: i64) -> DateTime<Utc> {
    let secs = ts_ms / 1000;
    let nsecs = ((ts_ms % 1000) * 1_000_000) as u32;
    Utc.timestamp_opt(secs, nsecs).single().unwrap_or_else(Utc::now)
}
