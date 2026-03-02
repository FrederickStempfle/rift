-- no-transaction
-- Add 'suspended' to the deployment_status enum for scale-to-zero persistence.
-- ALTER TYPE ... ADD VALUE cannot run inside a transaction, hence the no-transaction directive.
ALTER TYPE deployment_status ADD VALUE IF NOT EXISTS 'suspended' AFTER 'ready';
