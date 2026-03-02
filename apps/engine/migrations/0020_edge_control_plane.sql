-- Edge control-plane and regional routing primitives.

CREATE TYPE deploy_release_state AS ENUM ('packaged', 'staged', 'promoted', 'rollback');

CREATE TABLE regions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE edge_nodes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    region_id UUID NOT NULL REFERENCES regions(id) ON DELETE CASCADE,
    addr TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    capacity INTEGER NOT NULL DEFAULT 0,
    last_heartbeat_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX edge_nodes_region_id_idx ON edge_nodes(region_id);
CREATE INDEX edge_nodes_status_idx ON edge_nodes(status);

CREATE TABLE deploy_artifacts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL UNIQUE REFERENCES deployments(id) ON DELETE CASCADE,
    digest TEXT NOT NULL,
    size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0),
    manifest_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    signed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX deploy_artifacts_deployment_id_idx ON deploy_artifacts(deployment_id);
CREATE INDEX deploy_artifacts_digest_idx ON deploy_artifacts(digest);

CREATE TABLE deploy_releases (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    deployment_id UUID NOT NULL UNIQUE REFERENCES deployments(id) ON DELETE CASCADE,
    artifact_id UUID NOT NULL REFERENCES deploy_artifacts(id) ON DELETE RESTRICT,
    version BIGINT NOT NULL CHECK (version > 0),
    state deploy_release_state NOT NULL DEFAULT 'packaged',
    promoted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, version)
);

CREATE INDEX deploy_releases_project_id_idx ON deploy_releases(project_id);
CREATE INDEX deploy_releases_state_idx ON deploy_releases(state);

CREATE TABLE route_bindings (
    host TEXT PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    release_id UUID NOT NULL REFERENCES deploy_releases(id) ON DELETE CASCADE,
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX route_bindings_project_id_idx ON route_bindings(project_id);
CREATE INDEX route_bindings_release_id_idx ON route_bindings(release_id);

CREATE TABLE release_rollouts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    release_id UUID NOT NULL REFERENCES deploy_releases(id) ON DELETE CASCADE,
    strategy TEXT NOT NULL,
    percent INTEGER NOT NULL CHECK (percent >= 0 AND percent <= 100),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ
);

CREATE INDEX release_rollouts_release_id_idx ON release_rollouts(release_id);
