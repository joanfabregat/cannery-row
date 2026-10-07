INSERT INTO users(id,issuer,subject,email,email_verified,display_name,is_admin,created_at) VALUES
('00000000-0000-0000-0000-000000000001','provider','alice','Case@EXAMPLE.test',true,'Alice',false,'2024-01-02Z'),
('00000000-0000-0000-0000-000000000002','provider','unverified','case@example.test',false,NULL,false,'2024-01-01Z'),
('00000000-0000-0000-0000-000000000003','provider','tie','case@example.test',true,'Tie',false,'2024-01-02Z'),
('00000000-0000-0000-0000-000000000004','provider','admin',NULL,false,NULL,true,'2024-01-03Z');
INSERT INTO sessions(id,secret_hash,user_id,csrf_token,created_at,expires_at,last_seen_at) VALUES
('00000000-0000-0000-0000-000000000100',decode('00','hex'),'00000000-0000-0000-0000-000000000001','csrf-é','2024-01-02Z','9999-01-01Z','2024-01-02Z'),
('00000000-0000-0000-0000-000000000101',decode('01','hex'),'00000000-0000-0000-0000-000000000001','expired','2024-01-02Z','2000-01-01Z','2024-01-02Z');
CREATE SEQUENCE login_user_ids START 500;
CREATE SEQUENCE login_session_ids START 1000;
ALTER TABLE users ALTER COLUMN id SET DEFAULT ('00000000-0000-0000-0000-'||lpad(nextval('login_user_ids')::text,12,'0'))::uuid;
ALTER TABLE sessions ALTER COLUMN id SET DEFAULT ('00000000-0000-0000-0000-'||lpad(nextval('login_session_ids')::text,12,'0'))::uuid;
ALTER TABLE users ALTER COLUMN created_at SET DEFAULT '2025-01-02T03:04:05.123456Z'::timestamptz;
ALTER TABLE sessions ALTER COLUMN created_at SET DEFAULT '2025-01-02T03:04:05.123456Z'::timestamptz;
ALTER TABLE sessions ALTER COLUMN last_seen_at SET DEFAULT '2025-01-02T03:04:05.123456Z'::timestamptz;
ALTER TABLE oidc_login_requests ALTER COLUMN created_at SET DEFAULT '9999-01-01Z'::timestamptz;
INSERT INTO oidc_login_requests(state,nonce,code_verifier,return_to,browser_hash,created_at) VALUES
('expired','old','old','/',decode('00','hex'),'2000-01-01Z');
CREATE TABLE login_clocks(tag text PRIMARY KEY, clock timestamptz NOT NULL);
CREATE FUNCTION observe_login_clock() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
IF TG_TABLE_NAME='users' THEN
  IF TG_OP='INSERT' OR NEW.last_login_at IS DISTINCT FROM OLD.last_login_at THEN
    INSERT INTO login_clocks VALUES('user:'||NEW.id::text,now()) ON CONFLICT(tag) DO UPDATE SET clock=EXCLUDED.clock;
  END IF;
ELSE
  IF TG_OP='INSERT' THEN
    INSERT INTO login_clocks VALUES('expires:'||NEW.id::text,now()) ON CONFLICT(tag) DO UPDATE SET clock=EXCLUDED.clock;
  ELSIF NEW.last_seen_at IS DISTINCT FROM OLD.last_seen_at THEN
    INSERT INTO login_clocks VALUES('touch:'||NEW.id::text,now()) ON CONFLICT(tag) DO UPDATE SET clock=EXCLUDED.clock;
  END IF;
END IF;
RETURN NULL;
END $$;
CREATE TRIGGER login_user_clock AFTER INSERT OR UPDATE ON users FOR EACH ROW EXECUTE FUNCTION observe_login_clock();
CREATE TRIGGER login_session_clock AFTER INSERT OR UPDATE ON sessions FOR EACH ROW EXECUTE FUNCTION observe_login_clock();
