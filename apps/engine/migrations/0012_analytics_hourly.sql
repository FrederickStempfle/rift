CREATE TABLE IF NOT EXISTS analytics_hourly (
    project_id  UUID         NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    bucket      TIMESTAMPTZ  NOT NULL,
    requests    BIGINT       NOT NULL DEFAULT 0,
    errors      BIGINT       NOT NULL DEFAULT 0,
    total_ms    BIGINT       NOT NULL DEFAULT 0,
    PRIMARY KEY (project_id, bucket)
);

CREATE INDEX IF NOT EXISTS idx_analytics_hourly_bucket
    ON analytics_hourly (project_id, bucket DESC);
