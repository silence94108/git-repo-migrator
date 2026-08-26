CREATE TABLE connection (
    id TEXT PRIMARY KEY,
    platform_type TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    credential_ref TEXT NOT NULL,
    capabilities_json TEXT NOT NULL DEFAULT '{}',
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE plan (
    id TEXT PRIMARY KEY,
    selection_json TEXT NOT NULL,
    policy_json TEXT NOT NULL,
    module_json TEXT NOT NULL,
    plan_hash TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE batch (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL REFERENCES plan(id),
    status TEXT NOT NULL,
    total INTEGER NOT NULL DEFAULT 0 CHECK (total >= 0),
    completed INTEGER NOT NULL DEFAULT 0 CHECK (completed >= 0),
    failed INTEGER NOT NULL DEFAULT 0 CHECK (failed >= 0),
    started_at_ms INTEGER,
    ended_at_ms INTEGER
);

CREATE TABLE repository_candidate (
    id TEXT PRIMARY KEY,
    connection_id TEXT REFERENCES connection(id),
    source_url TEXT NOT NULL,
    provider_id TEXT,
    name TEXT NOT NULL,
    namespace TEXT NOT NULL,
    visibility TEXT NOT NULL,
    role TEXT NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE repository_task (
    id TEXT PRIMARY KEY,
    batch_id TEXT NOT NULL REFERENCES batch(id),
    candidate_id TEXT NOT NULL REFERENCES repository_candidate(id),
    target_url TEXT NOT NULL,
    target_id TEXT,
    action TEXT NOT NULL,
    status TEXT NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    lease_owner TEXT,
    lease_expires_at_ms INTEGER,
    error_code TEXT,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (batch_id, candidate_id),
    UNIQUE (batch_id, target_url),
    CHECK ((lease_owner IS NULL) = (lease_expires_at_ms IS NULL))
);

CREATE INDEX repository_task_recovery_idx
    ON repository_task(status, lease_expires_at_ms)
    WHERE lease_owner IS NOT NULL;

CREATE TABLE checkpoint (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES repository_task(id),
    stage TEXT NOT NULL,
    attempt INTEGER NOT NULL CHECK (attempt >= 0),
    transition TEXT NOT NULL CHECK (
        transition IN ('started', 'heartbeat', 'succeeded', 'failed', 'interrupted')
    ),
    input_hash TEXT NOT NULL,
    output_summary_json TEXT NOT NULL DEFAULT '{}',
    resumable INTEGER NOT NULL CHECK (resumable IN (0, 1)),
    idempotency_key TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    UNIQUE (task_id, idempotency_key)
);

CREATE INDEX checkpoint_current_idx
    ON checkpoint(task_id, stage, created_at_ms DESC);

CREATE TRIGGER checkpoint_append_only_update
BEFORE UPDATE ON checkpoint
BEGIN
    SELECT RAISE(ABORT, 'checkpoint is append-only');
END;

CREATE TRIGGER checkpoint_append_only_delete
BEFORE DELETE ON checkpoint
BEGIN
    SELECT RAISE(ABORT, 'checkpoint is append-only');
END;

CREATE TABLE module_result (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES repository_task(id),
    module TEXT NOT NULL,
    fidelity TEXT NOT NULL CHECK (
        fidelity IN ('native_rebuild', 'read_only_archive', 'unsupported')
    ),
    status TEXT NOT NULL,
    source_count INTEGER NOT NULL DEFAULT 0,
    target_count INTEGER NOT NULL DEFAULT 0,
    identity_map_json TEXT NOT NULL DEFAULT '{}',
    state_map_json TEXT NOT NULL DEFAULT '{}',
    attachment_map_json TEXT NOT NULL DEFAULT '{}',
    source_links_json TEXT NOT NULL DEFAULT '[]',
    error_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE log_event (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES repository_task(id),
    level TEXT NOT NULL,
    stage TEXT NOT NULL,
    message_code TEXT NOT NULL,
    safe_context_json TEXT NOT NULL DEFAULT '{}',
    created_at_ms INTEGER NOT NULL
);
