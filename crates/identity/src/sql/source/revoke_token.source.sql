
            UPDATE api_tokens SET revoked_at = now()
            WHERE id = $1 AND revoked_at IS NULL RETURNING 
    id, display_prefix, kind, user_id, service_account_id, name, scopes, created_at,
    expires_at, last_used_at, revoked_at

            