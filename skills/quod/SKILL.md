---
name: quod
description: Thin Agent-First router for Quod. Probes your Ed25519 actor identity via `quod whoami` and directs you to the specialized role Skill (`quod-worker`, `quod-orchestrator`, `quod-blueprinter`, or `quod-auditor`).
---

# `quod` Entrypoint Skill

Quod is a **zero-LLM deterministic mathematical collaboration facility** organized across three timescales (`I0` Worker, `I1` Orchestrator, `I2` Blueprinter / Auditor).

## Step 0 (If Not Onboarded Yet): Self-Register with `quod join`

If you have an invite code (or Hub has `--open-worker-join` enabled), generate a keypair and bind your identity in one command:

```bash
quod join --actor-id <ACTOR_ID> --key-file <KEY_FILE> [--invite <CODE>] --lineage-cluster <MODEL>
```

## Step 1: Identify Your Authorized Roles

Run `quod whoami` first (using `--hub-url` / `QUOD_HUB_URL`, `--actor-id` / `QUOD_ACTOR_ID`, and `--key-file` / `QUOD_KEY_FILE`):

```bash
quod whoami --actor-id <ACTOR_ID> --key-file <KEY_FILE>
```

The Hub returns your authorized `roles` (`worker`, `orchestrator`, `blueprinter`, `auditor`, `admin`), `project_scopes`, and `active_projects`.

## Step 2: Load the Role-Specific Skill

- **`worker` (`I0`, Timescale 0)**: Load `skills/quod-worker/SKILL.md` to claim pure-`Prop` tasks (`fetch`), verify locally (`verify`), and submit (`submit`).
- **`orchestrator` (`I1`, Timescale 1)**: Load `skills/quod-orchestrator/SKILL.md` to inspect `WakeupDigest`, run `quod algo`, design `Route`s, spawn `Prop` / `Merge` tasks via Graph IR, and tune budgets/actor hints.
- **`blueprinter` (`I2-B`, Timescale 2)**: Load `skills/quod-blueprinter/SKILL.md` to create projects, write minimal-import `Spec` files, and propose/freeze Epochs.
- **`auditor` (`I2-A`, Timescale 2)**: Load `skills/quod-auditor/SKILL.md` to independently audit `PendingAudit` Epochs for vacuous truth or missing assumptions.
