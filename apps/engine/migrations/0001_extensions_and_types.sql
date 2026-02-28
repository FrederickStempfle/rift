CREATE EXTENSION IF NOT EXISTS pgcrypto;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'framework_type') THEN
        CREATE TYPE framework_type AS ENUM (
            'nextjs',
            'vite',
            'remix',
            'astro',
            'svelte',
            'static',
            'unknown'
        );
    END IF;
END $$;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'deployment_status') THEN
        CREATE TYPE deployment_status AS ENUM (
            'queued',
            'cloning',
            'building',
            'deploying',
            'ready',
            'failed',
            'cancelled'
        );
    END IF;
END $$;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'ssl_status') THEN
        CREATE TYPE ssl_status AS ENUM (
            'pending',
            'provisioning',
            'active',
            'failed'
        );
    END IF;
END $$;
