-- Track serverless function routes per project.
-- Populated during the build pipeline when rift/functions/ is detected.
CREATE TABLE IF NOT EXISTS functions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    deployment_id UUID NOT NULL REFERENCES deployments(id) ON DELETE CASCADE,
    route_pattern TEXT NOT NULL,
    entry_file TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(project_id, route_pattern)
);

CREATE INDEX IF NOT EXISTS idx_functions_project_id ON functions(project_id);
CREATE INDEX IF NOT EXISTS idx_functions_deployment_id ON functions(deployment_id);

-- Add runtime_mode columns to projects and deployments for pool/process tracking.
ALTER TABLE projects ADD COLUMN IF NOT EXISTS runtime_mode TEXT NOT NULL DEFAULT 'process';
ALTER TABLE deployments ADD COLUMN IF NOT EXISTS runtime_mode TEXT NOT NULL DEFAULT 'process';
