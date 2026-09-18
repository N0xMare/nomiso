# nomiso-blob

Content-addressed blob storage for Nomiso evidence (Phase 3b).

- **`FsBlobStore`** — local directory layout by BLAKE3 (dev / zero-process). Puts are tmp + `sync_all` + rename; a completed name is never a partial write.
- **`S3BlobStore`** — S3-compatible HTTP (RustFS, MinIO, AWS, R2)

The Nomiso kernel stores only `blake3` + `location` in Surreal; bytes live here. Write bytes first, then `put_artifact`.
