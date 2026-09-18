//! OpenAI-compatible HTTP embeddings (optional feature).

use std::time::{Duration, Instant};

use async_trait::async_trait;
use nomiso_core::error::{Error, Result};
use nomiso_service::{Embedder, ProviderStatus};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Readiness probes must answer quickly; never wait a full embed deadline.
const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// `/models` probe bodies are metadata, not vectors — a much smaller bound.
const READY_MAX_RESPONSE_BYTES: usize = 256 * 1024;
const FALLBACK_KEY_ENV: &str = "OPENAI_API_KEY";

/// Config for OpenAI-compatible `/v1/embeddings`.
///
/// `Debug` is manually implemented so the API key is never printed.
#[derive(Clone)]
pub struct HttpEmbedderConfig {
    /// Base URL, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    /// API key (Bearer).
    pub api_key: String,
    /// Model id.
    pub model: String,
    /// Expected dimension (validated against response length).
    pub dimension: usize,
}

impl std::fmt::Debug for HttpEmbedderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEmbedderConfig")
            .field("base_url", &"[CONFIGURED]")
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .field("dimension", &self.dimension)
            .finish()
    }
}

/// HTTP embedder via OpenAI-compatible JSON API.
pub struct HttpEmbedder {
    client: reqwest::Client,
    config: HttpEmbedderConfig,
    api_key_env: Option<String>,
    timeout: Duration,
    max_response_bytes: usize,
}

