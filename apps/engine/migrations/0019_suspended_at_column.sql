-- Track when a deployment was suspended (scale-to-zero).
ALTER TABLE deployments ADD COLUMN IF NOT EXISTS suspended_at TIMESTAMPTZ;
