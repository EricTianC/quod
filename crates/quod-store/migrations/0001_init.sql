-- Quod Control-Plane Schema (PostgreSQL 16)
-- Multi-project shared base + Unified Actor RBAC + Route-grouped pure-Prop Task DAG + Verified Overlays.

CREATE TABLE IF NOT EXISTS toolchain_bundles (
    bundle_id      TEXT PRIMARY KEY,
    lean_toolchain TEXT NOT NULL,
    mathlib_rev    TEXT NOT NULL,
    toolchain_dir  TEXT NOT NULL,
    lean_paths     JSONB NOT NULL DEFAULT '[]'::jsonb,
    bundle_sha256  TEXT NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS projects (
    project_id        TEXT PRIMARY KEY,
    title             TEXT NOT NULL,
    description       TEXT NOT NULL DEFAULT '',
    spec_namespace    TEXT NOT NULL,
    status            TEXT NOT NULL,
    default_bundle_id TEXT,
    created_by        TEXT NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS actors (
    actor_id             TEXT PRIMARY KEY,
    pubkey_ed25519       TEXT NOT NULL UNIQUE,
    roles                JSONB NOT NULL DEFAULT '["worker"]'::jsonb,
    project_scopes       JSONB NOT NULL DEFAULT '[]'::jsonb,
    lineage_cluster      TEXT NOT NULL,
    tier                 SMALLINT NOT NULL DEFAULT 1,
    skill_tags           JSONB NOT NULL DEFAULT '[]'::jsonb,
    enabled              BOOLEAN NOT NULL DEFAULT TRUE,
    display_name         TEXT,
    git_email            TEXT,
    credit_points        BIGINT NOT NULL DEFAULT 0,
    ema_accept_rate      DOUBLE PRECISION NOT NULL DEFAULT 0.5,
    total_probes         BIGINT NOT NULL DEFAULT 0,
    probe_passes         BIGINT NOT NULL DEFAULT 0,
    total_submissions    BIGINT NOT NULL DEFAULT 0,
    accepted_submissions BIGINT NOT NULL DEFAULT 0,
    rejected_submissions BIGINT NOT NULL DEFAULT 0,
    avg_compile_ms       BIGINT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS epochs (
    project_id      TEXT NOT NULL REFERENCES projects(project_id),
    epoch_id        INT NOT NULL,
    state           TEXT NOT NULL,
    bundle_id       TEXT NOT NULL REFERENCES toolchain_bundles(bundle_id),
    base_commit     TEXT NOT NULL,
    spec_module     TEXT NOT NULL,
    spec_decl_name  TEXT NOT NULL,
    spec_statement  TEXT NOT NULL DEFAULT '',
    spec_sha256     TEXT NOT NULL,
    root_task_id    TEXT NOT NULL,
    dispatch_policy JSONB NOT NULL DEFAULT '{"algo":"weighted_voi","exploration_rate":0.05,"allow_cross_route_parallel":true}'::jsonb,
    created_by      TEXT NOT NULL,
    audited_by      TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, epoch_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_single_active_epoch_per_project
    ON epochs (project_id) WHERE state = 'ACTIVE';

CREATE TABLE IF NOT EXISTS routes (
    project_id TEXT NOT NULL,
    epoch_id   INT NOT NULL,
    route_id   TEXT NOT NULL,
    title      TEXT NOT NULL,
    rationale  TEXT NOT NULL DEFAULT '',
    state      TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, epoch_id, route_id),
    FOREIGN KEY (project_id, epoch_id) REFERENCES epochs(project_id, epoch_id)
);

CREATE TABLE IF NOT EXISTS tasks (
    project_id         TEXT NOT NULL,
    epoch_id           INT NOT NULL,
    task_id            TEXT NOT NULL,
    route_id           TEXT,
    task_role          TEXT NOT NULL,
    state              TEXT NOT NULL,
    priority           DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    racer_cap_k        SMALLINT NOT NULL DEFAULT 2,
    lease_ttl_secs     BIGINT,
    min_tier           SMALLINT NOT NULL DEFAULT 1,
    preferred_lineages JSONB NOT NULL DEFAULT '[]'::jsonb,
    prior_p            DOUBLE PRECISION NOT NULL DEFAULT 0.5,
    expected_cost_c    DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    spec_decl_name     TEXT NOT NULL,
    spec_statement     TEXT NOT NULL DEFAULT '',
    target_inline      BOOLEAN NOT NULL DEFAULT FALSE,
    candidate_decl     TEXT NOT NULL DEFAULT 'candidate',
    curated_imports    JSONB NOT NULL DEFAULT '[]'::jsonb,
    module_hint        TEXT,
    credit_bounty      INT NOT NULL DEFAULT 50,
    total_attempts     BIGINT NOT NULL DEFAULT 0,
    failed_attempts    BIGINT NOT NULL DEFAULT 0,
    timeout_count      BIGINT NOT NULL DEFAULT 0,
    failed_lineages    JSONB NOT NULL DEFAULT '[]'::jsonb,
    last_compile_ms    BIGINT,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, epoch_id, task_id),
    FOREIGN KEY (project_id, epoch_id) REFERENCES epochs(project_id, epoch_id)
);

CREATE TABLE IF NOT EXISTS task_edges (
    project_id   TEXT NOT NULL,
    epoch_id     INT NOT NULL,
    from_task_id TEXT NOT NULL,
    to_task_id   TEXT NOT NULL,
    edge_kind    TEXT NOT NULL,
    PRIMARY KEY (project_id, epoch_id, from_task_id, to_task_id, edge_kind)
);

CREATE TABLE IF NOT EXISTS overlays (
    project_id      TEXT NOT NULL,
    epoch_id        INT NOT NULL,
    module_name     TEXT NOT NULL,
    overlay_kind    TEXT NOT NULL,
    source_task_id  TEXT,
    source_actor_id TEXT NOT NULL,
    blob_sha256     TEXT NOT NULL,
    imports         JSONB NOT NULL DEFAULT '[]'::jsonb,
    compile_ms      BIGINT NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, epoch_id, module_name)
);

CREATE TABLE IF NOT EXISTS leases (
    lease_id         UUID PRIMARY KEY,
    project_id       TEXT NOT NULL,
    epoch_id         INT NOT NULL,
    task_id          TEXT NOT NULL,
    actor_id         TEXT NOT NULL,
    lineage_cluster  TEXT NOT NULL,
    propensity_score DOUBLE PRECISION NOT NULL,
    issued_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at       TIMESTAMPTZ NOT NULL,
    status           TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS submissions (
    submission_id     UUID PRIMARY KEY,
    submission_sha256 TEXT NOT NULL UNIQUE,
    project_id        TEXT NOT NULL,
    epoch_id          INT NOT NULL,
    task_id           TEXT NOT NULL,
    actor_id          TEXT NOT NULL,
    kind              TEXT NOT NULL,
    state             TEXT NOT NULL,
    blob_sha256       TEXT NOT NULL,
    received_at_ns    BIGINT NOT NULL,
    compile_ms        BIGINT,
    idea_attr_id      TEXT,
    reject_detail     JSONB
);

CREATE TABLE IF NOT EXISTS verification_jobs (
    job_id           UUID PRIMARY KEY,
    submission_id    UUID NOT NULL UNIQUE REFERENCES submissions(submission_id),
    project_id       TEXT NOT NULL,
    epoch_id         INT NOT NULL,
    task_id          TEXT NOT NULL,
    actor_id         TEXT NOT NULL,
    kind             TEXT NOT NULL,
    bundle_id        TEXT NOT NULL,
    spec_module      TEXT NOT NULL,
    spec_decl        TEXT NOT NULL,
    spec_statement   TEXT NOT NULL,
    target_inline    BOOLEAN NOT NULL DEFAULT FALSE,
    candidate_module TEXT NOT NULL,
    candidate_decl   TEXT NOT NULL DEFAULT 'candidate',
    blob_sha256      TEXT NOT NULL,
    curated_imports  JSONB NOT NULL DEFAULT '[]'::jsonb,
    status           TEXT NOT NULL DEFAULT 'PENDING',
    claimed_by       TEXT,
    claimed_at       TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_verification_jobs_pending
    ON verification_jobs (created_at) WHERE status = 'PENDING';

CREATE TABLE IF NOT EXISTS graveyard_entries (
    entry_seq    BIGSERIAL PRIMARY KEY,
    project_id   TEXT NOT NULL,
    epoch_id     INT NOT NULL,
    task_id      TEXT NOT NULL,
    failure_kind TEXT NOT NULL,
    summary      TEXT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_graveyard_proj_epoch_task
    ON graveyard_entries (project_id, epoch_id, task_id, entry_seq);

CREATE TABLE IF NOT EXISTS events (
    event_seq       BIGSERIAL PRIMARY KEY,
    event_id        UUID NOT NULL UNIQUE,
    occurred_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    project_id      TEXT NOT NULL,
    epoch_id        INT NOT NULL,
    aggregate_type  TEXT NOT NULL,
    aggregate_id    TEXT NOT NULL,
    event_type      TEXT NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    actor_id        TEXT NOT NULL,
    payload         JSONB NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_events_proj_epoch
    ON events (project_id, epoch_id, event_seq);

CREATE TABLE IF NOT EXISTS invite_tokens (
    invite_code     TEXT PRIMARY KEY,
    roles           JSONB NOT NULL,
    project_scopes  JSONB NOT NULL DEFAULT '[]'::jsonb,
    max_uses        INT NOT NULL DEFAULT 1,
    used_count      INT NOT NULL DEFAULT 0,
    initial_tier    SMALLINT NOT NULL DEFAULT 1,
    created_by      TEXT NOT NULL,
    note            TEXT NOT NULL DEFAULT '',
    expires_at      TIMESTAMPTZ NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS project_actor_blocks (
    project_id  TEXT NOT NULL REFERENCES projects(project_id),
    actor_id    TEXT NOT NULL REFERENCES actors(actor_id),
    blocked_by  TEXT NOT NULL,
    reason      TEXT NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, actor_id)
);

CREATE TABLE IF NOT EXISTS credit_ledger (
    entry_id      UUID PRIMARY KEY,
    project_id    TEXT NOT NULL,
    epoch_id      INT NOT NULL,
    task_id       TEXT NOT NULL,
    submission_id UUID,
    actor_id      TEXT NOT NULL REFERENCES actors(actor_id),
    points        BIGINT NOT NULL,
    reason        TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_credit_ledger_proj_actor
    ON credit_ledger (project_id, actor_id);
CREATE INDEX IF NOT EXISTS idx_credit_ledger_actor_created
    ON credit_ledger (actor_id, created_at DESC);
