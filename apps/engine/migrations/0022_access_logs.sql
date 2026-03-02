CREATE TABLE IF NOT EXISTS access_logs (
    id BIGSERIAL PRIMARY KEY,
    project_id UUID REFERENCES projects(id) ON DELETE CASCADE,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT now(),
    client_ip TEXT NOT NULL,
    host TEXT,
    method TEXT NOT NULL,
    path TEXT NOT NULL,
    status INTEGER NOT NULL CHECK (status >= 100 AND status <= 599),
    duration_ms BIGINT NOT NULL CHECK (duration_ms >= 0)
);

CREATE INDEX IF NOT EXISTS idx_access_logs_project_id_id_desc
    ON access_logs (project_id, id DESC);

CREATE INDEX IF NOT EXISTS idx_access_logs_timestamp_desc
    ON access_logs (timestamp DESC);