impl std::fmt::Debug for HttpEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEmbedder")
            .field("config", &self.config)
            .field("api_key_env", &self.api_key_env)
            .field("timeout", &self.timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

impl HttpEmbedder {
    /// Build client.
    pub fn new(config: HttpEmbedderConfig) -> Result<Self> {
        if config.dimension == 0 {
            return Err(Error::invalid("dimension must be > 0"));
        }
        if config.base_url.is_empty() || config.model.is_empty() {
            return Err(Error::invalid("base_url and model are required"));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| Error::store(format!("embed client build: {e}")))?;
        Ok(Self {
            client,
            config,
            api_key_env: None,
            timeout: DEFAULT_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        })
    }

    /// Resolve the bearer token from the named environment variable (falling
    /// back to `OPENAI_API_KEY`) at call time instead of using `config.api_key`.
    pub fn with_api_key_env(mut self, name: impl Into<String>) -> Self {
        self.api_key_env = Some(name.into());
        self
    }

    /// Override the per-request timeout and maximum response body size.
    pub fn with_request_limits(
        mut self,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self> {
        if timeout.is_zero() {
            return Err(Error::invalid("timeout must be > 0"));
        }
        if max_response_bytes == 0 {
            return Err(Error::invalid("max_response_bytes must be > 0"));
        }
        self.timeout = timeout;
        self.max_response_bytes = max_response_bytes;
        Ok(self)
    }

    fn resolve_api_key(&self) -> Result<String> {
        if let Some(name) = &self.api_key_env {
            for var in [name.as_str(), FALLBACK_KEY_ENV] {
                if let Ok(key) = std::env::var(var) {
                    if !key.is_empty() {
                        return Ok(key);
                    }
                }
            }
            return Err(Error::ProviderUnavailable(format!(
                "embedding API key missing: set {name:?} or {FALLBACK_KEY_ENV:?}"
            )));
        }
        if self.config.api_key.is_empty() {
            return Err(Error::ProviderUnavailable(
                "embedding API key is empty".into(),
            ));
        }
        Ok(self.config.api_key.clone())
    }
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
    index: usize,
}

/// Validate a decoded provider response into position-ordered vectors.
fn validate_embed_response(
    parsed: EmbedResponse,
    expected_len: usize,
    dimension: usize,
) -> Result<Vec<Vec<f32>>> {
    if parsed.data.len() != expected_len {
        return Err(Error::InvalidProviderResponse(format!(
            "embed returned {} vectors for {expected_len} inputs",
            parsed.data.len()
        )));
    }
    let mut out: Vec<Option<Vec<f32>>> = (0..expected_len).map(|_| None).collect();
    for item in parsed.data {
        if item.index >= expected_len {
            return Err(Error::InvalidProviderResponse(
                "embed index out of range".into(),
            ));
        }
        if out[item.index].is_some() {
            return Err(Error::InvalidProviderResponse(
                "embed duplicate index".into(),
            ));
        }
        if item.embedding.is_empty() {
            return Err(Error::InvalidProviderResponse("embed empty vector".into()));
        }
        if item.embedding.len() != dimension {
            return Err(Error::DimensionMismatch {
                expected: dimension,
                got: item.embedding.len(),
            });
        }
        if item.embedding.iter().any(|v| !v.is_finite()) {
            return Err(Error::InvalidProviderResponse(
                "embed non-finite vector value".into(),
            ));
        }
        out[item.index] = Some(item.embedding);
    }
    out.into_iter()
        .map(|v| {
            v.ok_or_else(|| Error::InvalidProviderResponse("incomplete embedding response".into()))
        })
        .collect()
}

impl HttpEmbedder {
    /// The actual request/validate path; factored out so `embed_cancellable`
    /// can race it against a caller token without recursion.
    async fn send_embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let api_key = self.resolve_api_key()?;
        let url = format!("{}/embeddings", self.config.base_url.trim_end_matches('/'));
        let body = EmbedRequest {
            model: &self.config.model,
            input: texts,
        };
        let mut resp = self
            .client
            .post(&url)
            .bearer_auth(&api_key)
            .json(&body)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    Error::DeadlineExceeded
                } else if e.is_connect() {
                    Error::ProviderUnavailable("embed provider unreachable".into())
                } else if e.is_request() {
                    Error::ProviderUnavailable("embed request build failed".into())
                } else if e.is_body() || e.is_decode() {
                    Error::ProviderUnavailable("embed response read failed".into())
                } else {
                    // Classified only — transport error strings can embed
                    // the request address/authority and must not echo.
                    Error::ProviderUnavailable("embed transport failed".into())
                }
            })?;
        if !resp.status().is_success() {
            return Err(Error::ProviderUnavailable(format!(
                "embed HTTP status {}",
                resp.status().as_u16()
            )));
        }
        if resp
            .content_length()
            .is_some_and(|len| len > self.max_response_bytes as u64)
        {
            return Err(Error::InvalidProviderResponse(format!(
                "embed response exceeds limit of {} bytes",
                self.max_response_bytes
            )));
        }
        let mut buf = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|_| Error::InvalidProviderResponse("embed body read failed".into()))?
        {
            if buf.len() + chunk.len() > self.max_response_bytes {
                return Err(Error::InvalidProviderResponse(format!(
                    "embed response exceeds limit of {} bytes",
                    self.max_response_bytes
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        let parsed: EmbedResponse = serde_json::from_slice(&buf).map_err(|_| {
            Error::InvalidProviderResponse("invalid embedding response JSON".into())
        })?;
        validate_embed_response(parsed, texts.len(), self.config.dimension)
    }
}

#[async_trait]
impl Embedder for HttpEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_cancellable(texts, None).await
    }

    async fn embed_cancellable(
        &self,
        texts: &[String],
        cancel: Option<&CancellationToken>,
    ) -> Result<Vec<Vec<f32>>> {
        let fut = self.send_embed(texts);
        match cancel {
            None => fut.await,
            Some(token) => {
                if token.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                // Dropping the reqwest future aborts the in-flight request.
                tokio::select! {
                    _ = token.cancelled() => Err(Error::Cancelled),
                    r = fut => r,
                }
            }
        }
    }

    /// OPS-001 readiness probe: `GET {base}/models` under a short deadline
    /// with a bounded body. Reports `ready: false` + safe detail on any
    /// failure (unreachable, non-2xx, missing key) rather than erroring —
    /// a probe that finds the provider down is a status, not an exception.
    async fn ready(&self) -> Result<ProviderStatus> {
        let t0 = Instant::now();
        let ms = || t0.elapsed().as_millis() as u64;
        let api_key = match self.resolve_api_key() {
            Ok(k) => k,
            Err(_) => return Ok(ProviderStatus::down("missing api key", ms())),
        };
        let url = format!("{}/models", self.config.base_url.trim_end_matches('/'));
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&api_key)
            .timeout(self.timeout.min(READY_TIMEOUT))
            .send()
            .await;
        match resp {
            Err(e) if e.is_timeout() => Ok(ProviderStatus::down("probe timeout", ms())),
            // Classified reasons only — transport errors can embed the
            // address/authority, which must not leak into caller detail.
            Err(e) if e.is_connect() => Ok(ProviderStatus::down("probe failed: connect", ms())),
            Err(_) => Ok(ProviderStatus::down("probe failed: transport", ms())),
            Ok(mut r) => {
                if !r.status().is_success() {
                    return Ok(ProviderStatus::down(
                        format!("http status {}", r.status().as_u16()),
                        ms(),
                    ));
                }
                // Drain only enough to confirm a bounded, well-formed body —
                // the listing itself is not parsed (provider shapes vary).
                let mut received = 0usize;
                loop {
                    match r.chunk().await {
                        Ok(Some(chunk)) => {
                            received += chunk.len();
                            if received > READY_MAX_RESPONSE_BYTES {
                                return Ok(ProviderStatus::down(
                                    "probe response exceeds limit",
                                    ms(),
                                ));
                            }
                        }
                        Ok(None) => return Ok(ProviderStatus::ok(ms())),
                        Err(e) => {
                            return Ok(ProviderStatus::down(
                                format!("probe body read failed: {}", e.without_url()),
                                ms(),
                            ));
                        }
                    }
                }
            }
        }
    }

    fn identity(&self) -> Option<nomiso_core::EmbeddingIdentity> {
        // MIG-004: an OpenAI-compatible model id is not an immutable weights
        // revision — record the declared model plus the limitation, honestly.
        Some(nomiso_core::EmbeddingIdentity {
            family: "openai-compatible".into(),
            model: self.config.model.clone(),
            dimension: self.config.dimension as u32,
            normalization: nomiso_core::EmbeddingNormalization::Unknown,
            encoding: "f32".into(),
            limitation: Some(
                "provider model id is not an immutable weights revision; exact cross-run equivalence is not guaranteed"
                    .into(),
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    fn config(base_url: &str) -> HttpEmbedderConfig {
        HttpEmbedderConfig {
            base_url: base_url.into(),
            api_key: "dummy-key".into(),
            model: "m".into(),
            dimension: 2,
        }
    }

    fn resp(vecs: Vec<(usize, Vec<f32>)>) -> EmbedResponse {
        EmbedResponse {
            data: vecs
                .into_iter()
                .map(|(index, embedding)| EmbedData { embedding, index })
                .collect(),
        }
    }

    /// One-shot HTTP/1.1 stub on loopback: reads the request, sends `bytes()`.
    fn stub_server(bytes: impl FnOnce() -> Vec<u8> + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let l = line.trim();
                if l.is_empty() {
                    break;
                }
                if let Some(v) = l.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            if content_length > 0 {
                let mut body = vec![0u8; content_length];
                let _ = reader.read_exact(&mut body);
            }
            let mut stream = stream;
            let _ = stream.write_all(&bytes());
            let _ = stream.flush();
        });
        format!("http://127.0.0.1:{port}")
    }

    fn http_response(status: &str, extra_headers: &str, body: &[u8]) -> Vec<u8> {
        let mut head = format!("HTTP/1.1 {status}\r\n{extra_headers}\r\n\r\n").into_bytes();
        head.extend_from_slice(body);
        head
    }

    #[test]
    fn new_rejects_empty_url_or_zero_dim() {
        assert!(HttpEmbedder::new(HttpEmbedderConfig {
            base_url: String::new(),
            api_key: "k".into(),
            model: "m".into(),
            dimension: 8,
        })
        .is_err());
        assert!(HttpEmbedder::new(HttpEmbedderConfig {
            base_url: "https://example.com/v1".into(),
            api_key: "k".into(),
            model: "m".into(),
            dimension: 0,
        })
        .is_err());
    }

    #[test]
    fn with_request_limits_rejects_zero() {
        let e = HttpEmbedder::new(config("https://example.com")).unwrap();
        assert!(e.with_request_limits(Duration::ZERO, 1024).is_err());
        let e = HttpEmbedder::new(config("https://example.com")).unwrap();
        assert!(e.with_request_limits(Duration::from_secs(1), 0).is_err());
        let e = HttpEmbedder::new(config("https://example.com")).unwrap();
        assert!(e.with_request_limits(Duration::from_millis(1), 1).is_ok());
    }

    #[test]
    fn debug_redacts_api_key() {
        let cfg = HttpEmbedderConfig {
            api_key: "sk-sentinel-secret".into(),
            ..config("https://api.example.com/v1")
        };
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("[REDACTED]"), "{dbg}");
        assert!(!dbg.contains("sk-sentinel-secret"), "{dbg}");
        let emb = HttpEmbedder::new(cfg).unwrap();
        let dbg = format!("{emb:?}");
        assert!(!dbg.contains("sk-sentinel-secret"), "{dbg}");
    }

    #[test]
    fn debug_never_prints_base_url() {
        let cfg = config("https://urluser:urlpw@api.example.com/v1?token=urltoken9");
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("[CONFIGURED]"), "{dbg}");
        for secret in ["urluser", "urlpw", "urltoken9", "api.example.com"] {
            assert!(!dbg.contains(secret), "{dbg}");
        }
        let emb = HttpEmbedder::new(cfg).unwrap();
        assert!(!format!("{emb:?}").contains("urltoken9"));
    }

    #[test]
    fn validate_rejects_wrong_count() {
        let r = resp(vec![(0, vec![0.0; 2]), (1, vec![0.0; 2])]);
        assert!(
            validate_embed_response(r, 1, 2).is_err(),
            "2 vectors for 1 input"
        );
        let r = resp(vec![(0, vec![0.0; 2])]);
        assert!(
            validate_embed_response(r, 2, 2).is_err(),
            "1 vector for 2 inputs"
        );
    }

    #[test]
    fn validate_rejects_duplicate_index() {
        let r = resp(vec![(0, vec![0.0; 2]), (0, vec![1.0; 2])]);
        assert!(validate_embed_response(r, 2, 2).is_err());
    }

    #[test]
    fn validate_rejects_index_out_of_range() {
        let r = resp(vec![(0, vec![0.0; 2]), (7, vec![1.0; 2])]);
        assert!(validate_embed_response(r, 2, 2).is_err());
    }

    #[test]
    fn validate_rejects_wrong_dimension() {
        let r = resp(vec![(0, vec![0.0; 3])]);
        let err = validate_embed_response(r, 1, 2).unwrap_err();
        assert!(
            matches!(
                err,
                Error::DimensionMismatch {
                    expected: 2,
                    got: 3
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn validate_rejects_empty_and_nonfinite_vectors() {
        let r = resp(vec![(0, vec![])]);
        assert!(validate_embed_response(r, 1, 2).is_err(), "empty vector");
        for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let r = resp(vec![(0, vec![v, 0.0])]);
            assert!(validate_embed_response(r, 1, 2).is_err(), "non-finite {v}");
        }
    }

    #[test]
    fn validate_orders_by_index() {
        let r = resp(vec![(1, vec![1.0, 1.0]), (0, vec![0.0, 0.0])]);
        let out = validate_embed_response(r, 2, 2).unwrap();
        assert_eq!(out, vec![vec![0.0, 0.0], vec![1.0, 1.0]]);
    }

    #[tokio::test]
    async fn stub_valid_response() {
        let body = serde_json::to_vec(&json!({
            "data": [{"embedding": [0.5, 0.25], "index": 0}]
        }))
        .unwrap();
        let wire = http_response(
            "200 OK",
            &format!(
                "Content-Type: application/json\r\nContent-Length: {}",
                body.len()
            ),
            &body,
        );
        let base = stub_server(move || wire);
        let out = HttpEmbedder::new(config(&base))
            .unwrap()
            .embed(&["hi".to_string()])
            .await
            .unwrap();
        assert_eq!(out, vec![vec![0.5, 0.25]]);
    }

    #[tokio::test]
    async fn stub_oversized_body_rejected_via_content_length() {
        let body = vec![b'x'; 1024];
        let wire = http_response("200 OK", &format!("Content-Length: {}", body.len()), &body);
        let base = stub_server(move || wire);
        let err = HttpEmbedder::new(config(&base))
            .unwrap()
            .with_request_limits(Duration::from_secs(5), 64)
            .unwrap()
            .embed(&["hi".to_string()])
            .await
            .expect_err("oversized body must fail");
        assert!(err.to_string().contains("exceeds limit"), "{err}");
    }

    #[tokio::test]
    async fn stub_oversized_chunked_body_rejected_while_streaming() {
        let wire = http_response(
            "200 OK",
            "Transfer-Encoding: chunked",
            b"40\r\n0123456789012345678901234567890123456789012345678901234567890123\r\n0\r\n\r\n",
        );
        let base = stub_server(move || wire);
        let err = HttpEmbedder::new(config(&base))
            .unwrap()
            .with_request_limits(Duration::from_secs(5), 32)
            .unwrap()
            .embed(&["hi".to_string()])
            .await
            .expect_err("chunked oversized body must fail");
        assert!(err.to_string().contains("exceeds limit"), "{err}");
    }

    #[tokio::test]
    async fn stub_non_2xx_does_not_echo_body() {
        let body = b"provider sentinel body secret-9x".to_vec();
        let wire = http_response(
            "500 Internal Server Error",
            &format!("Content-Length: {}", body.len()),
            &body,
        );
        let base = stub_server(move || wire);
        let err = HttpEmbedder::new(config(&base))
            .unwrap()
            .embed(&["hi".to_string()])
            .await
            .expect_err("non-2xx must fail");
        assert!(
            matches!(err, Error::ProviderUnavailable(_)),
            "non-2xx maps to provider_unavailable: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("500"), "{msg}");
        assert!(!msg.contains("secret-9x"), "{msg}");
        assert!(!msg.contains("sentinel"), "{msg}");
        assert_eq!(err.code(), "provider_unavailable");
        assert_eq!(err.public_message(), "embedding/model provider unavailable");
    }

    #[tokio::test]
    async fn stub_undecodable_body_never_echoes_secret() {
        let body = br#""provider payload sk-synthetic-secret-Z9""#.to_vec();
        let wire = http_response(
            "200 OK",
            &format!(
                "Content-Type: application/json\r\nContent-Length: {}",
                body.len()
            ),
            &body,
        );
        let base = stub_server(move || wire);
        let err = HttpEmbedder::new(config(&base))
            .unwrap()
            .embed(&["hi".to_string()])
            .await
            .expect_err("undecodable body must fail");
        assert!(matches!(err, Error::InvalidProviderResponse(_)), "{err:?}");
        let msg = err.to_string();
        assert!(!msg.contains("sk-synthetic-secret-Z9"), "{msg}");
        assert!(!err.public_message().contains("sk-synthetic"), "{err:?}");
    }

    #[tokio::test]
    async fn stub_request_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            // Hold the accepted connection open without ever responding.
            if let Ok((_s, _)) = listener.accept() {
                std::thread::sleep(Duration::from_secs(3));
            }
        });
        let err = HttpEmbedder::new(config(&format!("http://127.0.0.1:{port}")))
            .unwrap()
            .with_request_limits(Duration::from_millis(50), 8 * 1024 * 1024)
            .unwrap()
            .embed(&["hi".to_string()])
            .await
            .expect_err("stalled server must time out");
        assert!(
            matches!(err, Error::DeadlineExceeded),
            "timeout must be DeadlineExceeded: {err:?}"
        );
        assert!(!err.to_string().contains(&port.to_string()), "{err}");
    }

    /// One-shot stub that stalls `hold` before responding (cancellation /
    /// probe-timeout tests).
    fn stub_server_slow(hold: Duration, bytes: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // Read the request head so the client isn't blocked on write.
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            std::thread::sleep(hold);
            let _ = stream.write_all(&bytes);
        });
        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn ready_reports_ok_on_2xx() {
        let wire = http_response(
            "200 OK",
            "Content-Type: application/json\r\nContent-Length: 11",
            br#"{"data":[]}"#,
        );
        let base = stub_server(move || wire);
        let st = HttpEmbedder::new(config(&base))
            .unwrap()
            .ready()
            .await
            .unwrap();
        assert!(st.ready, "{st:?}");
        assert!(st.detail.is_none(), "{st:?}");
    }

    #[tokio::test]
    async fn ready_reports_down_on_non_2xx() {
        let wire = http_response("503 Service Unavailable", "Content-Length: 0", b"");
        let base = stub_server(move || wire);
        let st = HttpEmbedder::new(config(&base))
            .unwrap()
            .ready()
            .await
            .unwrap();
        assert!(!st.ready, "{st:?}");
        assert!(
            st.detail.as_deref().is_some_and(|d| d.contains("503")),
            "{st:?}"
        );
    }

    #[tokio::test]
    async fn ready_reports_down_on_unreachable() {
        // Bind then drop: the port is closed.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let st = HttpEmbedder::new(config(&format!("http://127.0.0.1:{port}")))
            .unwrap()
            .ready()
            .await
            .unwrap();
        assert!(!st.ready, "{st:?}");
        assert!(st.detail.is_some(), "{st:?}");
        // Detail is caller-safe: no URL/userinfo echo.
        let d = st.detail.unwrap();
        assert!(!d.contains(&port.to_string()), "{d}");
    }

    #[tokio::test]
    async fn ready_reports_down_on_timeout() {
        let base = stub_server_slow(Duration::from_secs(3), Vec::new());
        let st = HttpEmbedder::new(config(&base))
            .unwrap()
            .with_request_limits(Duration::from_millis(50), 8 * 1024 * 1024)
            .unwrap()
            .ready()
            .await
            .unwrap();
        assert!(!st.ready, "{st:?}");
        assert_eq!(st.detail.as_deref(), Some("probe timeout"), "{st:?}");
    }

    #[tokio::test]
    async fn ready_reports_down_on_missing_key() {
        let mut cfg = config("http://127.0.0.1:1");
        cfg.api_key.clear();
        let emb = HttpEmbedder::new(cfg).unwrap();
        let st = emb.ready().await.unwrap();
        assert!(!st.ready, "{st:?}");
        assert_eq!(st.detail.as_deref(), Some("missing api key"), "{st:?}");
    }

    #[tokio::test]
    async fn embed_cancellable_precancelled_token() {
        let token = CancellationToken::new();
        token.cancel();
        let err = HttpEmbedder::new(config("http://127.0.0.1:1"))
            .unwrap()
            .embed_cancellable(&["hi".to_string()], Some(&token))
            .await
            .expect_err("pre-cancelled token must abort");
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        assert_eq!(err.code(), "cancelled");
    }

    #[tokio::test]
    async fn embed_cancellable_aborts_in_flight() {
        // Server stalls 30s; cancel after ~50ms must return Cancelled fast —
        // proof the reqwest future is aborted, not just awaited to timeout.
        let base = stub_server_slow(Duration::from_secs(30), Vec::new());
        let token = CancellationToken::new();
        let emb = HttpEmbedder::new(config(&base))
            .unwrap()
            .with_request_limits(Duration::from_secs(30), 8 * 1024 * 1024)
            .unwrap();
        let tok2 = token.clone();
        let t0 = std::time::Instant::now();
        let handle = tokio::spawn(async move {
            emb.embed_cancellable(&["hi".to_string()], Some(&tok2))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
        let err = handle.await.unwrap().expect_err("cancel must abort");
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "cancellation was not prompt: {:?}",
            t0.elapsed()
        );
    }

    #[tokio::test]
    async fn embed_cancellable_none_matches_embed() {
        let body = serde_json::to_vec(&json!({
            "data": [{"embedding": [1.0, 2.0], "index": 0}]
        }))
        .unwrap();
        let wire = http_response(
            "200 OK",
            &format!(
                "Content-Type: application/json\r\nContent-Length: {}",
                body.len()
            ),
            &body,
        );
        let base = stub_server(move || wire);
        let emb = HttpEmbedder::new(config(&base)).unwrap();
        // Single-connection stub would break on a second call, so verify the
        // None path through a fresh server per call is equivalent behavior.
        let out = emb
            .embed_cancellable(&["hi".to_string()], None)
            .await
            .unwrap();
        assert_eq!(out, vec![vec![1.0, 2.0]]);
    }
}
