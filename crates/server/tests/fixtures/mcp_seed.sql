CREATE TABLE fixture_mcp_touches(token_id uuid PRIMARY KEY, n integer NOT NULL);
CREATE FUNCTION fixture_mcp_touch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
INSERT INTO fixture_mcp_touches VALUES(NEW.id,1) ON CONFLICT(token_id) DO UPDATE SET n=fixture_mcp_touches.n+1;
RETURN NEW; END $$;
CREATE TRIGGER fixture_mcp_touch AFTER UPDATE OF last_used_at ON api_tokens FOR EACH ROW EXECUTE FUNCTION fixture_mcp_touch();
INSERT INTO artifacts(id,project_id,attempt_id,role,backend,bucket,key,size_bytes,sha256,media_type,verified_at,uri,origin,source_ref) SELECT
('70000000-0000-4000-8000-'||lpad(n::text,12,'0'))::uuid,
'00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000000200',
CASE WHEN n=2 THEN 'logs' ELSE 'report_asset' END,
CASE WHEN n=4 THEN 'external' ELSE 'local' END,
CASE WHEN n=3 THEN 'other' ELSE 'local' END,
'mcp-object-'||n,5,repeat('a',64),'text/plain','2001-01-01Z',
CASE WHEN n=4 THEN 'https://archive.example.invalid/object' END,
CASE WHEN n=4 THEN 'imported' ELSE 'live' END,
CASE WHEN n=4 THEN 'mcp-fixture/archive' END
FROM generate_series(1,4)n;
