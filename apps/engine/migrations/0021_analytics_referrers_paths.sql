-- Track traffic sources (referrer domains) per hourly bucket.
CREATE TABLE IF NOT EXISTS analytics_referrers (
    project_id  UUID         NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    bucket      TIMESTAMPTZ  NOT NULL,
    referrer    TEXT         NOT NULL,
    requests    BIGINT       NOT NULL DEFAULT 0,
    PRIMARY KEY (project_id, bucket, referrer)
);

CREATE INDEX IF NOT EXISTS idx_analytics_referrers_bucket
    ON analytics_referrers (project_id, bucket DESC);

-- Track request paths per hourly bucket.
CREATE TABLE IF NOT EXISTS analytics_paths (
    project_id  UUID         NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    bucket      TIMESTAMPTZ  NOT NULL,
    path        TEXT         NOT NULL,
    requests    BIGINT       NOT NULL DEFAULT 0,
    errors      BIGINT       NOT NULL DEFAULT 0,
    total_ms    BIGINT       NOT NULL DEFAULT 0,
    PRIMARY KEY (project_id, bucket, path)
);

CREATE INDEX IF NOT EXISTS idx_analytics_paths_bucket
    ON analytics_paths (project_id, bucket DESC);
