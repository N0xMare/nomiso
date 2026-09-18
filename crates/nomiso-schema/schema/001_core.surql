-- Nomiso core tables (MVP)
-- Applied by nomiso-schema migrate helpers.

DEFINE TABLE IF NOT EXISTS memory SCHEMAFULL;

DEFINE FIELD IF NOT EXISTS category ON memory TYPE string
  ASSERT $value IN ['episodic', 'semantic', 'identity', 'procedural', 'uncertainty', 'trace'];
DEFINE FIELD IF NOT EXISTS scope ON memory TYPE string
  ASSERT string::len($value) > 0 AND string::len($value) <= 512;

DEFINE FIELD IF NOT EXISTS content ON memory TYPE object;
DEFINE FIELD IF NOT EXISTS content.text ON memory TYPE string;
-- Prefer FLEXIBLE nested objects; 007_attrs_flexible OVERWRITEs if an older def exists.
DEFINE FIELD IF NOT EXISTS content.attrs ON memory TYPE option<object> FLEXIBLE;

DEFINE FIELD IF NOT EXISTS embedding ON memory TYPE option<array<float>>;
DEFINE FIELD IF NOT EXISTS valid_from ON memory TYPE datetime;
DEFINE FIELD IF NOT EXISTS valid_until ON memory TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS known_at ON memory TYPE datetime;
DEFINE FIELD IF NOT EXISTS confidence ON memory TYPE option<float>
  ASSERT $value = NONE OR ($value >= 0 AND $value <= 1);

DEFINE FIELD IF NOT EXISTS provenance ON memory TYPE option<object>;
DEFINE FIELD IF NOT EXISTS provenance.source ON memory TYPE option<string>;
DEFINE FIELD IF NOT EXISTS provenance.kind ON memory TYPE option<string>;
DEFINE FIELD IF NOT EXISTS provenance.span ON memory TYPE option<string>;

DEFINE FIELD IF NOT EXISTS version ON memory TYPE int ASSERT $value >= 1;
DEFINE FIELD IF NOT EXISTS supersedes ON memory TYPE option<record<memory>>;
DEFINE FIELD IF NOT EXISTS superseded_by ON memory TYPE option<record<memory>>;
DEFINE FIELD IF NOT EXISTS entity_links ON memory TYPE option<array<string>>;

DEFINE TABLE IF NOT EXISTS entity SCHEMALESS;
DEFINE FIELD IF NOT EXISTS kind ON entity TYPE option<string>;
DEFINE FIELD IF NOT EXISTS name ON entity TYPE option<string>;
DEFINE FIELD IF NOT EXISTS attrs ON entity TYPE option<object>;

DEFINE TABLE IF NOT EXISTS nomiso_meta SCHEMALESS;
DEFINE FIELD IF NOT EXISTS name ON nomiso_meta TYPE string;
DEFINE FIELD IF NOT EXISTS value ON nomiso_meta TYPE any;
