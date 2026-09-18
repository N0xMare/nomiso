//! Pluggable embedders for Nomiso (model-free hashing + optional HTTP).

#![forbid(unsafe_code)]

mod hashing;

pub use hashing::HashingEmbedder;

#[cfg(feature = "http")]
mod http_embed;
#[cfg(feature = "http")]
pub use http_embed::{HttpEmbedder, HttpEmbedderConfig};

pub use nomiso_service::Embedder;
