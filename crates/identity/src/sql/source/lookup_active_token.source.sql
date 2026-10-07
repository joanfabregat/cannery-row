
            UPDATE api_tokens SET last_used_at = now()
            WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now()
            RETURNING 
    id, display_prefix, kind, user_id, service_account_id, name, scopes, created_at,
    expires_at, last_used_at, revoked_at

            