-- Evidence plane (Phase 3b/3c): content-addressed artifacts + spans + derivation.

DEFINE TABLE IF NOT EXISTS artifact SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS scope ON artifact TYPE string
  ASSERT string::len($value) > 0 AND string::len($value) <= 512;
DEFINE FIELD IF NOT EXISTS blake3 ON artifact TYPE string
  ASSERT string::len($value) = 64;
DEFINE FIELD IF NOT EXISTS location ON artifact TYPE string
  ASSERT string::len($value) > 0;
DEFINE FIELD IF NOT EXISTS media_type ON artifact TYPE string;
DEFINE FIELD IF NOT EXISTS source ON artifact TYPE option<string>;
DEFINE FIELD IF NOT EXISTS trust ON artifact TYPE option<float>
  ASSERT $value = NONE OR ($value >= 0 AND $value <= 1);
DEFINE FIELD IF NOT EXISTS created_at ON artifact TYPE datetime;

-- Global blake3 UNIQUE leaked location/source/trust across scopes.
-- Dedup is per-scope content-addressing.
REMOVE INDEX IF EXISTS idx_artifact_blake3 ON artifact;
DEFINE INDEX IF NOT EXISTS idx_artifact_scope_blake3 ON artifact FIELDS scope, blake3 UNIQUE;
DEFINE INDEX IF NOT EXISTS idx_artifact_scope ON artifact FIELDS scope;

DEFINE TABLE IF NOT EXISTS span SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS artifact_id ON span TYPE record<artifact>;
DEFINE FIELD IF NOT EXISTS scope ON span TYPE string;
DEFINE FIELD IF NOT EXISTS start ON span TYPE int ASSERT $value >= 0;
DEFINE FIELD IF NOT EXISTS end ON span TYPE int ASSERT $value >= 0;
DEFINE FIELD IF NOT EXISTS unit ON span TYPE string
  ASSERT $value IN ['byte', 'line', 'char'];
DEFINE FIELD IF NOT EXISTS created_at ON span TYPE datetime;

DEFINE TABLE IF NOT EXISTS derived_from_edge TYPE RELATION FROM memory TO memory SCHEMALESS;
DEFINE TABLE IF NOT EXISTS derived_from_artifact TYPE RELATION FROM memory TO artifact SCHEMALESS;
DEFINE TABLE IF NOT EXISTS derived_from_span TYPE RELATION FROM memory TO span SCHEMALESS;

-- Optional stale flag + version stamps on memory (additive; soft SCHEMAFULL extend).
DEFINE FIELD IF NOT EXISTS stale ON memory TYPE option<bool>;
DEFINE FIELD IF NOT EXISTS extractor_version ON memory TYPE option<string>;
DEFINE FIELD IF NOT EXISTS model_version ON memory TYPE option<string>;
DEFINE FIELD IF NOT EXISTS idempotency_key ON memory TYPE option<string>;
DEFINE FIELD IF NOT EXISTS valid_rev_from ON memory TYPE option<string>;
DEFINE FIELD IF NOT EXISTS valid_rev_until ON memory TYPE option<string>;

-- Lookup aid on the memory row (NONE keys must not be UNIQUE).
DEFINE INDEX IF NOT EXISTS idx_memory_idempotency ON memory FIELDS scope, idempotency_key;

-- Sidecar slot: only created when a non-empty idempotency key is present.
-- Record id is the claim key; UNIQUE is the record primary key (no NONE).
DEFINE TABLE IF NOT EXISTS idempotency_slot SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS scope ON idempotency_slot TYPE string;
DEFINE FIELD IF NOT EXISTS k ON idempotency_slot TYPE string;
DEFINE FIELD IF NOT EXISTS memory_key ON idempotency_slot TYPE string;
