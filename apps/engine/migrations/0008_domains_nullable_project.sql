-- Domains can exist without a project (verify DNS first, assign later).
ALTER TABLE domains ALTER COLUMN project_id DROP NOT NULL;

-- Track who created the domain (needed for ownership when project_id is NULL).
ALTER TABLE domains ADD COLUMN created_by UUID REFERENCES users(id);

-- Backfill created_by from the project owner for existing rows.
UPDATE domains d
SET created_by = p.user_id
FROM projects p
WHERE d.project_id = p.id AND d.created_by IS NULL;

-- The old primary-per-project index only makes sense when project_id is set.
DROP INDEX IF EXISTS idx_domains_primary_per_project;
CREATE UNIQUE INDEX idx_domains_primary_per_project
    ON domains(project_id)
    WHERE is_primary = true AND project_id IS NOT NULL;
