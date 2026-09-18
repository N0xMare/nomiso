-- Phase 3e: system clocks on beliefs + append-only belief_event journal.

DEFINE FIELD IF NOT EXISTS sys_created ON memory TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS sys_updated ON memory TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS sys_closed ON memory TYPE option<datetime>;

DEFINE TABLE IF NOT EXISTS belief_event SCHEMALESS;
DEFINE INDEX IF NOT EXISTS idx_belief_event_mem ON belief_event FIELDS memory_id;
DEFINE INDEX IF NOT EXISTS idx_belief_event_scope ON belief_event FIELDS scope;
DEFINE INDEX IF NOT EXISTS idx_belief_event_sys ON belief_event FIELDS at_sys;

-- Phase 3.5: generic working-state slot (mutable, scoped, versioned).
DEFINE TABLE IF NOT EXISTS task_state SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS scope ON task_state TYPE string
  ASSERT string::len($value) > 0 AND string::len($value) <= 512;
DEFINE FIELD IF NOT EXISTS slot ON task_state TYPE string
  ASSERT string::len($value) > 0 AND string::len($value) <= 128;
-- FLEXIBLE: product-defined JSON body (arbitrary nested keys on SCHEMAFULL table).
DEFINE FIELD IF NOT EXISTS body ON task_state TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS version ON task_state TYPE int ASSERT $value >= 1;
DEFINE FIELD IF NOT EXISTS sys_created ON task_state TYPE datetime;
DEFINE FIELD IF NOT EXISTS sys_updated ON task_state TYPE datetime;
DEFINE INDEX IF NOT EXISTS idx_task_state_scope_slot ON task_state FIELDS scope, slot UNIQUE;
