ALTER TABLE users
    ADD COLUMN IF NOT EXISTS github_id TEXT UNIQUE,
    ADD COLUMN IF NOT EXISTS github_login TEXT,
    ADD COLUMN IF NOT EXISTS display_name TEXT,
    ADD COLUMN IF NOT EXISTS avatar_url TEXT;

CREATE INDEX IF NOT EXISTS idx_users_github_id ON users(github_id);
