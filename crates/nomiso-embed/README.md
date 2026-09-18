# nomiso-embed

Pluggable `Embedder` implementations for Nomiso / Vegapunk.

- **`HashingEmbedder`** — deterministic, offline, any dimension (tests + demos). **Not** semantic quality.
- **`HttpEmbedder`** (feature `http`) — OpenAI-compatible `/v1/embeddings`. Vegapunk CLI: `embed_url` + `VEGAPUNK_EMBED_API_KEY` (or `OPENAI_API_KEY`).

Core Nomiso never depends on HTTP embedders; attach via `NomisoClient::with_embedder`. Dim must match store HNSW (`embed_dim`).
