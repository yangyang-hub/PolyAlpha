//! Application composition modules used by the binary entrypoint.
//!
//! The goal of this tree is to keep `main.rs` thin and move concrete startup,
//! runtime, and operational wiring into focused modules.

pub mod accounts;
pub mod account_runtime;
pub mod bootstrap;
pub mod helpers;
pub mod liquidity_rewards;
pub mod market_runtime;
pub mod tasks;
pub mod types;
