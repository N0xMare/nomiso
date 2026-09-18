import argparse
import json
from pathlib import Path


def sync(root: Path, write: bool) -> int:
    source = root / "evals/coding_agent/scenarios"
    destination = root / "crates/nomiso-eval/fixtures/coding_agent"
    scenarios = sorted(source.glob("*.json"))
    if not scenarios:
        raise ValueError("coding scenario corpus is empty")
    pairs = [(path, destination / path.name) for path in scenarios]
    pairs.append((root / "evals/compare/sota_plane_v1.json", root / "crates/nomiso-eval/fixtures/sota_plane_v1.json"))
    expected = {target for _, target in pairs}
    extras = set(destination.glob("*.json")) - expected
    if extras:
        raise ValueError("unexpected packaged fixtures: " + ", ".join(sorted(p.name for p in extras)))
    changed = []
    for original, target in pairs:
        if original.is_symlink() or target.is_symlink():
            raise ValueError("fixture symlinks are not supported")
        if not original.resolve().is_relative_to(root) or not target.resolve().is_relative_to(root):
            raise ValueError("fixture path escapes the repository")
        content = original.read_bytes()
        json.loads(content)
        if not target.is_file() or target.read_bytes() != content:
            if write:
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(content)
            else:
                changed.append(str(target.relative_to(root)))
    if changed:
        raise ValueError("packaged fixtures differ; run with --write: " + ", ".join(changed))
    return len(pairs)


def main() -> None:
    parser = argparse.ArgumentParser(description="Verify or refresh byte-identical packaged evaluation fixtures.")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    try:
        count = sync(root, args.write)
    except (OSError, ValueError) as error:
        parser.exit(1, f"fixture verification failed: {error}\n")
    print(f"Verified {count} byte-identical packaged fixtures")


if __name__ == "__main__":
    main()
