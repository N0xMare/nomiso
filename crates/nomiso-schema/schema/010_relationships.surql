-- Typed relationship plane (REL-001..004).
-- Relationships are assertions: scoped, versioned, typed endpoints, registered
-- predicate, evidence, epistemic status, validity interval, producer, state.

-- Entities gain an owning scope so they can be relationship endpoints.
DEFINE FIELD IF NOT EXISTS scope ON entity TYPE option<string>
  ASSERT $value = NONE OR (string::len($value) > 0 AND string::len($value) <= 512);
DEFINE FIELD IF NOT EXISTS aliases ON entity TYPE option<array<string>>;
DEFINE FIELD IF NOT EXISTS version ON entity TYPE option<int> ASSERT $value = NONE OR $value >= 1;
DEFINE FIELD IF NOT EXISTS created_at ON entity TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS updated_at ON entity TYPE option<datetime>;

DEFINE TABLE IF NOT EXISTS relationship SCHEMAFULL;

DEFINE FIELD IF NOT EXISTS scope ON relationship TYPE string
  ASSERT string::len($value) > 0 AND string::len($value) <= 512;
DEFINE FIELD IF NOT EXISTS predicate ON relationship TYPE string;
DEFINE FIELD IF NOT EXISTS predicate_version ON relationship TYPE int ASSERT $value >= 1;
DEFINE FIELD IF NOT EXISTS subject_kind ON relationship TYPE string
  ASSERT $value IN ['memory', 'entity', 'artifact', 'span'];
DEFINE FIELD IF NOT EXISTS subject_id ON relationship TYPE string ASSERT string::len($value) > 0;
DEFINE FIELD IF NOT EXISTS object_kind ON relationship TYPE string
  ASSERT $value IN ['memory', 'entity', 'artifact', 'span'];
DEFINE FIELD IF NOT EXISTS object_id ON relationship TYPE string ASSERT string::len($value) > 0;
DEFINE FIELD IF NOT EXISTS subject_rev ON relationship TYPE option<int> ASSERT $value = NONE OR $value >= 1;
DEFINE FIELD IF NOT EXISTS object_rev ON relationship TYPE option<int> ASSERT $value = NONE OR $value >= 1;
DEFINE FIELD IF NOT EXISTS epistemic ON relationship TYPE string
  ASSERT $value IN ['reported', 'inferred', 'verified', 'disputed', 'unknown'];
DEFINE FIELD IF NOT EXISTS evidence ON relationship TYPE option<array<object>> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS state ON relationship TYPE string
  ASSERT $value IN ['active', 'closed', 'stale', 'purged'];
DEFINE FIELD IF NOT EXISTS state_reason ON relationship TYPE option<string>;
DEFINE FIELD IF NOT EXISTS valid_from ON relationship TYPE datetime;
DEFINE FIELD IF NOT EXISTS valid_until ON relationship TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS producer ON relationship TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS version ON relationship TYPE int ASSERT $value >= 1;
DEFINE FIELD IF NOT EXISTS created_at ON relationship TYPE datetime;
DEFINE FIELD IF NOT EXISTS updated_at ON relationship TYPE datetime;

-- Active-edge dedupe key: 'A|scope|pred|subject|srev|object|orev' while
-- active; 'X|<row id>' once inactive so uniqueness never blocks history.
DEFINE FIELD IF NOT EXISTS live_key ON relationship TYPE option<string>;
DEFINE INDEX IF NOT EXISTS rel_live ON relationship FIELDS live_key UNIQUE;
DEFINE INDEX IF NOT EXISTS rel_subject ON relationship FIELDS scope, subject_kind, subject_id;
DEFINE INDEX IF NOT EXISTS rel_object ON relationship FIELDS scope, object_kind, object_id;
DEFINE INDEX IF NOT EXISTS rel_predicate ON relationship FIELDS scope, predicate;

-- Append-only audit of relationship revisions (prior snapshot per change).
DEFINE TABLE IF NOT EXISTS relationship_event SCHEMALESS;
DEFINE INDEX IF NOT EXISTS rel_event_rel ON relationship_event FIELDS relationship_id;
