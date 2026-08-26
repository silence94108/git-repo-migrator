use rusqlite::{params, Connection};

use crate::StoreResult;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoverableTask {
    pub task_id: String,
    pub batch_id: String,
    pub status: String,
    pub previous_owner: String,
    pub lease_expired_at_ms: i64,
    pub attempt: i64,
}

pub struct LeaseRepository<'connection> {
    connection: &'connection Connection,
}

impl<'connection> LeaseRepository<'connection> {
    pub(crate) fn new(connection: &'connection Connection) -> Self {
        Self { connection }
    }

    pub fn acquire(
        &self,
        task_id: &str,
        owner: &str,
        now_ms: i64,
        ttl_ms: i64,
    ) -> StoreResult<bool> {
        let expires_at = now_ms.saturating_add(ttl_ms);
        let changed = self.connection.execute(
            "UPDATE repository_task
             SET lease_owner = ?2, lease_expires_at_ms = ?3, updated_at_ms = ?4
             WHERE id = ?1
               AND (lease_owner IS NULL OR lease_owner = ?2 OR lease_expires_at_ms <= ?4)",
            params![task_id, owner, expires_at, now_ms],
        )?;
        Ok(changed == 1)
    }

    pub fn acquire_expired(
        &self,
        task_id: &str,
        owner: &str,
        now_ms: i64,
        ttl_ms: i64,
    ) -> StoreResult<bool> {
        let expires_at = now_ms.saturating_add(ttl_ms);
        let changed = self.connection.execute(
            "UPDATE repository_task
             SET lease_owner = ?2, lease_expires_at_ms = ?3, updated_at_ms = ?4
             WHERE id = ?1 AND lease_owner IS NOT NULL AND lease_expires_at_ms <= ?4",
            params![task_id, owner, expires_at, now_ms],
        )?;
        Ok(changed == 1)
    }

    pub fn heartbeat(
        &self,
        task_id: &str,
        owner: &str,
        now_ms: i64,
        ttl_ms: i64,
    ) -> StoreResult<bool> {
        let expires_at = now_ms.saturating_add(ttl_ms);
        let changed = self.connection.execute(
            "UPDATE repository_task
             SET lease_expires_at_ms = ?3, updated_at_ms = ?4
             WHERE id = ?1 AND lease_owner = ?2 AND lease_expires_at_ms > ?4",
            params![task_id, owner, expires_at, now_ms],
        )?;
        Ok(changed == 1)
    }

    pub fn release(&self, task_id: &str, owner: &str, now_ms: i64) -> StoreResult<bool> {
        let changed = self.connection.execute(
            "UPDATE repository_task
             SET lease_owner = NULL, lease_expires_at_ms = NULL, updated_at_ms = ?3
             WHERE id = ?1 AND lease_owner = ?2",
            params![task_id, owner, now_ms],
        )?;
        Ok(changed == 1)
    }

    pub fn recoverable(&self, now_ms: i64) -> StoreResult<Vec<RecoverableTask>> {
        let mut statement = self.connection.prepare(
            "SELECT id, batch_id, status, lease_owner, lease_expires_at_ms, attempt
             FROM repository_task
             WHERE lease_owner IS NOT NULL
               AND lease_expires_at_ms <= ?1
               AND status NOT IN ('succeeded', 'partial', 'skipped', 'retryable_failed', 'cancelled')
             ORDER BY lease_expires_at_ms, id",
        )?;
        let tasks = statement
            .query_map([now_ms], |row| {
                Ok(RecoverableTask {
                    task_id: row.get(0)?,
                    batch_id: row.get(1)?,
                    status: row.get(2)?,
                    previous_owner: row.get(3)?,
                    lease_expired_at_ms: row.get(4)?,
                    attempt: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }
}
