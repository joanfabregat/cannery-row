-- Isolated recovery fixtures; real SELECT now() remains the lease authority.
-- Run 6 of attempt 2008 is the runner job fixture-verifier holds; run 3 failed, since an
-- attempt has one open verify job at a time.
ALTER TABLE jobs DISABLE TRIGGER jobs_frozen;
UPDATE jobs SET state='failed',lease_token_hash=NULL,lease_expires_at=NULL,finished_at='2001-02-03T04:05:08Z',error_step='step',error_code='opaque_code',error_reason='é😀 failed' WHERE id='00000000-0000-0000-0000-000000006003';
UPDATE jobs SET state='claimed',claimed_by_service='00000000-0000-0000-0000-000000000023',lease_generation=9,lease_token_hash=sha256(convert_to('fixture-input-held-verifier','UTF8')),lease_expires_at='2099-01-01Z',deadline='2099-01-02Z',claimed_at='2001-01-01Z',finished_at=NULL,error_code=NULL,error_reason=NULL,spec=jsonb_build_object('inputs',jsonb_build_object('run',jsonb_build_object('ref','00000000-0000-0000-0000-000000003008'),'manifest',jsonb_build_object('ref','00000000-0000-0000-0000-000000007001'))) WHERE id='00000000-0000-0000-0000-000000006006';
ALTER TABLE jobs ENABLE TRIGGER jobs_frozen;
ALTER TABLE manifests DISABLE TRIGGER manifests_immutable;
UPDATE manifests SET content='{"schema_version":"0.2","attempt_id":"00000000-0000-0000-0000-000000002008","objects":[{"role":"data","storage":{"backend":"local","bucket":"local","key":"inputs/data"},"size_bytes":14,"sha256":"2e3b5cf9f0ecafdc0e19c1d505b9f9765eef004b043c3dcf1c9e0ffcd5e56394","media_type":"application/octet-stream"}]}' WHERE id='00000000-0000-0000-0000-000000007001';
ALTER TABLE manifests ENABLE TRIGGER manifests_immutable;
-- A researcher ran attempt 2007; the agent service account holds its agent verify job,
-- which pins a copy of the same run record and manifest.
INSERT INTO phase_outputs(id,project_id,attempt_id,stage,status,front_matter,sha256,producer_user,via_channel,created_at)
SELECT '00000000-0000-0000-0000-000000003007',project_id,'00000000-0000-0000-0000-000000002007',stage,status,front_matter,sha256,'00000000-0000-0000-0000-000000000002',via_channel,created_at FROM phase_outputs WHERE id='00000000-0000-0000-0000-000000003008';
INSERT INTO manifests(id,attempt_id,stage,content,sha256,created_at)
SELECT '00000000-0000-0000-0000-000000007007','00000000-0000-0000-0000-000000002007',stage,content,sha256,created_at FROM manifests WHERE id='00000000-0000-0000-0000-000000007001';
INSERT INTO jobs(id,project_id,attempt_id,phase,run_number,state,science_revision,performer,verifier_id,spec,deadline_seconds,created_at,claimed_by_service,via_channel,lease_generation,lease_token_hash,lease_expires_at,claimed_at,deadline)
VALUES('00000000-0000-0000-0000-000000006013','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002007','verify',1,'claimed',3,'agent',NULL,'{"inputs":{"run":{"ref":"00000000-0000-0000-0000-000000003007"},"manifest":{"ref":"00000000-0000-0000-0000-000000007007"}}}',600,'2001-02-03T04:05:06Z','00000000-0000-0000-0000-000000000020','historical',9,sha256(convert_to('fixture-input-held-agent','UTF8')),'2099-01-01Z','2001-01-01Z','2099-01-02Z');
INSERT INTO service_accounts(id,project_id,kind,name,created_by) VALUES('00000000-0000-0000-0000-000000000027','00000000-0000-0000-0000-000000000010','verifier','other-verifier','00000000-0000-0000-0000-000000000001');
INSERT INTO api_tokens(token_hash,display_prefix,kind,service_account_id,name,scopes,expires_at) VALUES(sha256(convert_to('cr_svc_track_http_other-verifier','UTF8')),'cr_svc_fixture','service','00000000-0000-0000-0000-000000000027','other-verifier',ARRAY['read'],'2099-01-01Z'),(sha256(convert_to('cr_svc_track_http_verifier-readonly','UTF8')),'cr_svc_fixture','service','00000000-0000-0000-0000-000000000023','verifier-readonly',ARRAY['read'],'2099-01-01Z'),(sha256(convert_to('cr_svc_track_http_verifier-writeonly','UTF8')),'cr_svc_fixture','service','00000000-0000-0000-0000-000000000023','verifier-writeonly',ARRAY['write'],'2099-01-01Z');
UPDATE search_documents SET updated_at='2001-01-01Z',occurred_at='2001-01-01Z';
