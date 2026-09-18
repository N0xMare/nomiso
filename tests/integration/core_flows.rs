//! Workspace-level integration notes.
//! Run crate tests via:
//!   cargo test --workspace --features "nomiso/embedded-mem,nomiso-store/embedded-mem"
//!
//! Covered by crate tests (nomiso-store / nomiso-http / nomiso-service):
//! - put/read roundtrip
//! - supersede + as_of
//! - scope isolation (exact + prefix)
//! - soft forget
//! - vector search
//! - dimension mismatch
//! - HTTP put/search + API key auth
