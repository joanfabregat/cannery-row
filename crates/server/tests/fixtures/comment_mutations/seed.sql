-- Run after the comment-read seed in an isolated migrated child only.
INSERT INTO tracks(id,project_id,slug,title,created_by,created_at,updated_at) VALUES
('00000000-0000-0000-0000-000000090000','00000000-0000-0000-0000-000000000011','foreign','Foreign','00000000-0000-0000-0000-000000000001','2001-01-01Z','2001-01-01Z');
INSERT INTO hypotheses(id,project_id,number,track_id,title,created_by_user,created_at,updated_at) VALUES
('00000000-0000-0000-0000-000000090001','00000000-0000-0000-0000-000000000011',1,'00000000-0000-0000-0000-000000090000','Private reference','00000000-0000-0000-0000-000000000001','2001-01-01Z','2001-01-01Z');
UPDATE search_documents SET updated_at='2001-01-01Z';
CREATE SEQUENCE fixture_comment_identity START 1;
CREATE FUNCTION fixture_comment_identity() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
NEW.id=('60000000-0000-4000-8000-'||lpad(nextval('fixture_comment_identity')::text,12,'0'))::uuid;
IF NEW.body_markdown='fixed serialization 日本語 é😀' THEN
NEW.created_at='2024-07-01 01:02:03.123456Z';
END IF;
RETURN NEW; END $$;
CREATE TRIGGER fixture_comment_identity BEFORE INSERT ON comments FOR EACH ROW EXECUTE FUNCTION fixture_comment_identity();
CREATE TABLE fixture_comment_failure(action text);
INSERT INTO fixture_comment_failure VALUES(NULL);
CREATE FUNCTION fixture_comment_audit_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
IF NEW.action=(SELECT action FROM fixture_comment_failure) THEN
RAISE EXCEPTION 'isolated comment audit failure';
END IF;
RETURN NEW; END $$;
CREATE TRIGGER fixture_comment_audit_failure BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION fixture_comment_audit_failure();
-- Synthetic browser channel credentials remain confined to the owned test database.
INSERT INTO sessions(id,secret_hash,user_id,csrf_token,expires_at) VALUES
('00000000-0000-0000-0000-000000090010',sha256(convert_to('cr_ses_comment_fixture','UTF8')),'00000000-0000-0000-0000-000000000002','comment_fixture_csrf',now()+interval '1 day');
-- A global administrator still needs membership to write; once admitted,
-- cross-project mention visibility follows the source administrator read bypass.
INSERT INTO memberships(project_id,user_id,role,granted_by) VALUES
('00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000000001','member','00000000-0000-0000-0000-000000000001');
