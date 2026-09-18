-- 011: embedding model/index identity and staged reindex generations (MIG-004/005)

-- Every embedded write stamps `memory.embedding_generation` with the active
-- generation. Generation rows carry the full model identity (family, model
-- revision, dimension, normalization, encoding) plus lifecycle state so a
-- same-dimension model change can never silently mix vectors.

DEFINE TABLE IF NOT EXISTS embedding_generation SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS generation ON embedding_generation TYPE int ASSERT $value >= 1;
DEFINE FIELD IF NOT EXISTS family ON embedding_generation TYPE string;
DEFINE FIELD IF NOT EXISTS model ON embedding_generation TYPE string;
DEFINE FIELD IF NOT EXISTS dimension ON embedding_generation TYPE int ASSERT $value >= 1;
DEFINE FIELD IF NOT EXISTS normalization ON embedding_generation TYPE string
    ASSERT $value IN ['l2', 'none', 'unknown'];
DEFINE FIELD IF NOT EXISTS encoding ON embedding_generation TYPE string;
DEFINE FIELD IF NOT EXISTS limitation ON embedding_generation TYPE option<string>;
DEFINE FIELD IF NOT EXISTS status ON embedding_generation TYPE string
    ASSERT $value IN ['building', 'active', 'retired', 'failed'];
DEFINE FIELD IF NOT EXISTS frontier_captured_at ON embedding_generation TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS frontier_generation ON embedding_generation TYPE option<int>;
DEFINE FIELD IF NOT EXISTS expected_count ON embedding_generation TYPE option<int>;
DEFINE FIELD IF NOT EXISTS embedded_count ON embedding_generation TYPE int DEFAULT 0;
DEFINE FIELD IF NOT EXISTS note ON embedding_generation TYPE option<string>;
DEFINE FIELD IF NOT EXISTS created_at ON embedding_generation TYPE datetime;
DEFINE FIELD IF NOT EXISTS activated_at ON embedding_generation TYPE option<datetime>;
DEFINE INDEX IF NOT EXISTS idx_embedding_generation_gen
    ON embedding_generation FIELDS generation UNIQUE;
DEFINE INDEX IF NOT EXISTS idx_embedding_generation_status
    ON embedding_generation FIELDS status;

-- Staged vectors for a building generation. Kept separate from
-- `memory.embedding` so the active index never serves half-built vectors and
-- old generations remain restorable until explicit cleanup.
DEFINE TABLE IF NOT EXISTS embedding_vector SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS memory ON embedding_vector TYPE record<memory>;
DEFINE FIELD IF NOT EXISTS generation ON embedding_vector TYPE int ASSERT $value >= 1;
DEFINE FIELD IF NOT EXISTS vector ON embedding_vector TYPE array<float>;
DEFINE FIELD IF NOT EXISTS created_at ON embedding_vector TYPE datetime;
DEFINE INDEX IF NOT EXISTS idx_embedding_vector_unique
    ON embedding_vector FIELDS memory, generation UNIQUE;
DEFINE INDEX IF NOT EXISTS idx_embedding_vector_gen
    ON embedding_vector FIELDS generation;

-- Generation stamp on each memory row carrying a vector.
DEFINE FIELD IF NOT EXISTS embedding_generation ON memory TYPE option<int>;
