#!/usr/bin/env bash
# Print the crates.io publish order for workspace crates (dependency-first),
# or run `cargo publish --dry-run` per crate in that order.
#
#   scripts/publish-order.sh            # print the order
#   scripts/publish-order.sh --dry-run  # cargo publish --dry-run per crate
#   scripts/publish-order.sh --publish  # real publish (requires registry auth)
#
# Note: `cargo publish --dry-run` verifies packaging + metadata but still
# queries the registry index; run with CARGO_NET_OFFLINE=1 to skip the index
# check (version-existence checks will be skipped too).
set -euo pipefail
cd "$(dirname "$0")/.."

ORDER=$(cargo metadata --format-version 1 2>/dev/null | python3 -c '
import json, sys
meta = json.load(sys.stdin)
ws = set(meta["workspace_members"])
pkgs = {p["id"]: p for p in meta["packages"] if p["id"] in ws}
# Skip explicitly unpublished crates (publish=false → publish=[]).
pkgs = {i: p for i, p in pkgs.items() if p.get("publish") != []}
names = {p["name"]: i for i, p in pkgs.items()}
# dependency package ids come from the resolve graph when present
deps = {i: set() for i in pkgs}
resolve_deps = {}
for node in meta.get("resolve", {}).get("nodes", []):
    resolve_deps[node["id"]] = {
        d["pkg"] for d in node.get("deps", []) if d["pkg"] in pkgs
    }
if resolve_deps:
    deps = {i: resolve_deps.get(i, set()) for i in pkgs}
else:
    for i, p in pkgs.items():
        for dep in p["dependencies"]:
            if dep["kind"] == "normal" and dep["name"] in names:
                deps[i].add(names[dep["name"]])
# topological order (Kahn)
out = []
remaining = dict(deps)
while remaining:
    ready = sorted(i for i, d in remaining.items() if not d)
    if not ready:
        sys.exit("dependency cycle among workspace crates")
    for i in ready:
        out.append(pkgs[i]["name"])
        del remaining[i]
        for d in remaining.values():
            d.discard(i)
print(" ".join(out))
')

MODE="${1:-}"
echo "publish order: $ORDER" >&2
case "$MODE" in
  "") echo "$ORDER" | tr ' ' '\n' ;;
  --dry-run)
    for c in $ORDER; do
      echo "== cargo publish --dry-run -p $c" >&2
      cargo publish --dry-run -p "$c" --allow-dirty
    done ;;
  --publish)
    for c in $ORDER; do
      echo "== cargo publish -p $c" >&2
      cargo publish -p "$c"
      echo "published $c — sleeping for index propagation" >&2
      sleep 30
    done ;;
  *) echo "usage: $0 [--dry-run|--publish]" >&2; exit 2 ;;
esac
