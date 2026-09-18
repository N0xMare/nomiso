-- Durable generic job journal (Phase: JOB-001..007).
--
-- nomiso_job rows are toolkit-owned durable representations persisted through
-- the same store boundary as canonical memory state. dedup_slot is the live
-- deduplication key while pending/leased; terminal states tombstone it with a
-- guaranteed-unique `X|<id>` so a cancelled/superseded intent can be
-- re-enqueued as a new job. FLEXIBLE marks caller/toolkit-defined JSON bodies.

DEFINE TABLE IF NOT EXISTS nomiso_job SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS scope ON nomiso_job TYPE string;
DEFINE FIELD IF NOT EXISTS kind ON nomiso_job TYPE string;
DEFINE FIELD IF NOT EXISTS state ON nomiso_job TYPE string
  ASSERT $value IN ['pending', 'leased', 'succeeded', 'failed', 'cancelled', 'superseded'];
DEFINE FIELD IF NOT EXISTS dedup_key ON nomiso_job TYPE string;
DEFINE FIELD IF NOT EXISTS dedup_slot ON nomiso_job TYPE string;
DEFINE FIELD IF NOT EXISTS inputs ON nomiso_job TYPE array
  DEFAULT [];
DEFINE FIELD IF NOT EXISTS inputs.* ON nomiso_job TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS composition ON nomiso_job TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS payload ON nomiso_job TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS budget ON nomiso_job TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS attempts ON nomiso_job TYPE int DEFAULT 0;
DEFINE FIELD IF NOT EXISTS fence ON nomiso_job TYPE int DEFAULT 0;
DEFINE FIELD IF NOT EXISTS owner ON nomiso_job TYPE option<string>;
DEFINE FIELD IF NOT EXISTS lease_until ON nomiso_job TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS not_before ON nomiso_job TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS expires_at ON nomiso_job TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS checkpoint ON nomiso_job TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS result ON nomiso_job TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS error ON nomiso_job TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS replaced_by ON nomiso_job TYPE option<string>;
DEFINE FIELD IF NOT EXISTS terminal_reason ON nomiso_job TYPE option<string>;
DEFINE FIELD IF NOT EXISTS history ON nomiso_job TYPE array DEFAULT [];
DEFINE FIELD IF NOT EXISTS history.* ON nomiso_job TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS created_at ON nomiso_job TYPE datetime;
DEFINE FIELD IF NOT EXISTS updated_at ON nomiso_job TYPE datetime;
DEFINE FIELD IF NOT EXISTS completed_at ON nomiso_job TYPE option<datetime>;

REMOVE INDEX IF EXISTS idx_nomiso_job_dedup ON nomiso_job;
DEFINE INDEX IF NOT EXISTS idx_nomiso_job_dedup ON nomiso_job FIELDS dedup_slot UNIQUE;
DEFINE INDEX IF NOT EXISTS idx_nomiso_job_claim ON nomiso_job FIELDS state, scope, created_at;
DEFINE INDEX IF NOT EXISTS idx_nomiso_job_scope ON nomiso_job FIELDS scope, state;
