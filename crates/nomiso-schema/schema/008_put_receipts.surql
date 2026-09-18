DEFINE FIELD IF NOT EXISTS request_identity ON idempotency_slot TYPE option<string>;
DEFINE FIELD IF NOT EXISTS receipt ON idempotency_slot TYPE option<object> FLEXIBLE;
