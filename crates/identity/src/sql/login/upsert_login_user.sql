
            INSERT INTO users (issuer, subject, email, email_verified, display_name, is_admin,
                               last_login_at)
            VALUES ($1, $2, $3, $4, $5, $6, now())
            ON CONFLICT (issuer, subject) DO UPDATE SET
                email = EXCLUDED.email,
                email_verified = EXCLUDED.email_verified,
                display_name = EXCLUDED.display_name,
                is_admin = users.is_admin OR EXCLUDED.is_admin,
                last_login_at = now()
            RETURNING 
    id AS "id: _", issuer, subject, email, email_verified, display_name, is_admin, created_at AS "created_at: _", last_login_at AS "last_login_at: _"

            