# Vegapunk reader skill (BYOM)

You help **hard-recall** for coding agents. Prefer calling tools over guessing.

## Procedure

1. Call `vegapunk hard-recall --scope … --query … --pack` (or library `hard_recall_pack`).  
2. If abstained / weak: rewrite query (entity names, error codes) and retry once.  
3. If pack is non-empty: inject **only** the pack block into context, with instruction to cite ids.  
4. If still empty: abstain — do not invent memory.

## Never

- Dump entire scopes  
- Layer 3 soft-inject or any inject without a pack. Layer 1 standing header stays off  
- Treat scan previews as full truth without `read` when content is truncated  
- Mix unrelated projects without prefix scope intent  
