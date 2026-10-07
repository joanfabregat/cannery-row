-- Isolated recovery seed after attempt_reads_http; all frozen identities and specifications.
INSERT INTO service_accounts(id,project_id,kind,name,created_by) VALUES
('00000000-0000-0000-0000-000000000023','00000000-0000-0000-0000-000000000010','tester','fixture-tester','00000000-0000-0000-0000-000000000001'),
('00000000-0000-0000-0000-000000000024','00000000-0000-0000-0000-000000000010','evaluator','fixture-evaluator','00000000-0000-0000-0000-000000000001'),
('00000000-0000-0000-0000-000000000025','00000000-0000-0000-0000-000000000010','tester','other-tester','00000000-0000-0000-0000-000000000001'),
('00000000-0000-0000-0000-000000000026','00000000-0000-0000-0000-000000000011','tester','foreign-tester','00000000-0000-0000-0000-000000000001');
INSERT INTO api_tokens(token_hash,display_prefix,kind,service_account_id,name,scopes,expires_at)
SELECT sha256(convert_to('cr_svc_track_http_'||role,'UTF8')),'cr_svc_fixture','service',('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,role,ARRAY['read','write'],'2099-01-01Z'
FROM (VALUES(23,'tester'),(24,'evaluator'),(25,'other-tester'),(26,'foreign-tester')) AS u(i,role);
INSERT INTO api_tokens(token_hash,display_prefix,kind,service_account_id,name,scopes,expires_at) VALUES(sha256(convert_to('cr_svc_track_http_tester-readonly','UTF8')),'cr_svc_fixture','service','00000000-0000-0000-0000-000000000023','tester-readonly',ARRAY['read'],'2099-01-01Z');
INSERT INTO jobs(id,project_id,attempt_id,stage,run_number,science_revision,tester_id,spec,deadline_seconds,created_at) VALUES
('00000000-0000-0000-0000-000000006005','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002005','tester',1,3,'fixture-tester','{"tester":{"id":"fixture-tester"},"track":"track-4","steps":[],"inputs":{"baselines":[{"id":"alpha","revision":"1"}],"datasets":[],"claimed_sheet":{"ref":"sheet","sha256":"abc"},"manifest":{"ref":"manifest","sha256":"def"}},"control":null,"parameters":{"wide":123456789012345678901234567890},"limits":{"max_output_bytes":9223372036854775807},"output_prefix":"jobs/é😀"}',600,'2001-01-01Z'),
('00000000-0000-0000-0000-000000006006','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002006','evaluator',1,3,'fixture-evaluator','{"evaluator":{"id":"fixture-evaluator","revision":"policy-1"},"track":"track-4","inputs":{"baselines":[],"datasets":[],"evidence":[{"ref":"evidence","sha256":"abc"}],"manifest":{"ref":"manifest","sha256":"def"}},"control":{"id":"opaque","revision":"legacy"},"parameters":{"fraction":1.5,"nullable":null,"wide":123456789012345678901234567890},"limits":{"max_output_bytes":1000},"output_prefix":"eval/é😀"}',600,'2001-01-01Z');
CREATE TABLE fixture_job_fault(phase text);
INSERT INTO fixture_job_fault VALUES(NULL);
CREATE FUNCTION fixture_job_update_fault() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE phase text;
BEGIN
SELECT f.phase INTO phase FROM fixture_job_fault f;
IF phase='update' THEN RAISE EXCEPTION 'isolated job mutation failure';
ELSIF phase='fixed' THEN NEW.claimed_at='2099-01-01Z';NEW.lease_expires_at='2099-01-01T00:01:30Z';NEW.deadline='2099-01-01T00:10:00Z';
END IF;
RETURN NEW;
END $$;
CREATE TRIGGER fixture_job_update_fault BEFORE UPDATE ON jobs FOR EACH ROW EXECUTE FUNCTION fixture_job_update_fault();
CREATE FUNCTION fixture_job_audit_fault() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN IF EXISTS(SELECT 1 FROM fixture_job_fault WHERE phase='audit') THEN RAISE EXCEPTION 'isolated job audit failure';END IF;RETURN NEW;END $$;
CREATE TRIGGER fixture_job_audit_fault BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION fixture_job_audit_fault();
CREATE FUNCTION fixture_job_commit_fault() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN IF EXISTS(SELECT 1 FROM fixture_job_fault WHERE phase='commit') THEN RAISE EXCEPTION 'isolated deferred job failure';END IF;RETURN NEW;END $$;
CREATE CONSTRAINT TRIGGER fixture_job_commit_fault AFTER UPDATE ON jobs DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION fixture_job_commit_fault();
UPDATE search_documents SET updated_at='2001-01-01Z';
