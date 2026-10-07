
            INSERT INTO sessions (secret_hash, user_id, csrf_token, expires_at)
            VALUES ($1, $2, $3, now() + make_interval(hours => $4))
            RETURNING id, user_id, csrf_token, expires_at
            