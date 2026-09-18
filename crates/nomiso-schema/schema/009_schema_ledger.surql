-- Migration ledger (MIG-002): one durable row per applied migration.
-- Claimed with status 'applying', completed with 'applied'; a claimed but
-- stale row can be stolen by a later migrator (all shipped DDL is idempotent).
DEFINE TABLE IF NOT EXISTS schema_migration SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS checksum ON schema_migration TYPE string;
DEFINE FIELD IF NOT EXISTS status ON schema_migration TYPE string;
DEFINE FIELD IF NOT EXISTS started_at ON schema_migration TYPE datetime;
DEFINE FIELD IF NOT EXISTS applied_at ON schema_migration TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS error ON schema_migration TYPE option<string>;
