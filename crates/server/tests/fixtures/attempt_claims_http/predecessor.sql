-- A recovered failed predecessor is immutable history; only the new claim mutates.
UPDATE units SET lease_generation=7 WHERE number=2;
INSERT INTO attempts(id,project_id,unit_id,sequence,state,unit_revision,science_revision,track_id,producer,claimed_by_user,via_channel,lease_generation,claimed_at,finished_at,origin,source_ref,imported)
VALUES('00000000-0000-0000-0000-000000002004','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000001002',4,'failed',1,1,'00000000-0000-0000-0000-000000000004','{"legacy":"producer"}','00000000-0000-0000-0000-000000000001','historical',7,'2001-01-01Z','2001-01-02Z','imported','recovered-prior','{"label":"é😀","notes":[null,false]}');
INSERT INTO attempt_failures(id,attempt_id,stage,code,reason,details,created_at,requeued,log_refs)
VALUES('00000000-0000-0000-0000-000000003001','00000000-0000-0000-0000-000000002004','agent','prior-first','First','{}','2001-01-01Z',false,'[]'),
('00000000-0000-0000-0000-000000003002','00000000-0000-0000-0000-000000002004','verify','prior-last','Last','{"raw":[null,true]}','2001-01-02Z',false,'[]');
INSERT INTO artifacts(id,project_id,attempt_id,role,backend,bucket,key,size_bytes,sha256,media_type,verified_at,origin,source_ref,uri)
VALUES('00000000-0000-0000-0000-000000004001','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002004','prior','local','fixture','prior/é😀',17,repeat('a',64),'application/json','2001-01-01Z','live',NULL,NULL),
('00000000-0000-0000-0000-000000004002','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002004','prior','external','','',0,repeat('b',64),'text/plain','2001-01-02Z','imported','recovered-external','https://fixture.invalid/object'),
('00000000-0000-0000-0000-000000004003','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002004','unwanted','local','fixture','other',23,repeat('c',64),'text/plain','2001-01-03Z','live',NULL,NULL);
