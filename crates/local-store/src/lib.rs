mod checkpoint_repository;
mod lease_repository;

use std::path::Path;

pub use checkpoint_repository::{
    AppendCheckpoint, AppendOutcome, CheckpointRecord, CheckpointRepository,
};
pub use lease_repository::{LeaseRepository, RecoverableTask};
use rusqlite::{Connection, OptionalExtension};
use thiserror::Error;

const INITIAL_MIGRATION: &str = include_str!("../../../migrations/0001_initial.sql");
const WORKSPACE_POLICY_MIGRATION: &str =
    include_str!("../../../migrations/0002_batch_workspace_policy.sql");
const SCHEMA_VERSION: i64 = 2;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("unsupported schema version {0}")]
    UnsupportedSchemaVersion(i64),
    #[error("task {task_id} is not leased by {lease_owner}")]
    LeaseNotOwned {
        task_id: String,
        lease_owner: String,
    },
    #[error("idempotency key {idempotency_key} was reused with different checkpoint data")]
    IdempotencyConflict { idempotency_key: String },
}

pub type StoreResult<T> = Result<T, StoreError>;

pub struct LocalStore {
    connection: Connection,
}

impl LocalStore {
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let connection = Connection::open(path)?;
        Self::from_connection(connection)
    }

    pub fn open_in_memory() -> StoreResult<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(connection: Connection) -> StoreResult<Self> {
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "busy_timeout", 5_000_i64)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;

        let mut store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    pub fn migrate(&mut self) -> StoreResult<()> {
        let version: i64 = self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;

        match version {
            SCHEMA_VERSION => Ok(()),
            0 | 1 => {
                let transaction = self.connection.transaction()?;
                if version == 0 {
                    transaction.execute_batch(INITIAL_MIGRATION)?;
                }
                // Version 1 stores know nothing of the workspace policy; the
                // column is added with the safe default so old batches keep the
                // behaviour they were started with.
                transaction.execute_batch(WORKSPACE_POLICY_MIGRATION)?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                transaction.commit()?;
                Ok(())
            }
            other => Err(StoreError::UnsupportedSchemaVersion(other)),
        }
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn schema_version(&self) -> StoreResult<i64> {
        Ok(self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    pub fn has_column(&self, table: &str, column: &str) -> StoreResult<bool> {
        let sql = format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1");
        Ok(self
            .connection
            .query_row(&sql, [column], |_| Ok(()))
            .optional()?
            .is_some())
    }

    pub fn checkpoints(&self) -> CheckpointRepository<'_> {
        CheckpointRepository::new(&self.connection)
    }

    pub fn leases(&self) -> LeaseRepository<'_> {
        LeaseRepository::new(&self.connection)
    }
}
