-- Allow domains to route directly to a service (e.g. Supabase Kong/Studio)
-- instead of going through project_id -> RuntimeBackend resolution.

ALTER TABLE domains ADD COLUMN service_id UUID REFERENCES services(id) ON DELETE SET NULL;
ALTER TABLE domains ADD COLUMN target_url TEXT;

-- A domain can target a project OR a service, not both
ALTER TABLE domains ADD CONSTRAINT domains_single_target
  CHECK (NOT (project_id IS NOT NULL AND service_id IS NOT NULL));
