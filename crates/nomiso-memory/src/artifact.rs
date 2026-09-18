//! Content-addressed evidence: bytes then plane metadata.

use nomiso_blob::{BlobStore, FsBlobStore};
use nomiso_core::PutArtifactRequest;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Store bytes in a blob backend, then register `blake3` + location.
#[derive(Debug, Clone)]
pub struct StoreArtifactInput {
    pub scope: String,
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub source: Option<String>,
    pub trust: Option<f64>,
}

/// Resolved blob location for CLI/lib.
#[derive(Debug, Clone)]
pub struct BlobConfig {
    /// Local CAS root (`file://` layout).
    pub root: std::path::PathBuf,
}

impl BlobConfig {
    /// `NOMISO_BLOB_ROOT`, falling back to the legacy `VEGAPUNK_BLOB_ROOT`,
    /// else `./.nomiso-blobs`.
    pub fn from_env() -> Self {
        let root = std::env::var_os("NOMISO_BLOB_ROOT")
            .or_else(|| std::env::var_os("VEGAPUNK_BLOB_ROOT"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(".nomiso-blobs"));
        Self { root }
    }

    pub fn fs_store(&self) -> FsBlobStore {
        FsBlobStore::new(&self.root)
    }
}

/// Put bytes, then `put_artifact`. Never reverse.
pub async fn store_artifact(
    client: &nomiso_service::NomisoClient,
    blobs: &dyn BlobStore,
    input: StoreArtifactInput,
) -> Result<nomiso_core::ArtifactRecord> {
    if input.bytes.is_empty() {
        return Err(crate::error::Error::invalid("store_artifact: empty bytes"));
    }
    let put = blobs.put(&input.bytes).await?;
    let blake3 = put.blake3.clone();
    let location = put.location.clone();
    client
        .put_artifact(PutArtifactRequest {
            scope: input.scope,
            blake3: put.blake3,
            location: put.location,
            media_type: if input.media_type.trim().is_empty() {
                "application/octet-stream".into()
            } else {
                input.media_type
            },
            source: input.source,
            trust: input.trust,
        })
        .await
        .map_err(|e| {
            // Keep the store_error taxonomy but carry the CAS coordinates —
            // the bytes are already committed, so a retry must reuse them.
            nomiso_core::Error::Store(format!(
                "put_artifact after CAS (retry blake3={blake3} location={location}): {e}"
            ))
            .into()
        })
}

/// MCP/JSON body: bytes as standard base64.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreArtifactJson {
    pub scope: String,
    pub bytes_b64: String,
    #[serde(default = "default_media")]
    pub media_type: String,
    #[serde(default)]
    pub source: Option<String>,
}

fn default_media() -> String {
    "application/octet-stream".into()
}

impl StoreArtifactJson {
    pub fn into_input(self) -> Result<StoreArtifactInput> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(self.bytes_b64.as_bytes())
            .map_err(|e| crate::error::Error::invalid(format!("bytes_b64: {e}")))?;
        Ok(StoreArtifactInput {
            scope: self.scope,
            bytes,
            media_type: self.media_type,
            source: self.source,
            trust: None,
        })
    }
}

/// Per-artifact outcome of a relocation run (OPS-002).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ArtifactRelocation {
    /// Blob copied+verified and the row rebound to the new location.
    Relocated {
        artifact_id: String,
        from: String,
        to: String,
    },
    /// Blob copied+verified but the row's location changed under us —
    /// re-run to pick up the new source location.
    GuardRejected { artifact_id: String },
    /// Copy or read-back failed; the row was left untouched.
    Failed { artifact_id: String, error: String },
}

/// Report for [`relocate_artifacts`].
#[derive(Debug, Default, Serialize)]
pub struct ArtifactRelocationReport {
    /// Artifact rows considered.
    pub scanned: u32,
    /// Rows now pointing at the destination backend.
    pub relocated: u32,
    /// Verified copies whose row rebind guard failed (safe to retry).
    pub guard_rejected: u32,
    /// Rows left untouched (copy/verify failure).
    pub failed: u32,
    /// Per-artifact outcomes, in row order.
    pub outcomes: Vec<ArtifactRelocation>,
}

/// Relocate every artifact blob under `scope` (`None` = all scopes) from
/// `src` to `dst`, then rebind each artifact row to the proven destination
/// location. The row is rewritten only after the destination read-back
/// verifies the content address — a failed copy never orphans a row.
///
/// Partial runs are safe to re-run: rebind is guarded on the exact location
/// observed before copy, and CAS puts are idempotent.
pub async fn relocate_artifacts(
    client: &nomiso_service::NomisoClient,
    src: &dyn BlobStore,
    dst: &dyn BlobStore,
    scope: Option<&str>,
) -> Result<ArtifactRelocationReport> {
    let artifacts = client.list_artifacts(scope).await?;
    let mut report = ArtifactRelocationReport::default();
    for art in artifacts {
        report.scanned += 1;
        match nomiso_blob::relocate::relocate_object(src, dst, &art.location).await {
            Ok(obj) => {
                let rebound = client
                    .rebind_artifact_location(&art.id, &art.location, &obj.dst_location)
                    .await?;
                if rebound {
                    report.relocated += 1;
                    report.outcomes.push(ArtifactRelocation::Relocated {
                        artifact_id: art.id,
                        from: art.location,
                        to: obj.dst_location,
                    });
                } else {
                    report.guard_rejected += 1;
                    report.outcomes.push(ArtifactRelocation::GuardRejected {
                        artifact_id: art.id,
                    });
                }
            }
            Err(e) => {
                report.failed += 1;
                report.outcomes.push(ArtifactRelocation::Failed {
                    artifact_id: art.id,
                    error: e.to_string(),
                });
            }
        }
    }
    Ok(report)
}
