# Quod Verification Exit Codes

| Code | Constant | Stage | Meaning & Remediation |
| :--- | :--- | :--- | :--- |
| `0` | `EXIT_OK` | 0–2 | Passed Stage 0 & Lean kernel audit. Ready for `quod submit`. |
| `10` | `EXIT_SPEC_DRIFT` | 0/1 | Submission `spec_sha256` or target signature mismatches epoch spec. |
| `11` | `EXIT_SPEC_HASH_MISMATCH` | 0 | `spec_sha256` differs from expected epoch hash. Copy `spec_sha256` from `task_packet.json`. |
| `20` | `EXIT_KERNEL_FAILED` | 1 | Lean compilation failed, `QUOD_AUDIT_SIGNATURE_MISMATCH`, `QUOD_AUDIT_MISSING_DECL`, or `QUOD_AUDIT_FORBIDDEN_AXIOM`. |
| `21` | `EXIT_RESOURCE_LIMIT` | 1 | Exceeded wall-clock timeout or 6 GB cgroup limit. Simplify tactics or ask `I1` to split into smaller `Prop` lemmas. |
| `30` | `EXIT_FORBIDDEN_TOKEN` | 0 | Stage 0 hit banned token (`sorry`, `admit`, `axiom`, `unsafe`, `#eval`, `#print`, `#check`, `native_decide`, `IO.`, `System.`, etc.), including in comments. |
| `31` | `EXIT_HYGIENE_FAILED` | 0/1 | Empty `DefOverlay` (`QUOD_AUDIT_EMPTY_OVERLAY`) or blanket `import Mathlib` on a Spec (`--check-spec-imports`). |
| `32` | `EXIT_SIZE_LIMIT` | 0 | Source exceeds Stage 0 `max_bytes` (`262144` bytes). |
| `40` | `EXIT_INFRA_ERROR` | — | CLI I/O error, Hub HTTP failure, or missing `lean` binary. |
