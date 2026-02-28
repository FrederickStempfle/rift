DO $$ BEGIN
    CREATE TYPE firewall_mode AS ENUM ('allow_all', 'block_all');
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;

ALTER TABLE projects ADD COLUMN IF NOT EXISTS firewall_mode firewall_mode NOT NULL DEFAULT 'allow_all';

CREATE TABLE IF NOT EXISTS firewall_rules (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    cidr        INET NOT NULL,
    action      TEXT NOT NULL CHECK (action IN ('allow', 'block')),
    description TEXT NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_firewall_rules_project ON firewall_rules(project_id);
CREATE INDEX IF NOT EXISTS idx_firewall_rules_cidr ON firewall_rules USING gist (cidr inet_ops);
