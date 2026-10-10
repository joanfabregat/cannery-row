-- Recovery fixtures in a uniquely owned migrated child, with genuine operational PG clocks.
UPDATE units SET state='active',revision=2,approved_revision=2,approved_at=now() WHERE number IN (5,6) AND project_id='00000000-0000-0000-0000-000000000010';
INSERT INTO unit_revisions(unit_id,revision,content,science_revision,author_user,via_channel)
SELECT id,2,'{"project_fields":{},"control":null}',3,created_by_user,'api' FROM units WHERE number IN (5,6) AND project_id='00000000-0000-0000-0000-000000000010';
INSERT INTO config_revisions(project_id,kind,revision,content,created_by)
VALUES('00000000-0000-0000-0000-000000000010','science',3,
'{"baselines":[],"datasets":[{"id":"fixture","revision":"data-1","held_out_labels":false}],"metrics":[],"interfaces":[{"name":"fixture-json","version":1,"encoding":"json","schema":{"type":"object"}}],"verify":{"performer":"runner","verifier":{"id":"fixture-verifier","revision":"policy-1"}},"scorer":{"name":"scorer","spec":{"inputs":{"artifacts":[{"source":"fixture"}]} }},"required_artifact_roles":{"verify":[]},"limits":{"report_max_bytes":10000,"max_output_bytes":10000},"max_auto_retries":1}',
'00000000-0000-0000-0000-000000000001');
-- The run records the verify jobs pin.
INSERT INTO phase_outputs(id,project_id,attempt_id,stage,status,front_matter,sha256,producer_user,producer_service,via_channel) VALUES
('00000000-0000-0000-0000-000000003005','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002005','agent','completed','{"provenance":{"source_revision":"source-1"}}',repeat('a',64),'00000000-0000-0000-0000-000000000002',NULL,'api'),
('00000000-0000-0000-0000-000000003006','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002006','agent','completed','{"provenance":{"source_revision":"source-1"}}',repeat('a',64),NULL,'00000000-0000-0000-0000-000000000020','api');
-- Both jobs become runner jobs of fixture-verifier, claimed under generation 9. The
-- second one's scorer reads the fixture dataset, so its report names that revision.
ALTER TABLE jobs DISABLE TRIGGER jobs_frozen;
UPDATE jobs SET performer='runner',verifier_id='fixture-verifier',state='claimed',claimed_by_service='00000000-0000-0000-0000-000000000023',via_channel='api',
lease_generation=9,lease_token_hash=sha256(convert_to(CASE WHEN id='00000000-0000-0000-0000-000000006005' THEN 'fixture-held' ELSE 'fixture-held-second' END,'UTF8')),lease_expires_at=now()+interval '1 hour',deadline=now()+interval '2 hours',claimed_at=now(),
spec=jsonb_build_object('performer','runner','verifier',jsonb_build_object('id','fixture-verifier','revision','policy-1'),
'steps',jsonb_build_array(jsonb_build_object('name','scorer','revision',3,'manifest',jsonb_build_object('spec',jsonb_build_object('role','scorer','inputs',jsonb_build_object('artifacts',CASE WHEN id='00000000-0000-0000-0000-000000006005' THEN '[]'::jsonb ELSE '[{"from":"dataset","id":"fixture","name":"fixture"}]'::jsonb END))))),
'inputs',jsonb_build_object('baselines',jsonb_build_array(),'datasets',CASE WHEN id='00000000-0000-0000-0000-000000006005' THEN '[]'::jsonb ELSE '[{"id":"fixture","revision":"data-1"}]'::jsonb END,
'run',jsonb_build_object('ref',CASE WHEN id='00000000-0000-0000-0000-000000006005' THEN '00000000-0000-0000-0000-000000003005' ELSE '00000000-0000-0000-0000-000000003006' END,'sha256',repeat('a',64)),
'manifest',jsonb_build_object('ref','00000000-0000-0000-0000-000000003099','sha256',repeat('b',64))),
'control',null,'output_prefix','job-life/','parameters',jsonb_build_object(),'track','track-4','limits',jsonb_build_object('max_output_bytes',10000))
WHERE id IN ('00000000-0000-0000-0000-000000006005','00000000-0000-0000-0000-000000006006');
SET CONSTRAINTS ALL IMMEDIATE;
ALTER TABLE jobs ENABLE TRIGGER jobs_frozen;
CREATE TABLE fixture_job_lifecycle_fault(enabled boolean NOT NULL);
INSERT INTO fixture_job_lifecycle_fault VALUES(false);
CREATE FUNCTION fixture_job_lifecycle_audit_fault() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN IF EXISTS(SELECT 1 FROM fixture_job_lifecycle_fault WHERE enabled) THEN RAISE EXCEPTION 'isolated audit failure';END IF; RETURN NEW; END $$;
CREATE TRIGGER fixture_job_lifecycle_audit_fault BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION fixture_job_lifecycle_audit_fault();
