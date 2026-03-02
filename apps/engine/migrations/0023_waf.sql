-- WAF policies: scope defaults, mode, fail-open/fail-closed
CREATE TABLE waf_policies (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  UUID REFERENCES projects(id) ON DELETE CASCADE,
    mode        TEXT NOT NULL DEFAULT 'active',  -- 'active', 'log_only', 'disabled'
    fail_open   BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Unique constraint: one policy per project, plus one global (NULL project_id).
CREATE UNIQUE INDEX uq_waf_policies_scope
    ON waf_policies (COALESCE(project_id, '00000000-0000-0000-0000-000000000000'));

-- WAF rules: scope, matcher fields, action, priority, enabled
CREATE TABLE waf_rules (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  UUID REFERENCES projects(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    match_field TEXT NOT NULL,  -- 'ip', 'method', 'host', 'path', 'query', 'user_agent', 'header'
    match_op    TEXT NOT NULL,  -- 'exact', 'prefix', 'contains', 'regex', 'cidr'
    match_value TEXT NOT NULL,
    header_name TEXT,           -- for header field matching
    action      TEXT NOT NULL,  -- 'allow', 'challenge', 'block', 'log'
    priority    INTEGER NOT NULL DEFAULT 100,
    enabled     BOOLEAN NOT NULL DEFAULT true,
    is_managed  BOOLEAN NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_waf_rules_scope ON waf_rules (project_id, enabled, priority, created_at);
CREATE INDEX idx_waf_rules_global ON waf_rules (enabled, priority, created_at) WHERE project_id IS NULL;

-- WAF events: timestamp, action, sample request metadata
CREATE TABLE waf_events (
    id          BIGSERIAL PRIMARY KEY,
    project_id  UUID REFERENCES projects(id) ON DELETE SET NULL,
    rule_id     UUID REFERENCES waf_rules(id) ON DELETE SET NULL,
    action      TEXT NOT NULL,
    client_ip   TEXT NOT NULL,
    method      TEXT NOT NULL DEFAULT '',
    host        TEXT NOT NULL DEFAULT '',
    path        TEXT NOT NULL DEFAULT '',
    user_agent  TEXT NOT NULL DEFAULT '',
    rule_name   TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_waf_events_project ON waf_events (project_id, created_at DESC);
CREATE INDEX idx_waf_events_created ON waf_events (created_at DESC);

-- Insert default global policy (active, fail-open)
INSERT INTO waf_policies (project_id, mode, fail_open)
VALUES (NULL, 'active', true);
