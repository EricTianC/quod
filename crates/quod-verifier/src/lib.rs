//! Standalone bounded verification daemon library for Quod (`quod-verifier`).
//! Claims pending `verification_jobs`, executes Stage 0 + Stage 1 (`LocalLeanRunner` or `BwrapRunner`),
//! persists verified `.olean` overlay modules into `accepted_olean/<project_id>/epoch_<epoch_id>/`,
//! and unlocks downstream `Blocked` tasks (such as `Merge` tasks).

use std::path::PathBuf;
use std::sync::Arc;

use clap::ValueEnum;
use quod_core::{AuditVerdict, SubmissionKind};
use quod_sandbox::{
    BwrapConfig, BwrapRunner, HarnessSpec, LocalLeanRunner, LocalVerifyRequest, SandboxRequest,
    SandboxRunner, check_stage0, parse_audit_json, render_audit_harness, sha256_hex, Stage0Policy,
};
use quod_store::{BlobStore, QuodStore, VerificationOutcomeInput};
use tracing::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RunnerMode {
    Local,
    Bwrap,
}

#[derive(Debug, Clone)]
pub struct VerifierConfig {
    pub verifier_id: String,
    pub runner_mode: RunnerMode,
    pub lean_bin: PathBuf,
    pub spec_file: Option<PathBuf>,
    pub toolchain_dir: PathBuf,
    pub epoch_lean_paths: Vec<PathBuf>,
    pub accepted_olean_dir: PathBuf,
    pub timeout_secs: u64,
}

impl Default for VerifierConfig {
    fn default() -> Self {
        Self {
            verifier_id: "verifier-01".into(),
            runner_mode: RunnerMode::Local,
            lean_bin: PathBuf::from("lean"),
            spec_file: None,
            toolchain_dir: PathBuf::from("/opt/quod/lean"),
            epoch_lean_paths: vec![PathBuf::from("/opt/quod/epoch_lib")],
            accepted_olean_dir: PathBuf::from("/var/lib/quod/accepted_olean"),
            timeout_secs: 60,
        }
    }
}

