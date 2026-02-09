pub mod config;
pub mod error;
pub mod traits;
pub mod types;

pub use error::Error;
pub type Result<T> = std::result::Result<T, Error>;
