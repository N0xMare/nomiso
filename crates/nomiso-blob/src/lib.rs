//! Content-addressed blob store for Nomiso evidence plane.
//!
//! # Backends
//! - [`FsBlobStore`]: local filesystem (default for embed/tests)
//! - [`S3BlobStore`]: S3-compatible API with **AWS SigV4** (RustFS, MinIO, AWS, R2)

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

pub mod relocate;

type HmacSha256 = Hmac<Sha256>;

/// Blob store errors.
#[derive(Debug, Error)]
pub enum BlobError {
    #[error("io: {0}")]
    Io(String),
    #[error("not found: {0}")]
    NotFound(String),
    /// Bytes at a content-addressed location do not match the address.
    #[error("integrity: {0}")]
    Integrity(String),
    #[error("http: {0}")]
    Http(String),
    #[error("invalid: {0}")]
    Invalid(String),
}

impl BlobError {
    /// Stable machine-readable code (matches nomiso_core::Error taxonomy).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "io_error",
            Self::NotFound(_) => "not_found",
            Self::Integrity(_) => "integrity",
            Self::Http(_) => "provider_unavailable",
            Self::Invalid(_) => "invalid_request",
        }
    }

    /// Message safe to surface to untrusted callers — Io/Http internals may
    /// carry paths or authorities; Integrity/NotFound keep the location id
    /// (a content address, not a secret).
    pub fn public_message(&self) -> String {
        match self {
            Self::Io(_) => "blob io failed".into(),
            Self::Http(_) => "blob backend unavailable".into(),
            _ => self.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, BlobError>;

/// Result of putting bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobPut {
    /// Lowercase hex BLAKE3 (64 chars).
    pub blake3: String,
    /// Location URI (`file://…` or `s3://bucket/key`).
    pub location: String,
}

/// Content-addressed blob store.
#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put(&self, bytes: &[u8]) -> Result<BlobPut>;
    async fn get(&self, location: &str) -> Result<Vec<u8>>;
    async fn exists(&self, location: &str) -> Result<bool>;
}

/// Compute lowercase hex BLAKE3.
pub fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

/// Local filesystem blob store: `{root}/{ab}/{cd}/{fullhash}`.
#[derive(Debug, Clone)]
pub struct FsBlobStore {
    root: PathBuf,
}

impl FsBlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path_for_hash(&self, hash: &str) -> PathBuf {
        let a = &hash[0..2];
        let b = &hash[2..4];
        self.root.join(a).join(b).join(hash)
    }

    fn location_for_hash(&self, hash: &str) -> String {
        let p = self.path_for_hash(hash);
        format!("file://{}", p.display())
    }

    /// Resolve a `location` to a path confined under `root` (OPS-002):
    /// `..` components, absolute paths outside the root, and symlinks that
    /// escape it are rejected. Canonicalization resolves symlinks, so a
    /// link pointing outside fails the containment check.
    fn confine(&self, location: &str) -> Result<PathBuf> {
        let raw = location
            .strip_prefix("file://")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(location));
        if raw
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(BlobError::Invalid(format!(
                "path escapes store root: {location}"
            )));
        }
        let root_canon = self.root.canonicalize().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BlobError::NotFound(location.into())
            } else {
                BlobError::Io(e.to_string())
            }
        })?;
        let joined = if raw.is_absolute() {
            raw
        } else {
            root_canon.join(&raw)
        };
        match joined.canonicalize() {
            Ok(canon) => {
                if canon.starts_with(&root_canon) {
                    Ok(canon)
                } else {
                    Err(BlobError::Invalid(format!(
                        "path escapes store root: {location}"
                    )))
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(BlobError::NotFound(location.into()))
            }
            Err(e) => Err(BlobError::Io(e.to_string())),
        }
    }
}

fn cas_tmp_name(hash: &str) -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{hash}.tmp.{}.{nonce}", std::process::id())
}