/// Claims and processes at most one pending `VerificationJob`. Returns `Ok(true)` if a job ran.
pub async fn process_one_job(
    store: &Arc<dyn QuodStore>,
    blobs: &Arc<dyn BlobStore>,
    cfg: &VerifierConfig,
) -> anyhow::Result<bool> {
    let Some(job) = store.claim_next_verification_job(&cfg.verifier_id).await? else {
        return Ok(false);
    };

    let bytes = blobs.get_bytes(&job.blob_sha256)?;
    let code = String::from_utf8(bytes)?;

    let bundle = store.get_bundle(&job.bundle_id).await.ok().flatten();
    let epoch_accepted_dir = cfg
        .accepted_olean_dir
        .join(&job.project_id)
        .join(format!("epoch_{}", job.epoch_id));
    std::fs::create_dir_all(&epoch_accepted_dir)?;

    let mut lean_paths: Vec<PathBuf> = Vec::new();
    if let Some(b) = &bundle {
        for p in &b.lean_paths {
            let pb = PathBuf::from(p);
            if pb.exists() && !lean_paths.contains(&pb) {
                lean_paths.push(pb);
            }
        }
    }
    for p in &cfg.epoch_lean_paths {
        if p.exists() && !lean_paths.contains(p) {
            lean_paths.push(p.clone());
        }
    }
    if epoch_accepted_dir.exists() {
        lean_paths.push(epoch_accepted_dir.clone());
    }

    let toolchain_dir = bundle
        .as_ref()
        .map(|b| PathBuf::from(&b.toolchain_dir))
        .filter(|p| p.exists())
        .unwrap_or_else(|| cfg.toolchain_dir.clone());

    let extra_imports: Vec<String> = job
        .curated_imports
        .iter()
        .filter(|m| *m != &job.spec_module && *m != &job.candidate_module)
        .cloned()
        .collect();

    let persist_olean_dir = match job.kind {
        SubmissionKind::Proof | SubmissionKind::DefOverlay => Some(epoch_accepted_dir.clone()),
        SubmissionKind::Refutation => None,
    };

    let verdict = match cfg.runner_mode {
        RunnerMode::Local => {
            let runner = LocalLeanRunner::new(&cfg.lean_bin);
            runner
                .verify(&LocalVerifyRequest {
                    task_id: job.task_id.clone(),
                    kind: job.kind,
                    candidate_code: code,
                    candidate_module: job.candidate_module.clone(),
                    candidate_decl: job.candidate_decl.clone(),
                    spec_file: cfg.spec_file.clone(),
                    spec_module: job.spec_module.clone(),
                    spec_decl: job.spec_decl.clone(),
                    inline_target_prop: if job.target_inline {
                        Some(job.spec_statement.clone())
                    } else {
                        None
                    },
                    extra_imports,
                    lean_paths,
                    expected_spec_sha256: None,
                    actual_spec_sha256: None,
                    timeout_secs: cfg.timeout_secs,
                    stage0_only: false,
                    persist_olean_dir,
                })
                .await?
        }
        RunnerMode::Bwrap => {
            run_bwrap_job(
                cfg,
                toolchain_dir,
                lean_paths,
                epoch_accepted_dir,
                &job.task_id,
                job.kind,
                &code,
                &job.candidate_module,
                &job.candidate_decl,
                &job.spec_module,
                &job.spec_decl,
                if job.target_inline {
                    Some(job.spec_statement.clone())
                } else {
                    None
                },
                extra_imports,
            )
            .await?
        }
    };

    info!(
        job_id = %job.job_id,
        submission_id = %job.submission_id,
        project_id = %job.project_id,
        task_id = %job.task_id,
        ok = verdict.ok,
        exit_code = verdict.exit_code,
        compile_ms = verdict.compile_ms,
        "verified submission"
    );

    let result = store
        .record_verification_outcome(VerificationOutcomeInput {
            job_id: job.job_id,
            submission_id: job.submission_id,
            verdict,
        })
        .await?;

    if !result.unlocked_tasks.is_empty() {
        info!(unlocked = ?result.unlocked_tasks, "unlocked downstream dependent tasks");
    }
    if result.project_proved {
        info!(project_id = %job.project_id, "project root goal PROVED");
    }

    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn run_bwrap_job(
    cfg: &VerifierConfig,
    toolchain_dir: PathBuf,
    epoch_lean_paths: Vec<PathBuf>,
    epoch_accepted_dir: PathBuf,
    task_id: &str,
    kind: SubmissionKind,
    code: &str,
    candidate_module: &str,
    candidate_decl: &str,
    spec_module: &str,
    spec_decl: &str,
    inline_target_prop: Option<String>,
    extra_imports: Vec<String>,
) -> anyhow::Result<AuditVerdict> {
    let stage0 = check_stage0(code, None, &Stage0Policy::default());
    if !stage0.passed {
        return Ok(AuditVerdict {
            ok: false,
            exit_code: stage0.exit_code,
            kind,
            code_sha256: stage0.code_sha256,
            compile_ms: 0,
            diagnostics: stage0
                .violations
                .into_iter()
                .map(|v| format!("line {}: {}", v.line, v.detail))
                .collect(),
        });
    }

    let tmp = tempfile::tempdir()?;
    let sub_file = tmp.path().join("submission.lean");
    let harness_file = tmp.path().join("AuditHarness.lean");
    std::fs::write(&sub_file, code)?;
    let harness_src = render_audit_harness(&HarnessSpec {
        kind,
        spec_module: if kind == SubmissionKind::DefOverlay {
            String::new()
        } else {
            spec_module.to_string()
        },
        spec_decl: spec_decl.to_string(),
        inline_target_prop,
        extra_imports,
        candidate_module: candidate_module.to_string(),
        candidate_decl: candidate_decl.to_string(),
    });
    std::fs::write(&harness_file, harness_src)?;

    let runner = BwrapRunner::new(BwrapConfig {
        toolchain_dir,
        epoch_lean_paths,
        accepted_olean_dir: Some(epoch_accepted_dir),
        harness_path: harness_file,
        ..Default::default()
    });
    let outcome = runner
        .run(SandboxRequest {
            task_id: task_id.to_string(),
            candidate_module: candidate_module.to_string(),
            submission_file: sub_file,
            timeout_secs: cfg.timeout_secs,
        })
        .await?;

    let ok = outcome.exit_code == 0 && parse_audit_json(&outcome.stdout);
    let mut diagnostics = Vec::new();
    if !ok {
        for l in outcome.stdout.lines().chain(outcome.stderr.lines()) {
            let t = l.trim();
            if !t.is_empty() {
                diagnostics.push(t.to_string());
            }
        }
        warn!(task_id, exit_code = outcome.exit_code, "bwrap verification failed");
    }

    Ok(AuditVerdict {
        ok,
        exit_code: outcome.exit_code,
        kind,
        code_sha256: sha256_hex(code.as_bytes()),
        compile_ms: outcome.compile_ms,
        diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quod_core::{
        ApplyIrRequest, ClaimLeaseRequest, CreateEpochRequest, CreateProjectRequest, GraphIrOp,
        ProjectStatus, RegisterActorRequest, RegisterBundleRequest, Role, SubmitRequest,
        TaskRole, generate_keypair_hex,
    };
    use quod_store::{LocalFsBlobStore, MemoryStore, NewSubmissionInput};
    use uuid::Uuid;

    #[tokio::test]
    async fn process_jobs_end_to_end_def_overlay_lemmas_and_merge() {
        let store_concrete = MemoryStore::new();
        let store: Arc<dyn QuodStore> = Arc::new(store_concrete);
        let blob_tmp = tempfile::tempdir().unwrap();
        let blobs: Arc<dyn BlobStore> = Arc::new(LocalFsBlobStore::new(blob_tmp.path()));

        let (_, pk_w) = generate_keypair_hex();
        store
            .register_actor(&RegisterActorRequest {
                actor_id: "w1".into(),
                pubkey_ed25519: pk_w,
                roles: vec![Role::Worker, Role::Orchestrator, Role::Blueprinter, Role::Admin],
                project_scopes: vec![],
                lineage_cluster: "test".into(),
                initial_tier: Some(3),
                skill_tags: vec![],
                display_name: None,
                git_email: None,
            })
            .await
            .unwrap();

        store
            .register_bundle(&RegisterBundleRequest {
                bundle_id: "lean-4.36.0".into(),
                lean_toolchain: "leanprover/lean4:v4.36.0".into(),
                mathlib_rev: "none".into(),
                toolchain_dir: String::new(),
                lean_paths: vec![],
                bundle_sha256: "b0".into(),
            })
            .await
            .unwrap();
        store
            .create_project(
                "w1",
                &CreateProjectRequest {
                    project_id: "smoke".into(),
                    title: "Smoke".into(),
                    description: "e2e".into(),
                    spec_namespace: "Quod.Epoch0".into(),
                    default_bundle_id: Some("lean-4.36.0".into()),
                },
            )
            .await
            .unwrap();
        store
            .create_epoch(
                "smoke",
                "w1",
                &CreateEpochRequest {
                    epoch_id: 1,
                    bundle_id: "lean-4.36.0".into(),
                    base_commit: "0000".into(),
                    spec_module: "Spec".into(),
                    spec_decl_name: "Quod.Epoch0.smoke_stmt".into(),
                    spec_statement: "∀ n : Nat, (n + 1) + (n + 1) = 2 * n + 2".into(),
                    spec_sha256: "sha".into(),
                    spec_code: None,
                    root_task_id: "root".into(),
                    auto_activate: true,
                },
            )
            .await
            .unwrap();

        let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("fixtures")
            .join("epoch0-smoke");

        let accepted_tmp = tempfile::tempdir().unwrap();
        let cfg = VerifierConfig {
            spec_file: Some(root_dir.join("Spec.lean")),
            accepted_olean_dir: accepted_tmp.path().to_path_buf(),
            timeout_secs: 180,
            ..Default::default()
        };

        // 1. Publish DefOverlay via IR
        let def_code = std::fs::read_to_string(root_dir.join("samples/def_overlay_ok.lean")).unwrap();
        store
            .apply_ir(
                "smoke",
                1,
                "w1",
                &ApplyIrRequest {
                    ops: vec![GraphIrOp::PublishDefOverlay {
                        module_name: "QuodDef_01".into(),
                        code_utf8: def_code,
                    }],
                    dry_run: false,
                },
                blobs.as_ref(),
            )
            .await
            .unwrap();
        assert!(process_one_job(&store, &blobs, &cfg).await.unwrap());

        // 2. Spawn Route A: lemma_a + merge_a (depending on lemma_a)
        store
            .apply_ir(
                "smoke",
                1,
                "w1",
                &ApplyIrRequest {
                    ops: vec![
                        GraphIrOp::CreateRoute {
                            route_id: "route-a".into(),
                            title: "Via doublePlusTwo".into(),
                            rationale: "Helper def + lemma + merge".into(),
                        },
                        GraphIrOp::SpawnTask {
                            task_id: "lemma_a".into(),
                            route_id: Some("route-a".into()),
                            task_role: TaskRole::Lemma,
                            spec_decl_name: "lemma_a".into(),
                            spec_statement: "∀ n : Nat, (n + 1) + (n + 1) = doublePlusTwo n".into(),
                            target_inline: true,
                            candidate_decl: "proof_lemma_a".into(),
                            curated_imports: vec!["QuodDef_01".into()],
                            depends_on: vec![],
                            refutes_task_id: None,
                            module_hint: None,
                            priority: 10.0,
                            prior_p: 0.7,
                            expected_cost_c: 1.0,
                            racer_cap_k: 2,
                            min_tier: 1,
                            preferred_lineages: vec![],
                            credit_bounty: None,
                        },
                        GraphIrOp::SpawnTask {
                            task_id: "merge_a".into(),
                            route_id: Some("route-a".into()),
                            task_role: TaskRole::Merge,
                            spec_decl_name: "Quod.Epoch0.smoke_stmt".into(),
                            spec_statement: String::new(),
                            target_inline: false,
                            candidate_decl: "candidate".into(),
                            curated_imports: vec!["QuodDef_01".into()],
                            depends_on: vec!["lemma_a".into()],
                            refutes_task_id: None,
                            module_hint: None,
                            priority: 20.0,
                            prior_p: 0.8,
                            expected_cost_c: 1.0,
                            racer_cap_k: 2,
                            min_tier: 1,
                            preferred_lineages: vec![],
                            credit_bounty: None,
                        },
                    ],
                    dry_run: false,
                },
                blobs.as_ref(),
            )
            .await
            .unwrap();

        // Claim & prove lemma_a
        let pkt_a = store
            .claim_lease("smoke", "w1", &ClaimLeaseRequest::default(), 60, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pkt_a.task_id, "lemma_a");
        let la_code = std::fs::read_to_string(root_dir.join("samples/lemma_a.lean")).unwrap();
        let la_sha = blobs.put_bytes(la_code.as_bytes()).unwrap();
        store
            .create_submission(NewSubmissionInput {
                submission_id: Uuid::now_v7(),
                project_id: "smoke".into(),
                actor_id: "w1".into(),
                req: SubmitRequest {
                    epoch_id: 1,
                    task_id: "lemma_a".into(),
                    lease_id: pkt_a.lease.unwrap().lease_id,
                    kind: SubmissionKind::Proof,
                    spec_sha256: "sha".into(),
                    code_utf8: la_code,
                    idea_attr_id: None,
                },
                blob_sha256: la_sha,
                received_at_ns: 1,
                stage0_passed: true,
                stage0_exit_code: 0,
                stage0_diagnostics: vec![],
            })
            .await
            .unwrap();
        assert!(process_one_job(&store, &blobs, &cfg).await.unwrap());

        // Merge task is now unlocked and carries lemma_a's overlay module!
        let pkt_m = store
            .claim_lease("smoke", "w1", &ClaimLeaseRequest::default(), 60, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pkt_m.task_id, "merge_a");
        let ov_mod = pkt_m
            .curated_imports
            .iter()
            .find(|m| m.starts_with("QuodOverlay_lemma_a_"))
            .unwrap()
            .clone();

        let merge_code = format!(
            "import Spec\nimport QuodDef_01\nimport {ov_mod}\ntheorem candidate : Quod.Epoch0.smoke_stmt := by\n  intro n\n  have h1 := proof_lemma_a n\n  dsimp [doublePlusTwo] at h1\n  omega\n"
        );
        let m_sha = blobs.put_bytes(merge_code.as_bytes()).unwrap();
        store
            .create_submission(NewSubmissionInput {
                submission_id: Uuid::now_v7(),
                project_id: "smoke".into(),
                actor_id: "w1".into(),
                req: SubmitRequest {
                    epoch_id: 1,
                    task_id: "merge_a".into(),
                    lease_id: pkt_m.lease.unwrap().lease_id,
                    kind: SubmissionKind::Proof,
                    spec_sha256: "sha".into(),
                    code_utf8: merge_code,
                    idea_attr_id: None,
                },
                blob_sha256: m_sha,
                received_at_ns: 2,
                stage0_passed: true,
                stage0_exit_code: 0,
                stage0_diagnostics: vec![],
            })
            .await
            .unwrap();
        assert!(process_one_job(&store, &blobs, &cfg).await.unwrap());

        let g = store.project_graph("smoke", 1).await.unwrap().unwrap();
        assert_eq!(g.project_status, ProjectStatus::Proved);
    }
}
