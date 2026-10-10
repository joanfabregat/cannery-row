-- Fictional users of dev/bench.sh.
--
-- Users normally appear on their first OIDC sign-in, and no API creates one,
-- so these rows are written straight to the database, as dev/seed-demo.sh
-- does for its demo users. The issuer is a reserved .invalid domain that no
-- Cannery Row installation is configured with, so nobody can ever sign in as
-- these users; their emails are on example.com. The admin sets the bench
-- project up and mints and revokes the service tokens, through sessions that
-- last minutes; the researcher holds the bench's personal token. Re-running
-- is a no-op.
INSERT INTO users (issuer, subject, email, email_verified, display_name, is_admin)
VALUES
    ('https://bench-users.cannery-row.invalid', 'admin', 'bench.admin@example.com', true, 'Bench Admin', true),
    ('https://bench-users.cannery-row.invalid', 'researcher', 'bench.researcher@example.com', true, 'Bench Researcher', false)
ON CONFLICT (issuer, subject) DO NOTHING;
