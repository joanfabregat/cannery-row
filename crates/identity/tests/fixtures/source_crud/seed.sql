INSERT INTO users(id,issuer,subject,created_at) VALUES
('00000000-0000-0000-0000-000000000001','fixture','owner','2024-01-02Z'),
('00000000-0000-0000-0000-000000000002','fixture','other','2024-01-02Z');
INSERT INTO projects(id,slug,title,created_by,created_at) VALUES
('00000000-0000-0000-0000-000000000003','fixture','Fixture','00000000-0000-0000-0000-000000000001','2024-01-02Z'),
('00000000-0000-0000-0000-000000000006','other','Other','00000000-0000-0000-0000-000000000001','2024-01-02Z');
INSERT INTO service_accounts(id,project_id,kind,name,description,created_by,created_at) VALUES
('00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000003','agent','agent','Unicode é','00000000-0000-0000-0000-000000000001','2024-01-02Z'),
('00000000-0000-0000-0000-000000000005','00000000-0000-0000-0000-000000000003','tester','tester','','00000000-0000-0000-0000-000000000001','2024-01-02Z');
INSERT INTO api_tokens(id,token_hash,display_prefix,kind,user_id,service_account_id,name,scopes,created_at,expires_at) VALUES
('00000000-0000-0000-0000-000000000100',decode('00','hex'),'fixture','personal','00000000-0000-0000-0000-000000000001',NULL,'first',ARRAY['write','read','write'],'2024-01-02Z','9999-01-01Z'),
('00000000-0000-0000-0000-000000000101',decode('01','hex'),'fixture','personal','00000000-0000-0000-0000-000000000001',NULL,'second',ARRAY['read'],'2024-01-02Z','9999-01-01Z'),
('00000000-0000-0000-0000-000000000102',decode('02','hex'),'fixture','personal','00000000-0000-0000-0000-000000000002',NULL,'other',ARRAY['read'],'2024-01-03Z','9999-01-01Z'),
('00000000-0000-0000-0000-000000000103',decode('03','hex'),'fixture','service',NULL,'00000000-0000-0000-0000-000000000004','service',ARRAY['write'],'2024-01-02Z','9999-01-01Z'),
('00000000-0000-0000-0000-000000000104',decode('04','hex'),'fixture','service',NULL,'00000000-0000-0000-0000-000000000005','other-service',ARRAY['read'],'2024-01-03Z','9999-01-01Z');
CREATE SEQUENCE identity_token_ids START 1000;
CREATE SEQUENCE identity_service_ids START 500;
ALTER TABLE api_tokens ALTER COLUMN id SET DEFAULT ('00000000-0000-0000-0000-'||lpad(nextval('identity_token_ids')::text,12,'0'))::uuid;
ALTER TABLE service_accounts ALTER COLUMN id SET DEFAULT ('00000000-0000-0000-0000-'||lpad(nextval('identity_service_ids')::text,12,'0'))::uuid;
ALTER TABLE api_tokens ALTER COLUMN created_at SET DEFAULT '2025-01-02T03:04:05.123456Z'::timestamptz;
ALTER TABLE service_accounts ALTER COLUMN created_at SET DEFAULT '2025-01-02T03:04:05.123456Z'::timestamptz;
CREATE TABLE identity_clocks(tag text PRIMARY KEY, expires_clock timestamptz, touch_clock timestamptz, revoke_clock timestamptz, disable_clock timestamptz);
CREATE FUNCTION observe_identity_clock() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
IF TG_TABLE_NAME='api_tokens' THEN
  INSERT INTO identity_clocks(tag,expires_clock,touch_clock,revoke_clock)
  VALUES('token:'||NEW.id::text,CASE WHEN TG_OP='INSERT' THEN now() END,CASE WHEN NEW.last_used_at IS NOT NULL THEN now() END,CASE WHEN NEW.revoked_at IS NOT NULL THEN now() END)
  ON CONFLICT(tag) DO UPDATE SET
    touch_clock=CASE WHEN NEW.last_used_at IS DISTINCT FROM OLD.last_used_at THEN now() ELSE identity_clocks.touch_clock END,
    revoke_clock=CASE WHEN NEW.revoked_at IS DISTINCT FROM OLD.revoked_at THEN now() ELSE identity_clocks.revoke_clock END;
ELSE
  INSERT INTO identity_clocks(tag,disable_clock) VALUES('service:'||NEW.id::text,CASE WHEN NEW.disabled_at IS NOT NULL THEN now() END)
  ON CONFLICT(tag) DO UPDATE SET disable_clock=CASE WHEN NEW.disabled_at IS DISTINCT FROM OLD.disabled_at THEN now() ELSE identity_clocks.disable_clock END;
END IF;
RETURN NULL;
END $$;
CREATE TRIGGER identity_token_clock AFTER INSERT OR UPDATE ON api_tokens FOR EACH ROW EXECUTE FUNCTION observe_identity_clock();
CREATE TRIGGER identity_service_clock AFTER INSERT OR UPDATE ON service_accounts FOR EACH ROW EXECUTE FUNCTION observe_identity_clock();
