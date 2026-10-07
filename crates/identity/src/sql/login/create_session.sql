
            INSERT INTO sessions (secret_hash, user_id, csrf_token, expires_at)
            VALUES ($1, $2, $3, now() + make_interval(hours => $4::text::integer))
            RETURNING id AS "id: _", user_id AS "user_id: _", csrf_token, expires_at AS "expires_at: _"
            