/// Write `bytes` to `tmp`, fsync, rename over `final_path`.
///
/// Existence of `final_path` after any race is success (content-addressed).
/// Leftover tmp is removed on failure. Parent-dir fsync is best-effort.
async fn durable_cas_install(tmp: &Path, final_path: &Path, bytes: &[u8]) -> Result<()> {
    if tokio::fs::try_exists(final_path)
        .await
        .map_err(|e| BlobError::Io(e.to_string()))?
    {
        return Ok(());
    }

    let write_rename = async {
        let mut file = tokio::fs::File::create(tmp).await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(tmp, final_path).await
    }
    .await;

    if let Err(e) = write_rename {
        let _ = tokio::fs::remove_file(tmp).await;
        let dest_ok = tokio::fs::try_exists(final_path).await.unwrap_or(false);
        if !dest_ok {
            return Err(BlobError::Io(e.to_string()));
        }
    }

    if let Some(parent) = final_path.parent() {
        if let Ok(dir) = tokio::fs::File::open(parent).await {
            let _ = dir.sync_all().await;
        }
    }
    Ok(())
}

#[async_trait]
impl BlobStore for FsBlobStore {
    async fn put(&self, bytes: &[u8]) -> Result<BlobPut> {
        let hash = blake3_hex(bytes);
        let path = self.path_for_hash(&hash);
        let location = self.location_for_hash(&hash);
        if tokio::fs::try_exists(&path)
            .await
            .map_err(|e| BlobError::Io(e.to_string()))?
        {
            return Ok(BlobPut {
                blake3: hash,
                location,
            });
        }
        let parent = path
            .parent()
            .ok_or_else(|| BlobError::Io("cas path has no parent".into()))?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| BlobError::Io(e.to_string()))?;
        let tmp = parent.join(cas_tmp_name(&hash));
        durable_cas_install(&tmp, &path, bytes).await?;
        Ok(BlobPut {
            blake3: hash,
            location,
        })
    }

    async fn get(&self, location: &str) -> Result<Vec<u8>> {
        let path = self.confine(location)?;
        let bytes = tokio::fs::read(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BlobError::NotFound(location.into())
            } else {
                BlobError::Io(e.to_string())
            }
        })?;
        // CAS locations embed the content hash as the file basename — a
        // corrupted or mis-renamed object must not be returned as the
        // addressed bytes. Non-CAS paths (arbitrary file reads) skip this.
        if let Some(expected) = path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| n.len() == 64 && n.chars().all(|c| c.is_ascii_hexdigit()))
        {
            let actual = blake3_hex(&bytes);
            if actual != expected {
                return Err(BlobError::Integrity(format!(
                    "{location}: content blake3 {actual} != address {expected}"
                )));
            }
        }
        Ok(bytes)
    }

    async fn exists(&self, location: &str) -> Result<bool> {
        match self.confine(location) {
            Ok(path) => Ok(path.exists()),
            // A path that escapes the root, or names a not-yet-written
            // object, is simply absent — not an error.
            Err(BlobError::Invalid(_)) | Err(BlobError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// S3-compatible client with AWS Signature Version 4 (path-style).
///
/// Works with **RustFS**, MinIO, AWS S3, R2 (with appropriate endpoint/region).
///
/// `Debug` is manually implemented so credentials and endpoint
/// userinfo/query components are never printed.
#[derive(Clone)]
pub struct S3BlobStore {
    endpoint: String,
    bucket: String,
    access_key: String,
    secret_key: String,
    region: String,
    /// When true, use path-style URLs `{endpoint}/{bucket}/{key}` (RustFS/MinIO).
    path_style: bool,
    client: reqwest::Client,
}

/// Strip URL userinfo and query/fragment so embedded credentials never appear in logs.
fn redacted_endpoint(endpoint: &str) -> String {
    let mut ep = endpoint
        .split(['?', '#'])
        .next()
        .unwrap_or(endpoint)
        .to_string();
    if let Some(scheme_end) = ep.find("://") {
        let rest = &ep[scheme_end + 3..];
        if let Some(at) = rest.rfind('@') {
            ep = format!("{}://[REDACTED]@{}", &ep[..scheme_end], &rest[at + 1..]);
        }
    }
    ep
}

impl std::fmt::Debug for S3BlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3BlobStore")
            .field("endpoint", &redacted_endpoint(&self.endpoint))
            .field("bucket", &self.bucket)
            .field("access_key", &"[REDACTED]")
            .field("secret_key", &"[REDACTED]")
            .field("region", &self.region)
            .field("path_style", &self.path_style)
            .finish()
    }
}

impl S3BlobStore {
    pub fn new(
        endpoint: impl Into<String>,
        bucket: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            bucket: bucket.into(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            region: "us-east-1".into(),
            path_style: true,
            client: reqwest::Client::new(),
        }
    }

    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
    }

    pub fn with_path_style(mut self, path_style: bool) -> Self {
        self.path_style = path_style;
        self
    }

    fn object_url(&self, key: &str) -> String {
        if self.path_style {
            format!("{}/{}/{}", self.endpoint, self.bucket, key)
        } else {
            // virtual-hosted: https://bucket.s3.region.amazonaws.com/key — endpoint should be base
            format!("{}/{}", self.endpoint, key)
        }
    }

    fn location_for_key(&self, key: &str) -> String {
        format!("s3://{}/{}", self.bucket, key)
    }

    fn key_from_location(&self, location: &str) -> Result<String> {
        let prefix = format!("s3://{}/", self.bucket);
        location
            .strip_prefix(&prefix)
            .map(|s| s.to_string())
            .ok_or_else(|| BlobError::Invalid(format!("location not in bucket: {location}")))
    }

    fn host_header(&self) -> String {
        self.endpoint
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .to_string()
    }

    fn amz_date() -> (String, String) {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let dt = jiff::Timestamp::from_second(secs as i64).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
        // Strip subseconds: 2026-08-11T12:00:00.123Z → date + time
        let s = dt.to_string();
        let date = s.get(0..10).unwrap_or("1970-01-01").replace('-', "");
        let after_t = s.split('T').nth(1).unwrap_or("00:00:00Z");
        let hms = after_t.split(['.', 'Z']).next().unwrap_or("00:00:00");
        let time = hms.replace(':', "");
        let amz = format!("{date}T{time}Z");
        (amz, date)
    }

    fn sign_headers(
        &self,
        method: &str,
        key: &str,
        payload: &[u8],
        content_type: Option<&str>,
    ) -> Result<Vec<(String, String)>> {
        let (amz_date, date_stamp) = Self::amz_date();
        let payload_hash = sha256_hex(payload);
        let host = self.host_header();
        let canonical_uri = if self.path_style {
            format!("/{}/{}", self.bucket, key)
        } else {
            format!("/{key}")
        };
        let canonical_query = "";
        let mut headers = vec![
            ("host".to_string(), host.clone()),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), amz_date.clone()),
        ];
        if let Some(ct) = content_type {
            headers.push(("content-type".to_string(), ct.to_string()));
        }
        headers.sort_by(|a, b| a.0.cmp(&b.0));
        let signed_headers = headers
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(";");
        let canonical_headers = headers
            .iter()
            .map(|(k, v)| format!("{k}:{}\n", v.trim()))
            .collect::<String>();
        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        );
        let credential_scope = format!("{date_stamp}/{}/s3/aws4_request", self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );
        let signing_key = aws4_signing_key(&self.secret_key, &date_stamp, &self.region, "s3")?;
        let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes())?);
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.access_key
        );
        let mut out = headers;
        out.push(("authorization".to_string(), authorization));
        Ok(out)
    }

    async fn signed_request(
        &self,
        method: reqwest::Method,
        key: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> Result<reqwest::Response> {
        let headers = self.sign_headers(method.as_str(), key, &body, content_type)?;
        let url = self.object_url(key);
        let mut req = self.client.request(method, &url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        if content_type.is_some() || !body.is_empty() {
            req = req.body(body);
        }
        req.send().await.map_err(|e| BlobError::Http(e.to_string()))
    }
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|e| BlobError::Invalid(e.to_string()))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn aws4_signing_key(secret: &str, date: &str, region: &str, service: &str) -> Result<Vec<u8>> {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes())?;
    let k_region = hmac_sha256(&k_date, region.as_bytes())?;
    let k_service = hmac_sha256(&k_region, service.as_bytes())?;
    hmac_sha256(&k_service, b"aws4_request")
}

