-- Indexes for hybrid search and filters.
-- HNSW DIMENSION is substituted at migrate time via {{EMBEDDING_DIM}}.

DEFINE ANALYZER IF NOT EXISTS mem_analyzer TOKENIZERS class, punct FILTERS lowercase, ascii;

DEFINE INDEX IF NOT EXISTS idx_mem_ft ON memory FIELDS content.text FULLTEXT ANALYZER mem_analyzer BM25;

DEFINE INDEX IF NOT EXISTS idx_mem_vec ON memory FIELDS embedding HNSW DIMENSION {{EMBEDDING_DIM}} DIST COSINE TYPE F32;

DEFINE INDEX IF NOT EXISTS idx_mem_scope ON memory FIELDS scope;
DEFINE INDEX IF NOT EXISTS idx_mem_cat ON memory FIELDS category;
DEFINE INDEX IF NOT EXISTS idx_mem_valid_from ON memory FIELDS valid_from;
DEFINE INDEX IF NOT EXISTS idx_mem_valid_until ON memory FIELDS valid_until;
DEFINE INDEX IF NOT EXISTS idx_mem_known ON memory FIELDS known_at;
