# Pi in this repo

Project-local only. Does **not** change `~/.pi/agent/settings.json`.

| Path | Role |
|---|---|
| [`../AGENTS.md`](../AGENTS.md) | Loaded whenever `pi` is run from this tree |
| [`skills/vegapunk/SKILL.md`](./skills/vegapunk/SKILL.md) | Layer-2 Vegapunk via bash CLI |
| *(no `extensions/`)* | No auto-inject. On purpose. |

Interactive: `cd` here and run `pi`. First time, `/trust` this folder (or `pi --approve`) so `.pi/skills` loads.

Print smoke: `just dogfood-pi` (skip-honest without `pi` or a working chat model).

Do not install `@mem0/pi-agent-plugin` or hindsight-pi against this project while measuring Vegapunk.
