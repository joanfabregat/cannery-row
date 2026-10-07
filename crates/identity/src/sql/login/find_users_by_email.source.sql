
            SELECT 
    id, issuer, subject, email, email_verified, display_name, is_admin, created_at, last_login_at
 FROM users
            WHERE lower(email) = lower($1) AND email_verified
              AND ($2::uuid IS NULL OR (created_at, id) > (
                  SELECT created_at, id FROM users WHERE id = $2))
            ORDER BY created_at, id LIMIT $3
            