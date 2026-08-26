use rusqlite::{params, Connection, OptionalExtension};

use crate::{StoreError, StoreResult};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppendCheckpoint<'a> {
    pub id: &'a str,
    pub task_id: &'a str,
    pub stage: &'a str,
    pub attempt: i64,
    pub transition: &'a str,
    pub input_hash: &'a str,
    pub output_summary_json: &'a str,
    pub resumable: bool,
    pub idempotency_key: &'a str,
    pub created_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppendOutcome {
    Inserted,
    Duplicate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointRecord {
    pub id: String,
    pub task_id: String,
    pub stage: String,
    pub attempt: i64,
    pub transition: String,
    pub input_hash: String,
    pub output_summary_json: String,
    pub resumable: bool,
    pub idempotency_key: String,
    pub created_at_ms: i64,
}

pub struct CheckpointRepository<'connection> {
    connection: &'connection Connection,
}

impl<'connection> CheckpointRepository<'connection> {
    pub(crate) fn new(connection: &'connection Connection) -> Self {
        Self { connection }
    }

    pub fn append(
        &self,
        lease_owner: &str,
        now_ms: i64,
        checkpoint: &AppendCheckpoint<'_>,
    ) -> StoreResult<AppendOutcome> {
        let owns_lease = self.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM repository_task
                WHERE id = ?1 AND lease_owner = ?2 AND lease_expires_at_ms > ?3
            )",
            params![checkpoint.task_id, lease_owner, now_ms],
            |row| row.get::<_, bool>(0),
        )?;
        if !owns_lease {
            return Err(StoreError::LeaseNotOwned {
                task_id: checkpoint.task_id.to_owned(),
                lease_owner: lease_owner.to_owned(),
            });
        }

        let inserted = self.connection.execute(
            "INSERT OR IGNORE INTO checkpoint (
                id, task_id, stage, attempt, transition, input_hash,
                output_summary_json, resumable, idempotency_key, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                checkpoint.id,
                checkpoint.task_id,
                checkpoint.stage,
                checkpoint.attempt,
                checkpoint.transition,
                checkpoint.input_hash,
                checkpoint.output_summary_json,
                checkpoint.resumable,
                checkpoint.idempotency_key,
                checkpoint.created_at_ms,
            ],
        )?;

        if inserted == 1 {
            return Ok(AppendOutcome::Inserted);
        }

        let existing = self
            .by_idempotency_key(checkpoint.task_id, checkpoint.idempotency_key)?
            .expect("the unique idempotency key must resolve after INSERT OR IGNORE");
        let expected = CheckpointRecord::from(checkpoint);
        if existing == expected {
            Ok(AppendOutcome::Duplicate)
        } else {
            Err(StoreError::IdempotencyConflict {
                idempotency_key: checkpoint.idempotency_key.to_owned(),
            })
        }
    }

    pub fn current(&self, task_id: &str, stage: &str) -> StoreResult<Option<CheckpointRecord>> {
        self.connection
            .query_row(
                "SELECT id, task_id, stage, attempt, transition, input_hash,
                        output_summary_json, resumable, idempotency_key, created_at_ms
                 FROM checkpoint
                 WHERE task_id = ?1 AND stage = ?2
                 ORDER BY created_at_ms DESC, rowid DESC
                 LIMIT 1",
                params![task_id, stage],
                map_checkpoint,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn history(&self, task_id: &str) -> StoreResult<Vec<CheckpointRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, task_id, stage, attempt, transition, input_hash,
                    output_summary_json, resumable, idempotency_key, created_at_ms
             FROM checkpoint
             WHERE task_id = ?1
             ORDER BY created_at_ms, rowid",
        )?;
        let records = statement
            .query_map([task_id], map_checkpoint)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    fn by_idempotency_key(
        &self,
        task_id: &str,
        idempotency_key: &str,
    ) -> StoreResult<Option<CheckpointRecord>> {
        self.connection
            .query_row(
                "SELECT id, task_id, stage, attempt, transition, input_hash,
                        output_summary_json, resumable, idempotency_key, created_at_ms
                 FROM checkpoint WHERE task_id = ?1 AND idempotency_key = ?2",
                params![task_id, idempotency_key],
                map_checkpoint,
            )
            .optional()
            .map_err(Into::into)
    }
}

fn map_checkpoint(row: &rusqlite::Row<'_>) -> rusqlite::Result<CheckpointRecord> {
    Ok(CheckpointRecord {
        id: row.get(0)?,
        task_id: row.get(1)?,
        stage: row.get(2)?,
        attempt: row.get(3)?,
        transition: row.get(4)?,
        input_hash: row.get(5)?,
        output_summary_json: row.get(6)?,
        resumable: row.get(7)?,
        idempotency_key: row.get(8)?,
        created_at_ms: row.get(9)?,
    })
}

impl From<&AppendCheckpoint<'_>> for CheckpointRecord {
    fn from(value: &AppendCheckpoint<'_>) -> Self {
        Self {
            id: value.id.to_owned(),
            task_id: value.task_id.to_owned(),
            stage: value.stage.to_owned(),
            attempt: value.attempt,
            transition: value.transition.to_owned(),
            input_hash: value.input_hash.to_owned(),
            output_summary_json: value.output_summary_json.to_owned(),
            resumable: value.resumable,
            idempotency_key: value.idempotency_key.to_owned(),
            created_at_ms: value.created_at_ms,
        }
    }
}
