use std::sync::atomic::{AtomicU64, Ordering};

/// Manages nonces for Polygon transactions to prevent stuck txs.
pub struct NonceManager {
    current_nonce: AtomicU64,
}

impl NonceManager {
    pub fn new(initial_nonce: u64) -> Self {
        Self {
            current_nonce: AtomicU64::new(initial_nonce),
        }
    }

    /// Get the next nonce and increment.
    pub fn next(&self) -> u64 {
        self.current_nonce.fetch_add(1, Ordering::SeqCst)
    }

    /// Reset nonce to a specific value (e.g., after querying on-chain).
    pub fn reset(&self, nonce: u64) {
        self.current_nonce.store(nonce, Ordering::SeqCst);
    }

    /// Get current nonce without incrementing.
    pub fn current(&self) -> u64 {
        self.current_nonce.load(Ordering::SeqCst)
    }
}
