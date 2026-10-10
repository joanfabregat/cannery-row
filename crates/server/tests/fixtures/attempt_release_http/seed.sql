ALTER TABLE attempts DROP CONSTRAINT IF EXISTS attempts_imported_check;
ALTER TABLE attempts ADD CONSTRAINT attempts_imported_check CHECK (imported IS NULL OR jsonb_typeof(imported)='object' OR id='00000000-0000-0000-0000-000000002001');
-- Historical exception is limited to the one release fixture attempt.
ALTER TABLE attempts DROP CONSTRAINT IF EXISTS attempts_workflow_check;
ALTER TABLE attempts ADD CONSTRAINT attempts_workflow_check CHECK (workflow IS NULL OR jsonb_typeof(workflow)='object' OR id='00000000-0000-0000-0000-000000002001');
-- Isolated deterministic serialization preconditions, never installed by the API.
CREATE OR REPLACE FUNCTION fixture_release_failure_id() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN
IF NEW.attempt_id='00000000-0000-0000-0000-000000002001' THEN
 IF (SELECT phase='failure' FROM fixture_release_fault) THEN RAISE EXCEPTION 'isolated failure insert fault'; END IF;
 NEW.id='00000000-0000-0000-0000-000000006001'; NEW.created_at='2001-01-01Z'; END IF; RETURN NEW; END$$;
DROP TRIGGER IF EXISTS fixture_release_failure_id ON attempt_failures;
CREATE TRIGGER fixture_release_failure_id BEFORE INSERT ON attempt_failures FOR EACH ROW EXECUTE FUNCTION fixture_release_failure_id();
CREATE OR REPLACE FUNCTION fixture_release_review_id() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN
IF NEW.attempt_id='00000000-0000-0000-0000-000000002001' THEN NEW.id='00000000-0000-0000-0000-000000007001'; NEW.opened_at='2001-01-01Z'; END IF; RETURN NEW; END$$;
DROP TRIGGER IF EXISTS fixture_release_review_id ON review_cases;
CREATE TRIGGER fixture_release_review_id BEFORE INSERT ON review_cases FOR EACH ROW EXECUTE FUNCTION fixture_release_review_id();
CREATE TABLE IF NOT EXISTS fixture_release_fault(phase text);
TRUNCATE fixture_release_fault;
INSERT INTO fixture_release_fault VALUES(NULL);
CREATE OR REPLACE FUNCTION fixture_release_audit() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN
IF NEW.action IN ('attempt.failed','unit.requeued') THEN
 IF (SELECT phase='audit' OR (phase='requeue_audit' AND NEW.action='unit.requeued') FROM fixture_release_fault) THEN RAISE EXCEPTION 'isolated release audit fault'; END IF;
 NEW.occurred_at='2001-01-01Z';
END IF; RETURN NEW; END$$;
DROP TRIGGER IF EXISTS fixture_release_audit ON audit_events;
CREATE TRIGGER fixture_release_audit BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION fixture_release_audit();
UPDATE attempts SET state='claimed',claimed_by_user='00000000-0000-0000-0000-000000000002',claimed_by_service=NULL,lease_token_hash=sha256(convert_to('cr_lease_fixture','UTF8')),lease_expires_at='2099-01-01Z',lease_generation=1,started_at=NULL,deadline=NULL,origin='live',source_ref=NULL,imported=NULL,workflow=NULL WHERE id='00000000-0000-0000-0000-000000002001';
UPDATE units SET state='active',approved_revision=1,approved_at='2001-01-01Z' WHERE id='00000000-0000-0000-0000-000000001001';
INSERT INTO config_revisions(project_id,kind,revision,content,created_by,created_at) VALUES('00000000-0000-0000-0000-000000000010','science',3,'{}','00000000-0000-0000-0000-000000000001','2001-01-01Z');
INSERT INTO artifacts(id,project_id,attempt_id,role,backend,bucket,key,size_bytes,sha256,media_type,verified_at) VALUES('00000000-0000-0000-0000-000000008001','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002001','logs','local','fixture','logs/é😀',5,repeat('a',64),'text/plain','2001-01-01Z');
UPDATE search_documents SET updated_at='2001-01-01Z';
-- One explicitly isolated historical row can expose late response model failures.
INSERT INTO jobs(id,project_id,attempt_id,phase,run_number,science_revision,performer,verifier_id,spec,deadline_seconds,created_at) VALUES('00000000-0000-0000-0000-000000009001','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002001','verify',1,3,'runner','fixture','{}',60,'2001-01-01Z');
INSERT INTO artifacts(id,project_id,attempt_id,job_id,role,backend,bucket,key,size_bytes,sha256,media_type,verified_at) VALUES('00000000-0000-0000-0000-000000008002','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002001','00000000-0000-0000-0000-000000009001','logs','local','fixture','job/logs',5,repeat('a',64),'text/plain','2001-01-01Z');
INSERT INTO sessions(secret_hash,user_id,csrf_token,created_at,expires_at,last_seen_at) VALUES(sha256(convert_to('cr_sess_release_fixture','UTF8')),'00000000-0000-0000-0000-000000000002','release-csrf','2001-01-01Z','2099-01-01Z','2001-01-01Z');
CREATE OR REPLACE FUNCTION fixture_release_commit() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN
 IF NEW.id='00000000-0000-0000-0000-000000002001' AND NEW.state='failed' AND OLD.state IN ('claimed','running') AND (SELECT phase='commit' FROM fixture_release_fault) THEN RAISE EXCEPTION 'isolated release commit fault'; END IF; RETURN NEW; END$$;
DROP TRIGGER IF EXISTS fixture_release_commit ON attempts;
CREATE CONSTRAINT TRIGGER fixture_release_commit AFTER UPDATE ON attempts DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION fixture_release_commit();
