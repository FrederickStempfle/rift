CREATE TABLE IF NOT EXISTS env_vars (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    encrypted_value BYTEA NOT NULL,
    nonce BYTEA NOT NULL,
    CONSTRAINT env_vars_project_key_unique UNIQUE (project_id, key)
);

CREATE INDEX IF NOT EXISTS idx_env_vars_project_id ON env_vars(project_id);

CREATE TABLE IF NOT EXISTS domains (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    domain TEXT NOT NULL UNIQUE,
    is_primary BOOLEAN NOT NULL DEFAULT false,
    ssl_status ssl_status NOT NULL DEFAULT 'pending'
);

CREATE INDEX IF NOT EXISTS idx_domains_project_id ON domains(project_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_domains_primary_per_project
    ON domains(project_id)
    WHERE is_primary = true;
