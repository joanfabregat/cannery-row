INSERT INTO users(id,issuer,subject) VALUES('00000000-0000-0000-0000-000000000001','fixture','jobs');
INSERT INTO projects(id,slug,title,created_by) VALUES('00000000-0000-0000-0000-000000000002','jobs-fixture','Jobs fixture','00000000-0000-0000-0000-000000000001');
INSERT INTO service_accounts(id,project_id,kind,name,created_by) VALUES('00000000-0000-0000-0000-000000000003','00000000-0000-0000-0000-000000000002','tester','tester','00000000-0000-0000-0000-000000000001');
INSERT INTO tracks(id,project_id,slug,title,created_by) VALUES('00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000002','main','Main','00000000-0000-0000-0000-000000000001');
INSERT INTO hypotheses(id,project_id,number,track_id,state,title,created_by_user,approved_revision,approved_at)
SELECT ('00000000-0000-0000-0000-'||lpad((10+i)::text,12,'0'))::uuid,'00000000-0000-0000-0000-000000000002',i,'00000000-0000-0000-0000-000000000004','active','Fixture','00000000-0000-0000-0000-000000000001',1,TIMESTAMPTZ '2024-01-02T03:04:05Z' FROM generate_series(1,3) i;
INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_user,via_channel,lease_generation)
SELECT ('00000000-0000-0000-0000-'||lpad((20+i)::text,12,'0'))::uuid,'00000000-0000-0000-0000-000000000002',('00000000-0000-0000-0000-'||lpad((10+i)::text,12,'0'))::uuid,1,CASE WHEN i=3 THEN 'evaluating' ELSE 'testing' END,1,1,'00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000001','api',0 FROM generate_series(1,3) i;
INSERT INTO manifests(id,attempt_id,stage,content,sha256) VALUES('00000000-0000-0000-0000-000000000031','00000000-0000-0000-0000-000000000021','tester','{}',repeat('0',64));
INSERT INTO evidence_records(id,project_id,attempt_id,stage,status,content,sha256,manifest_id,producer_user,via_channel) VALUES('00000000-0000-0000-0000-000000000032','00000000-0000-0000-0000-000000000002','00000000-0000-0000-0000-000000000021','tester','completed','{}',repeat('0',64),'00000000-0000-0000-0000-000000000031','00000000-0000-0000-0000-000000000001','api');
INSERT INTO jobs(id,project_id,attempt_id,stage,run_number,state,science_revision,tester_id,spec,deadline_seconds,claimed_by_service,via_channel,via_client,lease_generation,lease_token_hash,lease_expires_at,claimed_at,deadline,finished_at,error_code,error_step,error_reason,origin,previous_run_id)
SELECT ('00000000-0000-0000-0000-'||lpad((100+i)::text,12,'0'))::uuid,'00000000-0000-0000-0000-000000000002',('00000000-0000-0000-0000-'||lpad((CASE WHEN i=8 THEN 22 WHEN i=9 THEN 23 ELSE 21 END)::text,12,'0'))::uuid,
CASE WHEN i IN(7,9) THEN 'evaluator' ELSE 'tester' END,CASE WHEN i>=7 THEN 1 ELSE i END,
CASE WHEN i<=5 THEN 'failed' WHEN i=9 THEN 'claimed' ELSE 'pending' END,1,'fixture','{"evaluator":{"revision":"r1"},"label":"é😀","float":1.0}',600,
CASE WHEN i<=5 OR i=9 THEN '00000000-0000-0000-0000-000000000003'::uuid END,
CASE WHEN i<=5 OR i=9 THEN 'cli' END,CASE WHEN i<=5 OR i=9 THEN 'fixture' END,CASE WHEN i<=5 OR i=9 THEN 1 ELSE 0 END,
CASE WHEN i=9 THEN decode(repeat('07',32),'hex') END,CASE WHEN i=9 THEN TIMESTAMPTZ '2024-01-02T03:04:05Z'-interval '1 second' END,
CASE WHEN i<=5 OR i=9 THEN TIMESTAMPTZ '2024-01-02T03:04:05Z' END,CASE WHEN i<=5 OR i=9 THEN TIMESTAMPTZ '2024-01-02T03:04:05Z'+interval '600 seconds' END,CASE WHEN i<=5 THEN TIMESTAMPTZ '2024-01-02T03:04:05Z' END,
CASE WHEN i<=5 THEN 'seed_failure' END,
CASE WHEN i<=5 THEN 'fixture-step' END,CASE WHEN i<=5 THEN 'é reason' END,
CASE WHEN i IN(2,3,5) THEN 'auto_retry' WHEN i=4 THEN 'human_retry' ELSE 'submission' END,
CASE WHEN i IN(2,3,4,5) THEN ('00000000-0000-0000-0000-'||lpad((99+i)::text,12,'0'))::uuid END
FROM generate_series(1,9) i;
INSERT INTO jobs(id,project_id,attempt_id,stage,run_number,state,science_revision,tester_id,spec,deadline_seconds,claimed_by_service,deadline,finished_at,evidence_id,manifest_id)
VALUES ('00000000-0000-0000-0000-000000000114','00000000-0000-0000-0000-000000000002','00000000-0000-0000-0000-000000000021','tester',14,'completed',1,'fixture','{}',600,'00000000-0000-0000-0000-000000000003','2024-01-02Z','2024-01-02Z','00000000-0000-0000-0000-000000000032','00000000-0000-0000-0000-000000000031');
INSERT INTO uploads(id,attempt_id,job_id,lease_generation,token_hash,role,backend,bucket,key,declared_size,declared_sha256,media_type,state,expires_at,receiving_since,slot)
SELECT ('00000000-0000-0000-0000-'||lpad((200+i)::text,12,'0'))::uuid,'00000000-0000-0000-0000-000000000021','00000000-0000-0000-0000-000000000106',1,decode(lpad(i::text,64,'0'),'hex'),'result','s3','fixture','upload-'||i,CASE WHEN i IN(1,2) THEN 9223372036854775807 WHEN i=3 THEN 17 WHEN i=4 THEN 19 ELSE 23 END,repeat('0',64),'application/json',CASE WHEN i<=2 THEN 'verified' WHEN i IN(4,6) THEN 'receiving' WHEN i=7 THEN 'failed' ELSE 'pending' END,CASE WHEN i IN(5,6) THEN TIMESTAMPTZ '2024-01-02T03:04:05Z'-interval '1 second' ELSE TIMESTAMPTZ '2024-01-02T03:04:05Z'+interval '600 seconds' END,CASE WHEN i IN(4,6) THEN TIMESTAMPTZ '2024-01-02T03:04:05Z' END,'slot-'||i FROM generate_series(1,7) i;
