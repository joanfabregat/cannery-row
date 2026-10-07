-- Only isolated fixture objects are replaced between independent scenarios.
DROP TRIGGER IF EXISTS fixture_job_audit_fault ON audit_events;
DROP FUNCTION IF EXISTS fixture_job_audit_fault();
DROP FUNCTION IF EXISTS fixture_job_update_fault() CASCADE;
DROP FUNCTION IF EXISTS fixture_job_commit_fault() CASCADE;
DROP TABLE IF EXISTS fixture_job_fault;
TRUNCATE users,projects,idempotency_keys RESTART IDENTITY CASCADE;
