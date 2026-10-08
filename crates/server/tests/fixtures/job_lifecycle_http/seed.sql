-- Recovery fixtures in a uniquely owned migrated child, with genuine operational PG clocks.
UPDATE hypotheses SET state='active',revision=2,approved_revision=2,approved_at=now() WHERE number IN (5,6) AND project_id='00000000-0000-0000-0000-000000000010';
INSERT INTO hypothesis_revisions(hypothesis_id,revision,content,science_revision,author_user,via_channel)
SELECT id,2,'{"project_fields":{},"control":null}',3,created_by_user,'api' FROM hypotheses WHERE number IN (5,6) AND project_id='00000000-0000-0000-0000-000000000010';
INSERT INTO config_revisions(project_id,kind,revision,content,created_by)
VALUES('00000000-0000-0000-0000-000000000010','science',3,
'{"baselines":[],"datasets":[{"id":"fixture","revision":"data-1","held_out_labels":false}],"metrics":[],"interfaces":[{"name":"fixture-json","version":1,"encoding":"json","schema":{"type":"object"}}],"tester":{"id":"fixture-tester","revision":"tester-1"},"scorer":{"name":"scorer","spec":{"inputs":{"artifacts":[{"source":"fixture"}]} }},"required_artifact_roles":{"tester":[]},"max_auto_retries":1}',
'00000000-0000-0000-0000-000000000001');
ALTER TABLE jobs DISABLE TRIGGER jobs_frozen;
UPDATE jobs SET state='claimed',claimed_by_service=CASE WHEN stage='tester' THEN '00000000-0000-0000-0000-000000000023'::uuid ELSE '00000000-0000-0000-0000-000000000024'::uuid END,
lease_generation=9,lease_token_hash=sha256(convert_to('fixture-held','UTF8')),lease_expires_at=now()+interval '1 hour',deadline=now()+interval '2 hours',claimed_at=now(),
spec=spec||jsonb_build_object('steps',jsonb_build_array(jsonb_build_object('name','scorer','revision','scorer-1','manifest',jsonb_build_object('spec',jsonb_build_object('role','scorer','inputs',jsonb_build_object('artifacts',jsonb_build_array()))))),'tester',jsonb_build_object('id','fixture-tester','revision','tester-1'),'inputs',jsonb_build_object('baselines',jsonb_build_array(),'datasets',jsonb_build_array(),'claimed_sheet',jsonb_build_object('ref','00000000-0000-0000-0000-000000003008','sha256',repeat('a',64))),'control',null,'output_prefix','job-life/','parameters',jsonb_build_object(),'track','track-4')
WHERE id='00000000-0000-0000-0000-000000006005';
SET CONSTRAINTS ALL IMMEDIATE;
ALTER TABLE jobs ENABLE TRIGGER jobs_frozen;
ALTER TABLE phase_outputs DISABLE TRIGGER phase_outputs_immutable;
UPDATE phase_outputs SET front_matter='{"provenance":{"source_revision":"source-1"}}' WHERE id='00000000-0000-0000-0000-000000003008';
ALTER TABLE phase_outputs ENABLE TRIGGER phase_outputs_immutable;
CREATE TABLE fixture_job_lifecycle_fault(enabled boolean NOT NULL);
INSERT INTO fixture_job_lifecycle_fault VALUES(false);
CREATE FUNCTION fixture_job_lifecycle_audit_fault() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN IF EXISTS(SELECT 1 FROM fixture_job_lifecycle_fault WHERE enabled) THEN RAISE EXCEPTION 'isolated audit failure';END IF; RETURN NEW; END $$;
CREATE TRIGGER fixture_job_lifecycle_audit_fault BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION fixture_job_lifecycle_audit_fault();
-- A second independent tester run permits publication and capability uploads.
UPDATE attempts SET state='testing' WHERE id='00000000-0000-0000-0000-000000002006';
INSERT INTO phase_outputs(id,project_id,attempt_id,stage,status,front_matter,sha256,producer_user,via_channel)
VALUES('00000000-0000-0000-0000-000000003006','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002006','agent','completed','{"provenance":{"source_revision":"source-1"}}',repeat('a',64),'00000000-0000-0000-0000-000000000001','api');
ALTER TABLE jobs DISABLE TRIGGER jobs_frozen;
UPDATE jobs SET stage='tester',tester_id='fixture-tester',state='claimed',claimed_by_service='00000000-0000-0000-0000-000000000023',lease_generation=9,lease_token_hash=sha256(convert_to('fixture-held-second','UTF8')),lease_expires_at=now()+interval '1 hour',deadline=now()+interval '2 hours',claimed_at=now(),
spec=jsonb_build_object('steps',jsonb_build_array(jsonb_build_object('name','scorer','revision','scorer-1','manifest',jsonb_build_object('spec',jsonb_build_object('role','scorer','inputs',jsonb_build_object('artifacts',jsonb_build_array(jsonb_build_object('from','dataset','id','fixture','name','fixture'))))))),'tester',jsonb_build_object('id','fixture-tester','revision','tester-1'),'inputs',jsonb_build_object('baselines',jsonb_build_array(),'datasets',jsonb_build_array(jsonb_build_object('id','fixture','revision','data-1')),'claimed_sheet',jsonb_build_object('ref','00000000-0000-0000-0000-000000003006','sha256',repeat('a',64))),'control',null,'output_prefix','job-life/','parameters',jsonb_build_object(),'track','track-4','limits',jsonb_build_object('max_output_bytes',10000))
WHERE id='00000000-0000-0000-0000-000000006006';
SET CONSTRAINTS ALL IMMEDIATE;
ALTER TABLE jobs ENABLE TRIGGER jobs_frozen;
