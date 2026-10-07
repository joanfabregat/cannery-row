INSERT INTO users(id,issuer,subject) VALUES('00000000-0000-0000-0000-000000000001','fixture','jobs');
INSERT INTO projects(id,slug,title,created_by) VALUES('00000000-0000-0000-0000-000000000002','jobs-fixture','Jobs fixture','00000000-0000-0000-0000-000000000001');
INSERT INTO service_accounts(id,project_id,kind,name,created_by) VALUES('00000000-0000-0000-0000-000000000003','00000000-0000-0000-0000-000000000002','tester','tester','00000000-0000-0000-0000-000000000001');
INSERT INTO tracks(id,project_id,slug,title,created_by) VALUES('00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000002','main','Main','00000000-0000-0000-0000-000000000001');
INSERT INTO hypotheses(id,project_id,number,track_id,state,title,created_by_user,approved_revision,approved_at)
SELECT ('00000000-0000-0000-0000-'||lpad((10+i)::text,12,'0'))::uuid,'00000000-0000-0000-0000-000000000002',i,'00000000-0000-0000-0000-000000000004','active','Fixture','00000000-0000-0000-0000-000000000001',1,now() FROM generate_series(1,3) i;
INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_user,via_channel,lease_generation)
SELECT ('00000000-0000-0000-0000-'||lpad((20+i)::text,12,'0'))::uuid,'00000000-0000-0000-0000-000000000002',('00000000-0000-0000-0000-'||lpad((10+i)::text,12,'0'))::uuid,1,CASE WHEN i=3 THEN 'evaluating' ELSE 'testing' END,1,1,'00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000001','api',0 FROM generate_series(1,3) i;
