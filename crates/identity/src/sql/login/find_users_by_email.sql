
            SELECT 
    id AS "id: _", issuer, subject, email, email_verified, display_name, is_admin, created_at AS "created_at: _", last_login_at AS "last_login_at: _"
 FROM users
            WHERE lower(email) = lower($1) AND email_verified
              AND ($2::uuid IS NULL OR (created_at, id) > (
                  SELECT created_at, id FROM users WHERE id = $2))
            ORDER BY created_at, id LIMIT $3::text::bigint
            