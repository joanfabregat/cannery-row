-- Fictional demo users for dev/seed-demo.sh.
--
-- Users normally appear on their first OIDC sign-in, and no API creates one,
-- so this is the only part of the demo data written straight to the
-- database. The issuer is a reserved .invalid domain that no Cannery Row
-- installation is configured with, so nobody can ever sign in as these
-- users; their emails are on example.com. Re-running is a no-op.
INSERT INTO users (issuer, subject, email, email_verified, display_name, is_admin)
VALUES
    ('https://demo-users.cannery-row.invalid', 'ada', 'ada.marlowe@example.com', true, 'Ada Marlowe', true),
    ('https://demo-users.cannery-row.invalid', 'ben', 'ben.okoro@example.com', true, 'Ben Okoro', false),
    ('https://demo-users.cannery-row.invalid', 'chloe', 'chloe.varga@example.com', true, 'Chloé Varga', false),
    ('https://demo-users.cannery-row.invalid', 'dev', 'dev.raman@example.com', true, 'Dev Raman', false),
    ('https://demo-users.cannery-row.invalid', 'eli', 'eli.novak@example.com', true, 'Eli Novak', false)
ON CONFLICT (issuer, subject) DO NOTHING;
