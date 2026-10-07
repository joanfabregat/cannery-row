INSERT INTO users(id,issuer,subject,email,email_verified,display_name,is_admin,created_at,last_login_at)
VALUES ('00000000-0000-0000-0000-000000000001','fixture','user','verified@example.test',true,'Unicode é 🦀',true,'2024-01-02Z','2024-01-03Z'),
('00000000-0000-0000-0000-000000000002','fixture','other',NULL,false,NULL,false,'2024-01-02Z',NULL);
INSERT INTO projects(id,slug,title,created_by,created_at) VALUES('00000000-0000-0000-0000-000000000003','fixture','Fixture','00000000-0000-0000-0000-000000000001','2024-01-02Z');
INSERT INTO service_accounts(id,project_id,kind,name,description,created_by,created_at,disabled_at)
VALUES('00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000003','agent','agent','Unicode é','00000000-0000-0000-0000-000000000001','2024-01-02Z',NULL),
('00000000-0000-0000-0000-000000000005','00000000-0000-0000-0000-000000000003','experimenter','experimenter','','00000000-0000-0000-0000-000000000001','2024-01-02Z','2024-01-03Z');
