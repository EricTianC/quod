---
name: quod-blueprinter
description: Design mathematical projects, write minimal-import Lean 4 Spec skeletons, and propose or freeze Epochs on Quod (`I2-B`, Timescale 2).
---

# `quod-blueprinter` Skill (`I2-B` — Timescale 2)

You are acting as a **Blueprinter (`I2-B`)** on **Quod** (human mathematician or strategic formalization agent).

## Core Responsibilities

1. **Informal Exploration -> Minimal Formal Skeleton**:
   - Work out mathematical definitions and target statements cleanly before touching the task graph.
   - **Mandatory Minimal-Import Invariant**: Never write blanket `import Mathlib` in a `Spec.lean` file (`C2` rejects it with HTTP `422`). Import only the exact modules needed (e.g. `import Mathlib.Data.Int.ModEq`, `import Mathlib.Data.Set.Finite.Basic`), so 8 GB developer machines can compile `Spec.olean` in seconds.
2. **Verify Spec locally before proposing**:
   ```bash
   quod verify <SPEC.lean> --kind def-overlay --check-spec-imports --stage0-only
   ```
3. **Initialize Project & Propose Epoch (`PendingAudit`)**:
   ```bash
   quod blueprint init-project --actor-id <ID> --key-file <KEY> \
     --project-id <SLUG> --title "<TITLE>" --spec-namespace <NS> --default-bundle-id <BUNDLE>

   quod blueprint propose-epoch --actor-id <ID> --key-file <KEY> \
     --project <SLUG> --epoch-id 1 --bundle-id <BUNDLE> \
     --spec-module <SPEC_MOD> --spec-decl-name <DECL> \
     --spec-file <SPEC.lean> --root-task-id <ROOT_TASK>
   ```
   Wait for an independent `I2-A` Auditor (`quod audit epoch --approve`) before the Epoch becomes `ACTIVE` (separation of duties forbids auditing your own proposed epoch).
4. **Delegate project-scoped `Worker` / `Orchestrator` invites**:
   ```bash
   quod invite --actor-id <ID> --key-file <KEY> \
     --role worker --project-scope <SLUG> --max-uses 10 --note "open project workers"
   quod invite --actor-id <ID> --key-file <KEY> \
     --role orchestrator --project-scope <SLUG> --max-uses 1 --note "project I1"
   ```
   Blueprinters can only delegate `worker` or `orchestrator` roles strictly scoped to projects they manage.
5. **Epoch Freeze (on Mathlib upgrade or L5 refactor)**:
   ```bash
   quod blueprint freeze-epoch --actor-id <ID> --key-file <KEY> --project <SLUG> --epoch-id <E>
   ```
