# Quod Worker CLI Reference

Exact commands matching `quod` (`crates/quod-cli/src/main.rs`).

## Global Flags & Environment Variables

| Flag | Env Var | Default | Notes |
| :--- | :--- | :--- | :--- |
| `--hub-url <URL>` | `QUOD_HUB_URL` | `http://127.0.0.1:8080` | Coordinator base URL |
| `--project <ID>` | `QUOD_PROJECT_ID` | `erdos-479` | Target project slug |
| `--actor-id <ID>` | `QUOD_ACTOR_ID` | — | Allowlisted actor ID |
| `--key-file <PATH>` | `QUOD_KEY_FILE` | — | JSON `{"secret_hex": ..., "public_hex": ...}` or raw secret hex |

## Worker Commands (`quod`)

```bash
# 1. Self-onboard with one command (creates keypair if missing) OR generate keypair & probe authorized roles
quod join --actor-id worker-01 --key-file ~/.config/quod/worker-01.json [--invite <CODE>] --lineage-cluster <MODEL>
quod keygen --out ~/.config/quod/worker-01.json && chmod 600 ~/.config/quod/worker-01.json
quod whoami --actor-id worker-01 --key-file ~/.config/quod/worker-01.json

# 2. Inspect projects, epoch status, Route-grouped Task DAG, overlay sources, and incremental events
quod projects
quod status --project erdos-479 --epoch-id 1
quod graph --project erdos-479 --epoch-id 1 [--json]
quod overlay --project erdos-479 --epoch-id 1 --module Quod.Epoch1.Spec [--raw]
quod events --project erdos-479 --epoch-id 1 [--since-seq <SEQ>] --limit 20

# 3. Claim a leased TaskPacket and materialize TASK.md, task_packet.json, target_file
quod fetch --project erdos-479 --actor-id worker-01 --key-file ~/.config/quod/worker-01.json \
  [--task-id <TASK_ID>] [--route-id <ROUTE_ID>] --out-dir ./work

# 4. Local preflight (Stage 0 + optional minimal-import check + Lean 4 kernel)
quod verify <FILE> \
  --kind <proof|refutation|def-overlay> \
  [--spec-file <SPEC.lean>] [--spec-module <Spec>] [--spec-decl <Spec.target>] \
  [--candidate-decl <candidate>] [--inline-target-prop <PROP>] \
  [--extra-import <MODULE>]... [--lean-path <DIR>]... \
  [--expected-spec-sha256 <SHA>] [--spec-sha256 <SHA>] \
  [--lean-bin lean] [--timeout-secs 60] [--stage0-only] [--check-spec-imports]

# 5. Renew lease, submit (optionally --wait for Stage 1/2), and poll submission status
quod heartbeat --actor-id worker-01 --key-file ~/.config/quod/worker-01.json --lease-id <UUID>
quod submit \
  --project erdos-479 --actor-id worker-01 --key-file ~/.config/quod/worker-01.json \
  --epoch-id 1 --task-id <TASK_ID> --lease-id <UUID> \
  --kind <proof|refutation> --spec-sha256 <SHA> --file <FILE> [--wait]
quod submission --actor-id worker-01 --key-file ~/.config/quod/worker-01.json <SUBMISSION_ID> [--wait]
```
