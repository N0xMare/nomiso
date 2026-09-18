//! Verified CAS relocation between `BlobStore` backends (OPS-002).
//!
//! Every copied object is proven three ways: the source read verifies its
//! content address (fs and S3 `get` both do), the destination `put` result
//! is asserted to carry the same address, and the destination is read back
//! and re-hashed. A relocation that cannot prove identity fails loud —
//! partial runs report per-object outcomes, never silent skips.

use crate::{blake3_hex, BlobError, BlobPut, BlobStore, Result};

/// Extract the expected content address from a CAS location: the trailing
/// path segment must be a 64-hex blake3 (true of both the fs
/// `{root}/ab/cd/<hash>` and S3 `blobs/ab/cd/<hash>` layouts).
pub fn expected_address(location: &str) -> Option<String> {
    let tail = location
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default();
    (tail.len() == 64 && tail.chars().all(|c| c.is_ascii_hexdigit())).then(|| tail.to_string())
}

/// One verified object copy.
#[derive(Debug, Clone)]
pub struct RelocatedObject {
    /// Proven content address (blake3 hex).
    pub address: String,
    /// Location read from the source store.
    pub src_location: String,
    /// Location written in the destination store.
    pub dst_location: String,
    /// Byte length copied.
    pub bytes: u64,
}

/// Batch report — per-object failures are data, not aborts, so a partial
/// relocation is inspectable and resumable.
#[derive(Debug, Default)]
pub struct RelocateReport {
    /// Locations considered.
    pub scanned: u32,
    /// Objects copied and proven at the destination.
    pub copied: u32,
    /// Objects that failed verification or transport; see `failures`.
    pub failed: u32,
    /// `(location, safe error)` pairs for every object that did not copy.
    pub failures: Vec<(String, String)>,
    /// Destination locations for every copied object, in input order.
    pub relocated: Vec<RelocatedObject>,
}

/// Verified copy of one content-addressed object between stores.
///
/// Fails `Invalid` when `location` carries no CAS address — relocation of
/// unverifiable paths is refused rather than trusting a name.
pub async fn relocate_object(
    src: &dyn BlobStore,
    dst: &dyn BlobStore,
    location: &str,
) -> Result<RelocatedObject> {
    let expected = expected_address(location).ok_or_else(|| {
        BlobError::Invalid(format!("not a content-addressed location: {location}"))
    })?;
    let bytes = src.get(location).await?;
    // Defense in depth: re-hash even though verified `get` already checked —
    // a backend bug must not propagate corrupt bytes to a new home.
    let actual = blake3_hex(&bytes);
    if actual != expected {
        return Err(BlobError::Integrity(format!(
            "{location}: content blake3 {actual} != address {expected}"
        )));
    }
    let put: BlobPut = dst.put(&bytes).await?;
    if put.blake3 != expected {
        return Err(BlobError::Integrity(format!(
            "destination wrote address {} != {expected}",
            put.blake3
        )));
    }
    // Read-back: the destination must serve the addressed bytes before the
    // copy counts as landed (catches write-then-corrupt and stale stores).
    let back = dst.get(&put.location).await?;
    if blake3_hex(&back) != expected {
        return Err(BlobError::Integrity(format!(
            "destination read-back mismatch at {}",
            put.location
        )));
    }
    Ok(RelocatedObject {
        address: expected,
        src_location: location.to_string(),
        dst_location: put.location,
        bytes: bytes.len() as u64,
    })
}

/// Copy every `locations` entry, accumulating outcomes. Idempotent: putting
/// content-addressed bytes over an existing object is a no-op.
pub async fn relocate_many(
    src: &dyn BlobStore,
    dst: &dyn BlobStore,
    locations: &[String],
) -> RelocateReport {
    let mut report = RelocateReport::default();
    for loc in locations {
        report.scanned += 1;
        match relocate_object(src, dst, loc).await {
            Ok(obj) => {
                report.copied += 1;
                report.relocated.push(obj);
            }
            Err(e) => {
                report.failed += 1;
                report.failures.push((loc.clone(), e.to_string()));
            }
        }
    }
    report
}
