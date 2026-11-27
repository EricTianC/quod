---
name: quod-orchestrator
description: Orchestrate Quod proof search (`I1`, Timescale 1) by inspecting WakeupDigest, calling `quod algo`, organizing `Route`s and pure-`Prop` / `Merge` tasks via Graph IR, and tuning budgets and actor hints.
---

# `quod-orchestrator` Skill (`I1` — Timescale 1)

You are acting as an **Orchestrator (`I1`)** on **Quod**. You operate on **aggregated structure and telemetry** (`WakeupDigest` + `quod algo`), never raw Lean compiler error stacks.

## Core Principles

1. **You control topology & policy; `C0` enforces soundness**: Every task you spawn must be a closed `Prop`. You cannot produce false positives even if a decomposition is incomplete—only `Merge` verification proves a parent.
2. **Prevent wasted worker compute**: When spawning expensive sub-lemmas under a `Route`, also spawn a fast **`connector` task** (`P₁ → P₂ → Target`) in parallel so sufficiency is verified early before burning heavy compute.
3. **Collision-free `candidate_decl`**: Give parallel `Lemma` tasks distinct `candidate_decl` names (e.g., `proof_case_zero`, `proof_case_odd`) so downstream `Merge` tasks can import all upstream `QuodOverlay_*` modules cleanly without name clashes.
4. **Proxy capability & difficulty evaluation**: Read actor telemetry (`ema_accept_rate`, `avg_compile_ms`) and `stall_alerts` in `WakeupDigest` to tune `min_tier`, `racer_cap_k`, `preferred_lineages`, or `PatchActorHint` directly (`PatchActorHint.enabled` blocks/unblocks an actor only within this project, never globally).

## Execution Loop

1. **Read compressed state & run `quod-algo`**:
   ```bash
   quod orch digest --actor-id <ID> --key-file <KEY> --project <PROJECT> --epoch-id <E>
   quod algo voi-rank --actor-id <ID> --key-file <KEY> --project <PROJECT> --epoch-id <E>
   quod algo detect-stalls --actor-id <ID> --key-file <KEY> --project <PROJECT> --epoch-id <E>
   quod algo check-coupling --actor-id <ID> --key-file <KEY> --project <PROJECT> --epoch-id <E>
   quod algo mine-reusable --actor-id <ID> --key-file <KEY> --project <PROJECT> --epoch-id <E>
   ```
2. **Author a closed Graph IR plan (`plan.json`)**:
   Supported `ops` inside `{"ops": [...]}`:
   - `create_route`: `{ "op": "create_route", "route_id": "...", "title": "...", "rationale": "..." }`
   - `prune_route`: `{ "op": "prune_route", "route_id": "...", "reason": "..." }`
   - `spawn_task`: `{ "op": "spawn_task", "task_id": "...", "route_id": "...", "task_role": "lemma|connector|merge|refutation|aux_lemma|witness_bundle", "spec_decl_name": "...", "spec_statement": "...", "target_inline": true, "candidate_decl": "proof_...", "curated_imports": [], "depends_on": [], "refutes_task_id": null, "priority": 5.0, "expected_cost_c": 1.0, "racer_cap_k": 2, "min_tier": 1 }`
   - `link_dependency`: `{ "op": "link_dependency", "upstream_task_id": "...", "downstream_task_id": "..." }` (cycle-checked by `C1`)
   - `publish_def_overlay`: `{ "op": "publish_def_overlay", "module_name": "QuodDef_...", "code_utf8": "..." }`
   - `set_dispatch_policy`: `{ "op": "set_dispatch_policy", "config": { "algo": "weighted_voi|lineage_diversity|tier_matched|priority_queue", "exploration_rate": 0.05, "allow_cross_route_parallel": true } }`
   - `patch_node_budget`: `{ "op": "patch_node_budget", "task_id": "...", "priority": 8.0, "racer_cap_k": 3, "min_tier": 2 }`
   - `patch_actor_hint`: `{ "op": "patch_actor_hint", "actor_id": "...", "tier": 3, "enabled": true }`
   ```bash
   quod events --project <PROJECT> --epoch-id <E> --since-seq <LAST_SEQ> --limit 50
   ```
3. **Validate (`--dry-run`) and apply atomically**:
   ```bash
   quod orch apply --actor-id <ID> --key-file <KEY> --project <PROJECT> --epoch-id <E> --file plan.json --dry-run
   quod orch apply --actor-id <ID> --key-file <KEY> --project <PROJECT> --epoch-id <E> --file plan.json
   ```
