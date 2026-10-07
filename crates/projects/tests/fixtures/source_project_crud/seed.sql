INSERT INTO users(id,issuer,subject,email,display_name,created_at) VALUES
('00000000-0000-0000-0000-000000000064','fixture','a','a@example.invalid','Zebra','2020-01-02T03:04:05Z'),
('00000000-0000-0000-0000-000000000065','fixture','b','b@example.invalid','alpha','2020-01-02T03:04:05Z'),
('00000000-0000-0000-0000-000000000066','fixture','c',NULL,NULL,'2020-01-02T03:04:05Z'),
('00000000-0000-0000-0000-000000000067','fixture','d','é@example.invalid','Éclair','2020-01-02T03:04:05Z');
CREATE FUNCTION fixture_project_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN NEW.id=md5('fixture:'||NEW.slug)::uuid;
IF NEW.slug <> 'clock' THEN NEW.created_at='2020-01-02T03:04:05.123456Z'; END IF;
IF NEW.slug LIKE 'broken%' THEN NEW.created_at='10000-01-01T00:00:00Z'; END IF;
RETURN NEW; END $$;
CREATE TRIGGER fixture_project_insert BEFORE INSERT ON projects FOR EACH ROW EXECUTE FUNCTION fixture_project_insert();
CREATE FUNCTION fixture_membership_write() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
IF NEW.project_id <> md5('fixture:clock')::uuid THEN NEW.granted_at='2020-01-02T03:04:05.234567Z'; END IF;
IF NEW.project_id = md5('fixture:gamma')::uuid AND NEW.user_id='00000000-0000-0000-0000-000000000067'::uuid THEN NEW.granted_at='10000-01-01T00:00:00Z'; END IF;
RETURN NEW; END $$;
CREATE TRIGGER fixture_membership_write BEFORE INSERT OR UPDATE ON memberships FOR EACH ROW EXECUTE FUNCTION fixture_membership_write();
INSERT INTO projects(slug,title,description,created_by) VALUES
('alpha','Alpha','é 🦀','00000000-0000-0000-0000-000000000064'),
('beta','Beta','','00000000-0000-0000-0000-000000000064'),
('gamma','Gamma','','00000000-0000-0000-0000-000000000065');
INSERT INTO memberships(project_id,user_id,role,granted_by) VALUES
(md5('fixture:alpha')::uuid,'00000000-0000-0000-0000-000000000064','researcher','00000000-0000-0000-0000-000000000064'),
(md5('fixture:alpha')::uuid,'00000000-0000-0000-0000-000000000065','member','00000000-0000-0000-0000-000000000064'),
(md5('fixture:alpha')::uuid,'00000000-0000-0000-0000-000000000066','viewer','00000000-0000-0000-0000-000000000064'),
(md5('fixture:beta')::uuid,'00000000-0000-0000-0000-000000000067','viewer','00000000-0000-0000-0000-000000000064');
