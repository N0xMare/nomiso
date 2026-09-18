//! SurrealDB repository for Nomiso memory operations.

#![forbid(unsafe_code)]

mod config;
mod mapping;
mod surreal_store;

pub use config::{SearchConfig, StoreConfig};
pub use surreal_store::SurrealMemoryStore;
