INSERT INTO users(id,issuer,subject) VALUES('00000000-0000-0000-0000-000000000001','fixture','attempts');
INSERT INTO projects(id,slug,title,created_by) VALUES('00000000-0000-0000-0000-000000000002','attempts-fixture','Attempts fixture','00000000-0000-0000-0000-000000000001');
INSERT INTO service_accounts(id,project_id,kind,name,created_by) VALUES('00000000-0000-0000-0000-000000000003','00000000-0000-0000-0000-000000000002','agent','agent','00000000-0000-0000-0000-000000000001');
INSERT INTO tracks(id,project_id,slug,title,created_by,producer) VALUES('00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000002','main','Main','00000000-0000-0000-0000-000000000001','{"repo":"fixture","revision":1}');
INSERT INTO hypotheses(id,project_id,number,track_id,state,title,created_by_user,approved_revision,approved_at)
SELECT ('00000000-0000-0000-0000-'||lpad((10+i)::text,12,'0'))::uuid,'00000000-0000-0000-0000-000000000002',i,'00000000-0000-0000-0000-000000000004',CASE WHEN i IN (3,4) THEN 'queued' ELSE 'active' END,'Fixture','00000000-0000-0000-0000-000000000001',1,'2025-01-02T03:04:05.123456Z' FROM generate_series(1,4) i;
INSERT INTO hypothesis_revisions(hypothesis_id,revision,content,science_revision,author_user,via_channel)
SELECT id,1,'{"project_fields":{"threshold":3,"unicode":"é😀"},"control":{"id":"baseline","revision":1}}',1,'00000000-0000-0000-0000-000000000001','api' FROM hypotheses;
INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_user,via_channel,via_client,lease_generation,lease_token_hash,lease_expires_at,producer,workflow,deadline,claimed_at)
SELECT ('00000000-0000-0000-0000-'||lpad((20+i)::text,12,'0'))::uuid,'00000000-0000-0000-0000-000000000002',('00000000-0000-0000-0000-'||lpad((10+i)::text,12,'0'))::uuid,1,CASE WHEN i=1 THEN 'claimed' ELSE 'testing' END,1,1,'00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000001','api','legacy client',1,CASE WHEN i=1 THEN decode(repeat('07',32),'hex') END,CASE WHEN i=1 THEN now()+interval '60 seconds' END,'{"repo":"fixture","revision":1}',CASE WHEN i=1 THEN '{"steps":[]}'::jsonb END,CASE WHEN i=1 THEN now()+interval '600 seconds' END,'2025-01-02T03:04:05.123456Z' FROM generate_series(1,2) i;
INSERT INTO manifests(id,attempt_id,stage,content,sha256,created_at) VALUES('00000000-0000-0000-0000-000000000031','00000000-0000-0000-0000-000000000022','tester','{"value":1.0,"unicode":"é😀"}',repeat('0',64),'2025-01-02T03:04:05.123456Z');
INSERT INTO evidence_records(id,project_id,attempt_id,stage,status,content,sha256,manifest_id,producer_user,via_channel,created_at) VALUES('00000000-0000-0000-0000-000000000032','00000000-0000-0000-0000-000000000002','00000000-0000-0000-0000-000000000022','tester','completed','{"value":1.0,"unicode":"é😀"}',repeat('0',64),'00000000-0000-0000-0000-000000000031','00000000-0000-0000-0000-000000000001','api','2025-01-02T03:04:05.123456Z');
INSERT INTO jobs(id,project_id,attempt_id,stage,run_number,state,science_revision,tester_id,spec,deadline_seconds) VALUES('00000000-0000-0000-0000-000000000041','00000000-0000-0000-0000-000000000002','00000000-0000-0000-0000-000000000022','tester',1,'pending',1,'fixture','{}',600);
INSERT INTO uploads(id,attempt_id,job_id,lease_generation,token_hash,role,backend,bucket,key,declared_size,declared_sha256,media_type,state,expires_at,slot,interface)
SELECT ('00000000-0000-0000-0000-'||lpad((50+i)::text,12,'0'))::uuid,CASE WHEN i=1 THEN '00000000-0000-0000-0000-000000000021'::uuid ELSE '00000000-0000-0000-0000-000000000022'::uuid END,CASE WHEN i=2 THEN '00000000-0000-0000-0000-000000000041'::uuid END,1,decode(lpad(i::text,64,'0'),'hex'),'result','s3','fixture','upload-'||i,17,repeat('0',64),'application/json','pending',now()+interval '60 seconds','upload-'||i,CASE WHEN i=2 THEN 'metrics' END FROM generate_series(1,2) i;
INSERT INTO artifacts(id,project_id,attempt_id,role,backend,bucket,key,size_bytes,sha256,media_type,verified_at) VALUES('00000000-0000-0000-0000-000000000061','00000000-0000-0000-0000-000000000002','00000000-0000-0000-0000-000000000021','legacy role','s3','fixture','artifact-existing',17,repeat('0',64),'application/json','2025-01-02T03:04:05.123456Z');

INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_user,via_channel,lease_generation,claimed_at)
VALUES('00000000-0000-0000-0000-000000000024','00000000-0000-0000-0000-000000000002','00000000-0000-0000-0000-000000000014',1,'failed',1,1,'00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000001','legacy channel',0,'2025-01-02T03:04:05.123456Z');
INSERT INTO attempt_failures(id,attempt_id,stage,code,reason,details,log_refs,created_at,requeued) VALUES
('00000000-0000-0000-0000-000000000071','00000000-0000-0000-0000-000000000024','tester','baseline_failure','first','null','[1,"legacy"]','2025-01-03T03:04:05.123456Z',false),
('00000000-0000-0000-0000-000000000072','00000000-0000-0000-0000-000000000024','agent','automatic_failure','second','[1,2]','[]','2025-01-04T03:04:05.123456Z',true);
INSERT INTO artifacts(id,project_id,attempt_id,job_id,role,backend,bucket,key,size_bytes,sha256,media_type,verified_at,interface,content_validated) VALUES('00000000-0000-0000-0000-000000000062','00000000-0000-0000-0000-000000000002','00000000-0000-0000-0000-000000000022','00000000-0000-0000-0000-000000000041','result','s3','fixture','artifact-job',19,repeat('0',64),'application/json','2025-01-02T03:04:05.123456Z','metrics',true);

CREATE TABLE fixture_counter(n integer NOT NULL);
INSERT INTO fixture_counter VALUES(1000);
CREATE FUNCTION fixture_identity() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE n integer;
BEGIN
IF TG_OP='INSERT' OR NEW.id<>OLD.id THEN
 UPDATE fixture_counter SET n=fixture_counter.n+1 RETURNING fixture_counter.n INTO n;
 NEW.id=('00000000-0000-0000-0000-'||lpad(n::text,12,'0'))::uuid;
END IF;
IF TG_OP='UPDATE' AND NEW.id<>OLD.id THEN NEW.created_at='2025-01-02T03:04:05.123456Z'; END IF;
IF TG_OP='INSERT' THEN
 CASE TG_TABLE_NAME
 WHEN 'attempts' THEN NEW.claimed_at='2025-01-02T03:04:05.123456Z';
 WHEN 'artifacts' THEN NEW.verified_at='2025-01-02T03:04:05.123456Z';
 WHEN 'uploads' THEN NEW.created_at='2025-01-02T03:04:05.123456Z';
 WHEN 'review_cases' THEN NEW.opened_at='2025-01-02T03:04:05.123456Z';
 ELSE NEW.created_at='2025-01-02T03:04:05.123456Z';
 END CASE;
END IF;
RETURN NEW;
END $$;
CREATE TRIGGER fixture_identity BEFORE INSERT ON attempts FOR EACH ROW EXECUTE FUNCTION fixture_identity();
CREATE TRIGGER fixture_identity BEFORE INSERT OR UPDATE ON uploads FOR EACH ROW EXECUTE FUNCTION fixture_identity();
CREATE TRIGGER fixture_identity BEFORE INSERT ON artifacts FOR EACH ROW EXECUTE FUNCTION fixture_identity();
CREATE TRIGGER fixture_identity BEFORE INSERT ON manifests FOR EACH ROW EXECUTE FUNCTION fixture_identity();
CREATE TRIGGER fixture_identity BEFORE INSERT ON evidence_records FOR EACH ROW EXECUTE FUNCTION fixture_identity();
CREATE TRIGGER fixture_identity BEFORE INSERT ON attempt_failures FOR EACH ROW EXECUTE FUNCTION fixture_identity();
CREATE TRIGGER fixture_identity BEFORE INSERT ON review_cases FOR EACH ROW EXECUTE FUNCTION fixture_identity();
-- Deterministic isolated seed creation times, before repository operations.
UPDATE hypotheses SET created_at='2025-01-02T03:04:05.123456Z';
UPDATE uploads SET created_at='2025-01-02T03:04:05.123456Z';
