-- Make subdomain optional so projects can be created without one.
-- Projects without a subdomain are accessed via IP or custom domain only.
-- The UNIQUE constraint is kept — NULL values don't conflict in PostgreSQL.
ALTER TABLE projects ALTER COLUMN subdomain DROP NOT NULL;
