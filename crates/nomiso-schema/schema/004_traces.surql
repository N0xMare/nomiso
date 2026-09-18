-- Trace plane (Phase 2.5): parent handle + append-only events.
-- Not Category::Trace on memory — separate lifecycle (high volume, append-only).

DEFINE TABLE IF NOT EXISTS trace SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS scope ON trace TYPE string
  ASSERT string::len($value) > 0 AND string::len($value) <= 512;
DEFINE FIELD IF NOT EXISTS session_id ON trace TYPE option<string>;
DEFINE FIELD IF NOT EXISTS turn_id ON trace TYPE option<string>;
DEFINE FIELD IF NOT EXISTS created_at ON trace TYPE datetime;

-- SCHEMALESS payload so nested hit cards are allowed (SCHEMAFULL object rejects unknown keys).
DEFINE TABLE IF NOT EXISTS trace_event SCHEMALESS;

DEFINE INDEX IF NOT EXISTS idx_trace_scope_created ON trace FIELDS scope, created_at;
DEFINE INDEX IF NOT EXISTS idx_trace_event_trace ON trace_event FIELDS trace_id;
DEFINE INDEX IF NOT EXISTS idx_trace_event_scope ON trace_event FIELDS scope;

DEFINE FIELD IF NOT EXISTS memory_ids ON trace_event TYPE option<array<string>>;
DEFINE INDEX IF NOT EXISTS idx_trace_event_memory_ids ON trace_event FIELDS memory_ids;
