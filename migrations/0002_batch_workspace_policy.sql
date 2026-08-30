-- FR-011: the workspace policy is migration state, not a UI preference. It has
-- to survive a crash, because the retry of a failed batch must know whether the
-- operator asked for failed mirrors to be retained.
ALTER TABLE batch
    ADD COLUMN workspace_policy TEXT NOT NULL DEFAULT 'reuse'
    CHECK (workspace_policy IN ('reuse', 'clean'));
