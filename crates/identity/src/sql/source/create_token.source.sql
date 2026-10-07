
            INSERT INTO api_tokens (token_hash, display_prefix, kind, user_id, service_account_id,
                                    name, scopes, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, now() + make_interval(days => $8))
            RETURNING 
    id, display_prefix, kind, user_id, service_account_id, name, scopes, created_at,
    expires_at, last_used_at, revoked_at

            