use git_repo_migrator_local_store::{AppendCheckpoint, AppendOutcome, LocalStore, StoreError};
use rusqlite::params;

fn seeded_store() -> LocalStore {
    let store = LocalStore::open_in_memory().expect("store opens");
    store
        .connection()
        .execute(
            "INSERT INTO connection
             (id, platform_type, endpoint, credential_ref, created_at_ms)
             VALUES ('source', 'generic', 'https://source.example', 'credential/source', 1)",
            [],
        )
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO plan
             (id, selection_json, policy_json, module_json, plan_hash, status, created_at_ms)
             VALUES ('plan', '{}', '{}', '{}', 'plan-hash', 'frozen', 1)",
            [],
        )
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO batch (id, plan_id, status, total) VALUES ('batch', 'plan', 'running', 1)",
            [],
        )
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO repository_candidate
             (id, connection_id, source_url, name, namespace, visibility, role)
             VALUES ('candidate', 'source', 'https://source.example/acme/repo.git',
                     'repo', 'acme', 'private', 'owner')",
            [],
        )
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO repository_task
             (id, batch_id, candidate_id, target_url, action, status, updated_at_ms)
             VALUES ('task', 'batch', 'candidate', 'https://target.example/acme/repo.git',
                     'create', 'git', 1)",
            [],
        )
        .unwrap();
    store
}

fn checkpoint<'a>(id: &'a str, key: &'a str, transition: &'a str) -> AppendCheckpoint<'a> {
    AppendCheckpoint {
        id,
        task_id: "task",
        stage: "git",
        attempt: 1,
        transition,
        input_hash: "input-hash",
        output_summary_json: "{\"refs\":2}",
        resumable: true,
        idempotency_key: key,
        created_at_ms: 120,
    }
}

#[test]
fn migration_is_idempotent_enables_integrity_and_has_no_secret_columns() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let mut store = LocalStore::open(&database).unwrap();
    store.migrate().unwrap();

    assert_eq!(store.schema_version().unwrap(), 1);
    assert_eq!(
        store
            .connection()
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .connection()
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    for forbidden in ["token", "password", "secret", "private_key", "cookie"] {
        let count: i64 = store
            .connection()
            .query_row(
                "SELECT count(*) FROM pragma_table_info('connection') WHERE lower(name) = ?1",
                [forbidden],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "unexpected secret column {forbidden}");
    }
}

#[test]
fn checkpoint_history_is_append_only_and_idempotent() {
    let store = seeded_store();
    assert!(store
        .leases()
        .acquire("task", "worker-a", 100, 100)
        .unwrap());

    let first = checkpoint("cp-1", "git-push-1", "started");
    assert_eq!(
        store.checkpoints().append("worker-a", 110, &first).unwrap(),
        AppendOutcome::Inserted
    );
    assert_eq!(
        store.checkpoints().append("worker-a", 110, &first).unwrap(),
        AppendOutcome::Duplicate
    );
    assert_eq!(store.checkpoints().history("task").unwrap().len(), 1);

    let current = store.checkpoints().current("task", "git").unwrap().unwrap();
    assert_eq!(current.transition, "started");
    assert!(store
        .connection()
        .execute(
            "UPDATE checkpoint SET transition = 'failed' WHERE id = 'cp-1'",
            []
        )
        .is_err());
    assert!(store
        .connection()
        .execute("DELETE FROM checkpoint WHERE id = 'cp-1'", [])
        .is_err());

    let conflicting = AppendCheckpoint {
        transition: "succeeded",
        ..first
    };
    assert!(matches!(
        store.checkpoints().append("worker-a", 110, &conflicting),
        Err(StoreError::IdempotencyConflict { .. })
    ));
}

#[test]
fn only_current_owner_can_heartbeat_or_append() {
    let store = seeded_store();
    assert!(store
        .leases()
        .acquire("task", "worker-a", 100, 100)
        .unwrap());
    assert!(!store
        .leases()
        .heartbeat("task", "worker-b", 120, 100)
        .unwrap());
    assert!(matches!(
        store
            .checkpoints()
            .append("worker-b", 120, &checkpoint("cp-1", "key-1", "started")),
        Err(StoreError::LeaseNotOwned { .. })
    ));
    assert!(store
        .leases()
        .heartbeat("task", "worker-a", 120, 100)
        .unwrap());
}

#[test]
fn recovery_lists_and_takes_over_only_expired_leases() {
    let store = seeded_store();
    assert!(store.leases().acquire("task", "worker-a", 100, 50).unwrap());

    assert!(store.leases().recoverable(149).unwrap().is_empty());
    let recoverable = store.leases().recoverable(150).unwrap();
    assert_eq!(recoverable.len(), 1);
    assert_eq!(recoverable[0].previous_owner, "worker-a");
    assert!(!store
        .leases()
        .acquire_expired("task", "worker-b", 149, 100)
        .unwrap());
    assert!(store
        .leases()
        .acquire_expired("task", "worker-b", 150, 100)
        .unwrap());
    assert!(store.leases().recoverable(150).unwrap().is_empty());

    let owner: String = store
        .connection()
        .query_row(
            "SELECT lease_owner FROM repository_task WHERE id = ?1",
            params!["task"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(owner, "worker-b");
}
