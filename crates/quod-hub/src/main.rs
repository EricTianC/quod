//! `quod-hub` — HTTP coordinator entrypoint for Quod.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use quod_hub::{AppState, build_router};
use quod_store::{LocalFsBlobStore, MemoryStore, PgEventStore, QuodStore};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "quod-hub", about = "Quod event-sourced Lean 4 coordinator")]
struct CliArgs {
    #[arg(long, env = "QUOD_HUB_LISTEN", default_value = "0.0.0.0:8080")]
    listen: SocketAddr,

    #[arg(long, env = "DATABASE_URL")]
    database_url: Option<String>,

    #[arg(long, env = "QUOD_BLOB_DIR", default_value = "/var/lib/quod/blobs")]
    blob_dir: PathBuf,

    #[arg(long, env = "QUOD_ADMIN_TOKEN", default_value = "quod-dev-admin")]
    admin_token: String,

    #[arg(long, default_value_t = 900)]
    lease_ttl_secs: i64,

    #[arg(long, default_value_t = 120)]
    max_auth_skew_secs: i64,

    /// Allow uninvited Workers to self-register at `POST /api/actors/join`.
    #[arg(long, env = "QUOD_OPEN_WORKER_JOIN", default_value_t = false)]
    open_worker_join: bool,

    /// Maximum concurrent ACTIVE leases an individual actor may hold in one project epoch.
    #[arg(long, env = "QUOD_MAX_ACTIVE_LEASES_PER_ACTOR", default_value_t = 1)]
    max_active_leases_per_actor: usize,

    /// Populate demo projects and routes on startup (for Web Portal preview).
    #[arg(long, default_value_t = false)]
    seed_demo: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = CliArgs::parse();
    let (store, db_configured): (Arc<dyn QuodStore>, bool) = match &args.database_url {
        Some(url) => {
            let pg = PgEventStore::connect(url, 16).await?;
            pg.run_migrations().await?;
            info!("connected to PostgreSQL and verified migrations");
            (Arc::new(pg), true)
        }
        None => {
            warn!("DATABASE_URL not set; starting quod-hub with ephemeral MemoryStore");
            (Arc::new(MemoryStore::new()), false)
        }
    };

    let blobs: Arc<dyn quod_store::BlobStore> = Arc::new(LocalFsBlobStore::new(&args.blob_dir));
    if args.seed_demo {
        quod_hub::seed_demo_data(store.as_ref(), blobs.as_ref()).await?;
        info!("seeded demo projects for web portal preview");
    }

    let app = build_router(Arc::new(AppState {
        store,
        blobs,
        admin_token: args.admin_token,
        db_configured,
        lease_ttl_secs: args.lease_ttl_secs,
        max_auth_skew_secs: args.max_auth_skew_secs,
        open_worker_join: args.open_worker_join,
        max_active_leases_per_actor: args.max_active_leases_per_actor,
    }));

    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    info!(listen = %args.listen, "quod-hub listening");
    axum::serve(listener, app).await?;
    Ok(())
}
