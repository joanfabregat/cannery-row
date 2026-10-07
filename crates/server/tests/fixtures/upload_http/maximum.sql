-- Only this isolated fixed-serialization trigger is suspended, then restored.
-- Production constraints and triggers remain enabled.
ALTER TABLE uploads DISABLE TRIGGER fixture_upload_row;
INSERT INTO uploads(id,attempt_id,lease_generation,token_hash,role,backend,bucket,key,slot,declared_size,declared_sha256,media_type,expires_at,created_at)
SELECT ('00000000-0000-0000-0000-'||lpad((91000+i)::text,12,'0'))::uuid,
       '00000000-0000-0000-0000-000000002001',1,
       sha256(convert_to(i::text,'UTF8')),'data','local','local',
       'fixture/'||i,'fixture/'||i,5,
       '2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824',
       'application/octet-stream','2099-01-01Z','2001-01-01Z'
FROM generate_series(1,32) i;
ALTER TABLE uploads ENABLE TRIGGER fixture_upload_row;
