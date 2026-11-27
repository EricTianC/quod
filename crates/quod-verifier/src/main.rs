//! `quod-verifier` — bounded Stage 1/2 Lean kernel verification daemon (Quod).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use quod_store::{LocalFsBlobStore, PgEventStore, QuodStore};
use quod_verifier::{RunnerMode, VerifierConfig, process_one_job};
use tokio::sync::Semaphore;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "quod-verifier",
    about = "Quod Stage 1/2 Lean kernel verification daemon"
)]
struct Args {
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,

    #[arg(long, env = "QUOD_BLOB_DIR", default_value = "/var/lib/quod/blobs")]
    blob_dir: PathBuf,

    #[arg(
        long,
        env = "QUOD_ACCEPTED_OLEAN_DIR",
        default_value = "/var/lib/quod/accepted_olean"
    )]
    accepted_olean_dir: PathBuf,

    #[arg(
        long = "epoch-lean-path",
        env = "QUOD_EPOCH_LEAN_PATH",
        default_value = "/opt/quod/epoch_lib"
    )]
    epoch_lean_path: String,

    #[arg(long, env = "QUOD_TOOLCHAIN_DIR", default_value = "/opt/quod/lean")]
    toolchain_dir: PathBuf,

    #[arg(long, env = "QUOD_SPEC_FILE")]
    spec_file: Option<PathBuf>,

    #[arg(long, env = "QUOD_LEAN_BIN", default_value = "lean")]
    lean_bin: PathBuf,

    #[arg(long, value_enum, env = "QUOD_RUNNER", default_value = "local")]
    runner: RunnerMode,

    #[arg(long, default_value_t = 4)]
    concurrency: usize,

    #[arg(long, default_value_t = 500)]
    poll_ms: u64,

    #[arg(long, default_value_t = 60)]
    timeout_secs: u64,

    #[arg(long, default_value = "verifier-01")]
    verifier_id: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let pg = PgEventStore::connect(&args.database_url, (args.concurrency as u32) + 4).await?;
    pg.run_migrations().await?;

    let store: Arc<dyn QuodStore> = Arc::new(pg);
    let blobs: Arc<dyn quod_store::BlobStore> = Arc::new(LocalFsBlobStore::new(&args.blob_dir));
    let epoch_lean_paths: Vec<PathBuf> = args
        .epoch_lean_path
        .split(':')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    let cfg = Arc::new(VerifierConfig {
        verifier_id: args.verifier_id,
        runner_mode: args.runner,
        lean_bin: args.lean_bin,
        spec_file: args.spec_file,
        toolchain_dir: args.toolchain_dir,
        epoch_lean_paths,
        accepted_olean_dir: args.accepted_olean_dir,
        timeout_secs: args.timeout_secs,
    });
    let sem = Arc::new(Semaphore::new(args.concurrency.max(1)));

    info!(
        concurrency = args.concurrency,
        runner = ?cfg.runner_mode,
        "quod-verifier started"
    );

    loop {
        let permit = sem.clone().acquire_owned().await?;
        let st = store.clone();
        let bl = blobs.clone();
        let c = cfg.clone();
        let had_work = process_one_job(&st, &bl, &c).await.unwrap_or_else(|err| {
            error!(error = %err, "verification job error");
            false
        });
        drop(permit);

        if !had_work {
            tokio::time::sleep(Duration::from_millis(args.poll_ms)).await;
        }
    }
}
