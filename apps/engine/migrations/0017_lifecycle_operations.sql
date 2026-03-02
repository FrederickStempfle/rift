-- Lifecycle operation tracking for idempotent deploy/wake/suspend/stop.
-- Replaying a command with the same op_id returns the prior result
-- without re-applying side effects.

CREATE TABLE IF NOT EXISTS lifecycle_operations (
    op_id       UUID PRIMARY KEY,
    action      TEXT NOT NULL,            -- 'deploy', 'wake', 'suspend', 'stop'
    project_id  UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    deployment_id UUID,                    -- nullable (e.g. suspend/stop may not target a specific deployment)
    status      TEXT NOT NULL DEFAULT 'pending',  -- 'pending', 'running', 'completed', 'failed'
    result      JSONB,                     -- success payload (e.g. url, port)
    error       TEXT,                      -- error message on failure
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_lifecycle_ops_project ON lifecycle_operations(project_id);
CREATE INDEX IF NOT EXISTS idx_lifecycle_ops_status ON lifecycle_operations(status) WHERE status IN ('pending', 'running');
