# CHECKPOINT skill (reference)

> Historical plane playbook — **not installable**. Product monolith: [`../vegapunk/SKILL.md`](../vegapunk/SKILL.md).

After a durable unit of work, decide whether anything should be stored.

## Inspired by minimal explicit memory (Tact-like)

- Prefer **not** storing ephemeral chatter.
- Store only durable preferences, decisions, constraints, and project facts.
- Use `put` for new facts; `supersede` when a prior fact is obsolete.

## Checklist

1. Did the user state a lasting preference or constraint?
2. Did we learn a project fact that will matter next session?
3. Is there already a similar fact? If yes, supersede with `prior_id`.
4. If models disagree and policy refuses a winner → `category: "uncertainty"`.

## Non-goals

- Auto-inject large memory dumps into every prompt.
- Silent overwrites.