#[async_trait]
impl BlobStore for S3BlobStore {
    async fn put(&self, bytes: &[u8]) -> Result<BlobPut> {
        let hash = blake3_hex(bytes);
        let key = format!("blobs/{}/{}/{}", &hash[0..2], &hash[2..4], hash);
        let resp = self
            .signed_request(
                reqwest::Method::PUT,
                &key,
                bytes.to_vec(),
                Some("application/octet-stream"),
            )
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(BlobError::Http(format!("PUT {status}: {body}")));
        }
        Ok(BlobPut {
            blake3: hash,
            location: self.location_for_key(&key),
        })
    }

    async fn get(&self, location: &str) -> Result<Vec<u8>> {
        let key = self.key_from_location(location)?;
        let resp = self
            .signed_request(reqwest::Method::GET, &key, Vec::new(), None)
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(BlobError::NotFound(location.into()));
        }
        if !resp.status().is_success() {
            return Err(BlobError::Http(format!("GET {}", resp.status())));
        }
        let bytes = resp
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| BlobError::Http(e.to_string()))?;
        // CAS keys end in the content hash — same guarantee as fs reads:
        // corrupted or mis-keyed objects are Integrity failures, not data.
        if let Some(expected) = key
            .rsplit('/')
            .next()
            .filter(|n| n.len() == 64 && n.chars().all(|c| c.is_ascii_hexdigit()))
        {
            let actual = blake3_hex(&bytes);
            if actual != expected {
                return Err(BlobError::Integrity(format!(
                    "{location}: content blake3 {actual} != address {expected}"
                )));
            }
        }
        Ok(bytes)
    }

    async fn exists(&self, location: &str) -> Result<bool> {
        match self.get(location).await {
            Ok(_) => Ok(true),
            Err(BlobError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn fs_roundtrip() {
        let dir = tempdir().unwrap();
        let store = FsBlobStore::new(dir.path());
        let put = store.put(b"hello nomiso evidence").await.unwrap();
        assert_eq!(put.blake3.len(), 64);
        assert!(put.location.starts_with("file://"));
        let got = store.get(&put.location).await.unwrap();
        assert_eq!(got, b"hello nomiso evidence");
        let put2 = store.put(b"hello nomiso evidence").await.unwrap();
        assert_eq!(put.blake3, put2.blake3);
        assert_eq!(put.location, put2.location);
    }

    #[tokio::test]
    async fn fs_put_layout_and_no_tmp_after_success() {
        let dir = tempdir().unwrap();
        let store = FsBlobStore::new(dir.path());
        let bytes = b"cas-layout";
        let put = store.put(bytes).await.unwrap();
        let expected = store.path_for_hash(&put.blake3);
        assert_eq!(
            expected,
            dir.path()
                .join(&put.blake3[0..2])
                .join(&put.blake3[2..4])
                .join(&put.blake3)
        );
        assert!(expected.is_file());
        let shard = expected.parent().unwrap();
        for entry in std::fs::read_dir(shard).unwrap() {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            assert!(!name.contains(".tmp."), "leftover tmp: {name}");
            assert_eq!(name.as_ref(), put.blake3);
        }
    }

    #[tokio::test]
    async fn fs_put_skips_when_final_exists() {
        let dir = tempdir().unwrap();
        let store = FsBlobStore::new(dir.path());
        let bytes = b"already-there";
        let hash = blake3_hex(bytes);
        let path = store.path_for_hash(&hash);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let put = store.put(bytes).await.unwrap();
        assert_eq!(put.blake3, hash);
        assert_eq!(store.get(&put.location).await.unwrap(), bytes);
    }

    #[tokio::test]
    async fn fs_tmp_is_not_a_cas_object() {
        let dir = tempdir().unwrap();
        let store = FsBlobStore::new(dir.path());
        let bytes = b"partial-should-not-serve";
        let hash = blake3_hex(bytes);
        let final_path = store.path_for_hash(&hash);
        let parent = final_path.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        std::fs::write(parent.join(format!("{hash}.tmp.1.2")), b"trunc").unwrap();
        let loc = store.location_for_hash(&hash);
        assert!(!store.exists(&loc).await.unwrap());
        assert!(matches!(store.get(&loc).await, Err(BlobError::NotFound(_))));
    }

    #[tokio::test]
    async fn fs_get_detects_corrupted_cas_object() {
        let dir = tempdir().unwrap();
        let store = FsBlobStore::new(dir.path());
        let put = store.put(b"integrity-checked").await.unwrap();
        // Corrupt the CAS object in place — the read must not serve it.
        let path = store.path_for_hash(&put.blake3);
        std::fs::write(&path, b"tampered").unwrap();
        match store.get(&put.location).await {
            Err(BlobError::Integrity(_)) => {}
            other => panic!("expected Integrity, got {other:?}"),
        }
        // Non-CAS paths (basename is not a 64-hex address) still read freely.
        let plain = dir.path().join("notes.txt");
        std::fs::write(&plain, b"adhoc").unwrap();
        assert_eq!(store.get(plain.to_str().unwrap()).await.unwrap(), b"adhoc");
    }

    #[tokio::test]
    async fn fs_concurrent_same_bytes() {
        let dir = tempdir().unwrap();
        let store = FsBlobStore::new(dir.path());
        let bytes: &[u8] = b"race-same-hash";
        let mut joins = Vec::new();
        for _ in 0..8 {
            let s = store.clone();
            joins.push(tokio::spawn(async move { s.put(bytes).await }));
        }
        let mut hashes = Vec::new();
        for j in joins {
            let put = j.await.unwrap().unwrap();
            hashes.push(put.blake3);
        }
        assert!(hashes.iter().all(|h| h == &hashes[0]));
        let loc = store.location_for_hash(&hashes[0]);
        assert_eq!(store.get(&loc).await.unwrap(), bytes);
        let final_path = store.path_for_hash(&hashes[0]);
        let extras: Vec<_> = std::fs::read_dir(final_path.parent().unwrap())
            .unwrap()
            .filter_map(|e| {
                let n = e.unwrap().file_name();
                let n = n.to_string_lossy().into_owned();
                if n.contains(".tmp.") {
                    Some(n)
                } else {
                    None
                }
            })
            .collect();
        assert!(extras.is_empty(), "leftover tmps: {extras:?}");
    }

    #[tokio::test]
    async fn fs_get_rejects_escape_and_symlink_out() {
        let dir = tempdir().unwrap();
        let store = FsBlobStore::new(dir.path());
        // `..` traversal and absolute outside-root paths are refused.
        let outside = tempdir().unwrap();
        let secret = outside.path().join("secret.bin");
        std::fs::write(&secret, b"not-cas").unwrap();
        let esc = format!("../{}", dir.path().join("x").display());
        assert!(matches!(store.get(&esc).await, Err(BlobError::Invalid(_))));
        assert!(matches!(
            store.get(secret.to_str().unwrap()).await,
            Err(BlobError::Invalid(_))
        ));
        // A symlink inside the root pointing outside resolves out → refused.
        let link = dir.path().join("linked");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        assert!(matches!(
            store.get(link.to_str().unwrap()).await,
            Err(BlobError::Invalid(_))
        ));
        assert!(!store.exists(link.to_str().unwrap()).await.unwrap());
        // Legitimate object still reads.
        let put = store.put(b"inside").await.unwrap();
        assert_eq!(store.get(&put.location).await.unwrap(), b"inside");
    }

    #[tokio::test]
    async fn relocate_object_fs_to_fs_verified() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();
        let src = FsBlobStore::new(src_dir.path());
        let dst = FsBlobStore::new(dst_dir.path());
        let put = src.put(b"migrate me").await.unwrap();
        let obj = relocate::relocate_object(&src, &dst, &put.location)
            .await
            .unwrap();
        assert_eq!(obj.address, put.blake3);
        assert_eq!(
            obj.dst_location,
            put.location.replace(
                src_dir.path().to_str().unwrap(),
                dst_dir.path().to_str().unwrap()
            )
        );
        assert_eq!(dst.get(&obj.dst_location).await.unwrap(), b"migrate me");
        // Idempotent re-run.
        let obj2 = relocate::relocate_object(&src, &dst, &put.location)
            .await
            .unwrap();
        assert_eq!(obj2.dst_location, obj.dst_location);
    }

    #[tokio::test]
    async fn relocate_object_refuses_non_cas_and_corrupt() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();
        let src = FsBlobStore::new(src_dir.path());
        let dst = FsBlobStore::new(dst_dir.path());
        // Non-CAS location (basename not a 64-hex address): refused.
        let plain = src_dir.path().join("notes.txt");
        std::fs::write(&plain, b"adhoc").unwrap();
        assert!(matches!(
            relocate::relocate_object(&src, &dst, plain.to_str().unwrap()).await,
            Err(BlobError::Invalid(_))
        ));
        // Corrupted source object: Integrity, nothing lands at dst.
        let put = src.put(b"will be corrupted").await.unwrap();
        let path = src.path_for_hash(&put.blake3);
        std::fs::write(&path, b"tampered").unwrap();
        assert!(matches!(
            relocate::relocate_object(&src, &dst, &put.location).await,
            Err(BlobError::Integrity(_))
        ));
        assert!(!dst
            .exists(&put.location.replace(
                src_dir.path().to_str().unwrap(),
                dst_dir.path().to_str().unwrap()
            ))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn relocate_many_reports_partial() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();
        let src = FsBlobStore::new(src_dir.path());
        let dst = FsBlobStore::new(dst_dir.path());
        let a = src.put(b"object-a").await.unwrap();
        let b = src.put(b"object-b").await.unwrap();
        let missing = src.location_for_hash(&blake3_hex(b"never-written"));
        let report =
            relocate::relocate_many(&src, &dst, &[a.location, missing.clone(), b.location]).await;
        assert_eq!(report.scanned, 3);
        assert_eq!(report.copied, 2);
        assert_eq!(report.failed, 1);
        assert_eq!(report.failures[0].0, missing);
        assert_eq!(report.relocated.len(), 2);
    }

    #[test]
    fn expected_address_parses_cas_layouts() {
        let h = blake3_hex(b"x");
        assert_eq!(
            relocate::expected_address(&format!("file:///root/ab/cd/{h}")),
            Some(h.clone())
        );
        assert_eq!(
            relocate::expected_address(&format!("s3://bucket/blobs/ab/cd/{h}")),
            Some(h.clone())
        );
        assert_eq!(relocate::expected_address("file:///etc/passwd"), None);
        assert_eq!(relocate::expected_address("s3://b/notes.txt"), None);
    }

    #[test]
    fn s3_debug_redacts_credentials() {
        let store = S3BlobStore::new(
            "https://minio-admin:minio-secret@s3.internal:9000?token=q1",
            "evidence",
            "AKIA-SENTINEL",
            "secret-SENTINEL",
        )
        .with_region("us-west-2");
        let dbg = format!("{store:?}");
        assert!(dbg.contains("[REDACTED]"), "{dbg}");
        assert!(!dbg.contains("AKIA-SENTINEL"), "{dbg}");
        assert!(!dbg.contains("secret-SENTINEL"), "{dbg}");
        assert!(!dbg.contains("minio-secret"), "{dbg}");
        assert!(!dbg.contains("token=q1"), "{dbg}");
        assert!(dbg.contains("evidence"), "{dbg}");
        assert!(dbg.contains("us-west-2"), "{dbg}");
    }

    #[test]
    fn signing_key_stable_shape() {
        let k = aws4_signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20130524",
            "us-east-1",
            "s3",
        )
        .unwrap();
        assert_eq!(k.len(), 32);
    }
}
