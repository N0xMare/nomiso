-- Nested free-form attrs on SCHEMAFULL memory (procedural trees, product metadata).
-- OVERWRITE so existing non-FLEXIBLE definitions are upgraded on migrate.

DEFINE FIELD OVERWRITE content.attrs ON memory TYPE option<object> FLEXIBLE;
