# AGENTS.md — Quod Engineering & Architecture Guidelines

This repository implements **Quod**, an event-sourced multi-agent Lean 4 theorem-proving collaboration platform.

## Core Design Invariants

1. **Trusted Computing Base (`C_infra`) Minimality**:
   - External agents and arbitrary harnesses (untrusted `I`) communicate strictly via the `quod` CLI / HTTP protocol (`GapPacket` and `SealedSubmissionEnvelope`). Quod never spawns or wraps external agents.
   - Never trust worker-supplied metadata for acceptance: Stage 0 static policy checks, Stage 1 `bwrap` + cgroup v2 Lean kernel audit, and Stage 2 hygiene checks run strictly inside `C_infra` (`quod-sandbox` / `quod-hub`).
2. **PostgreSQL 16 Runtime Queries Only**:
   - `quod-store` uses `sqlx` **runtime** `sqlx::query(...)` (never compile-time `sqlx::query!` macros) so `cargo check`, `cargo build`, and `cargo test` run offline without a live PostgreSQL instance.
   - All TLS/HTTP clients (`sqlx`, `reqwest`) use `rustls` rather than OpenSSL so static `x86_64-unknown-linux-musl` builds succeed cleanly.
3. **Append-Only Event Log + Materialized State**:
   - Every FSM transition (`Epoch`, `Gap`, `Submission`, `Reduction`) emits an idempotent `EventEnvelope` to `events` in PostgreSQL alongside materialized projections.
4. **Zero-Warning Verification Standard**:
   - Before completing any change, verify:
     ```bash
     cargo test --workspace
     cargo build --workspace
     ```
   - Both must pass with **0 errors and 0 warnings**.
