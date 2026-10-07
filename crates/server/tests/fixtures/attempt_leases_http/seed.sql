-- Execute after the unchanged attempt-read seed in an isolated migrated child.
UPDATE attempts SET lease_token_hash=sha256(convert_to('cr_lease_fixture','UTF8')) WHERE id='00000000-0000-0000-0000-000000002001';
INSERT INTO service_accounts(id,project_id,kind,name,created_by) VALUES
('00000000-0000-0000-0000-000000000022','00000000-0000-0000-0000-000000000010','experimenter','fixture-experimenter','00000000-0000-0000-0000-000000000001'),
('00000000-0000-0000-0000-000000000023','00000000-0000-0000-0000-000000000010','tester','fixture-tester','00000000-0000-0000-0000-000000000001'),
('00000000-0000-0000-0000-000000000024','00000000-0000-0000-0000-000000000010','evaluator','fixture-evaluator','00000000-0000-0000-0000-000000000001');
INSERT INTO api_tokens(token_hash,display_prefix,kind,service_account_id,name,scopes,expires_at)
SELECT sha256(convert_to('cr_svc_track_http_'||role,'UTF8')),'cr_svc_fixture','service',('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,role,ARRAY['read','write'],now()+interval '1 day'
FROM (VALUES(22,'experimenter'),(23,'tester'),(24,'evaluator')) AS u(i,role);
UPDATE search_documents SET updated_at='2001-01-01Z';
-- Recovery fault controls affect only this fixture's existing attempt.
CREATE TABLE fixture_lease_fault(phase text);
INSERT INTO fixture_lease_fault VALUES(NULL);
CREATE FUNCTION fixture_lease_update_fault() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE phase text;
BEGIN
IF NEW.id='00000000-0000-0000-0000-000000002001' AND OLD.state='claimed' AND NEW.state='running' THEN
SELECT f.phase INTO phase FROM fixture_lease_fault f;
IF phase='extend' THEN RAISE EXCEPTION 'isolated extension failure';
ELSIF phase='renewed_decode' THEN UPDATE attempts SET claimed_at='infinity' WHERE id=NEW.id;
END IF;
END IF;
RETURN NEW;
END $$;
CREATE TRIGGER fixture_lease_update_fault AFTER UPDATE ON attempts FOR EACH ROW EXECUTE FUNCTION fixture_lease_update_fault();
CREATE FUNCTION fixture_lease_commit_fault() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
IF NEW.id='00000000-0000-0000-0000-000000002001' AND OLD.state='claimed' AND NEW.state='running'
AND EXISTS(SELECT 1 FROM fixture_lease_fault WHERE phase='commit') THEN
RAISE EXCEPTION 'isolated deferred commit failure';
END IF;
RETURN NEW;
END $$;
CREATE CONSTRAINT TRIGGER fixture_lease_commit_fault AFTER UPDATE ON attempts DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION fixture_lease_commit_fault();
