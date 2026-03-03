CREATE TABLE IF NOT EXISTS services (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    service_type    TEXT NOT NULL DEFAULT 'supabase',
    name            TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending',
    config          JSONB NOT NULL DEFAULT '{}',
    connection_info JSONB,
    error_message   TEXT,
    started_at      TIMESTAMPTZ,
    stopped_at      TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_services_user_type ON services (user_id, service_type);
CREATE INDEX IF NOT EXISTS idx_services_user_id ON services (user_id);

CREATE TABLE IF NOT EXISTS service_logs (
    id          BIGSERIAL PRIMARY KEY,
    service_id  UUID NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    timestamp   TIMESTAMPTZ NOT NULL DEFAULT now(),
    level       TEXT NOT NULL DEFAULT 'info',
    message     TEXT NOT NULL,
    source      TEXT NOT NULL DEFAULT 'system'
);

CREATE INDEX IF NOT EXISTS idx_service_logs_service_id ON service_logs (service_id, id);
