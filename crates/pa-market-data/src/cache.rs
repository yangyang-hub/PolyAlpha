use alloy::primitives::U256;
use dashmap::DashMap;
use pa_core::types::OrderBook;
use std::sync::Arc;

/// Thread-safe in-memory cache for order books, keyed by token_id.
#[derive(Debug, Clone)]
pub struct OrderBookCache {
    books: Arc<DashMap<U256, OrderBook>>,
}

impl OrderBookCache {
    pub fn new() -> Self {
        Self {
            books: Arc::new(DashMap::new()),
        }
    }

    /// Insert or update an order book snapshot.
    pub fn update(&self, token_id: U256, book: OrderBook) {
        self.books.insert(token_id, book);
    }

    /// Get a clone of the order book for a given token.
    pub fn get(&self, token_id: &U256) -> Option<OrderBook> {
        self.books.get(token_id).map(|r| r.value().clone())
    }

    /// Remove a token's order book from cache.
    pub fn remove(&self, token_id: &U256) {
        self.books.remove(token_id);
    }

    /// Number of cached order books.
    pub fn len(&self) -> usize {
        self.books.len()
    }

    pub fn is_empty(&self) -> bool {
        self.books.is_empty()
    }

    /// Get all cached token IDs.
    pub fn token_ids(&self) -> Vec<U256> {
        self.books.iter().map(|r| *r.key()).collect()
    }
}

impl Default for OrderBookCache {
    fn default() -> Self {
        Self::new()
    }
}
