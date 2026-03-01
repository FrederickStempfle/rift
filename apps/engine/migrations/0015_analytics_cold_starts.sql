-- Track cold start counts per analytics bucket for pool mode observability.
ALTER TABLE analytics_hourly ADD COLUMN IF NOT EXISTS cold_starts BIGINT NOT NULL DEFAULT 0;
