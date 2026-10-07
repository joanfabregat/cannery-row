-- Execute after the frozen five-operation fixture seed. All changes are isolated.
UPDATE projects SET next_hypothesis_number=30;
INSERT INTO config_revisions(project_id,kind,revision,content,created_by,created_at)
SELECT id,'science',1,'{"metrics":[{"key":"ndcg","splits":["dev","test"],"dimensions":[]}],"baselines":[{"id":"bm25","revision":"v1"}],"hypothesis_fields":{"type":"object","properties":{"architecture":{"type":"string"}},"required":["architecture"],"additionalProperties":false}}','00000000-0000-0000-0000-000000000001','2001-01-01Z' FROM projects;
INSERT INTO tracks(id,project_id,slug,title,state,created_by,created_at,updated_at) VALUES
('00000000-0000-0000-0000-000000000032','00000000-0000-0000-0000-000000000010','paused','Paused','paused','00000000-0000-0000-0000-000000000001','2001-01-01Z','2001-01-01Z'),
('00000000-0000-0000-0000-000000000033','00000000-0000-0000-0000-000000000010','archived','Archived','archived','00000000-0000-0000-0000-000000000001','2001-01-01Z','2001-01-01Z');
INSERT INTO service_accounts(id,project_id,kind,name,created_by) VALUES
('00000000-0000-0000-0000-000000000022','00000000-0000-0000-0000-000000000010','agent','another-agent','00000000-0000-0000-0000-000000000001'),
('00000000-0000-0000-0000-000000000023','00000000-0000-0000-0000-000000000010','tester','tester','00000000-0000-0000-0000-000000000001');
INSERT INTO api_tokens(token_hash,display_prefix,kind,service_account_id,name,scopes,expires_at) SELECT sha256(convert_to('cr_svc_track_http_'||role,'UTF8')),'cr_svc_fixture','service',('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,role,ARRAY['read','write'],now()+interval '1 day' FROM (VALUES(22,'otheragent'),(23,'testeragent')) AS u(i,role);
-- Fixed identity fixtures run BEFORE actual production INSERT, without rewriting
-- an observed value. Current operational clock fields remain actual PG now().
CREATE FUNCTION fixture_hypothesis_identity() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
IF NEW.project_id='00000000-0000-0000-0000-000000000010' AND NEW.number>=30 THEN
NEW.id=('40000000-0000-4000-8000-'||lpad(NEW.number::text,12,'0'))::uuid;
END IF; RETURN NEW; END $$;
CREATE TRIGGER fixture_hypothesis_identity BEFORE INSERT ON hypotheses FOR EACH ROW EXECUTE FUNCTION fixture_hypothesis_identity();
CREATE FUNCTION fixture_case_identity() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE n int; BEGIN
SELECT number INTO n FROM hypotheses WHERE id=NEW.hypothesis_id;
IF NEW.project_id='00000000-0000-0000-0000-000000000010' THEN
NEW.id=('50000000-0000-4000-8000-'||lpad((n*100000+NEW.subject_revision)::text,12,'0'))::uuid;
END IF; RETURN NEW; END $$;
CREATE TRIGGER fixture_case_identity BEFORE INSERT ON review_cases FOR EACH ROW EXECUTE FUNCTION fixture_case_identity();
