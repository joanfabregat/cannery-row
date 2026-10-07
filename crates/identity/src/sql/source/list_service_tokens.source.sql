
            SELECT 
    id, display_prefix, kind, user_id, service_account_id, name, scopes, created_at,
    expires_at, last_used_at, revoked_at
 FROM api_tokens
            WHERE service_account_id = $1
              AND ($2::uuid IS NULL OR (created_at, id) < (
                  SELECT created_at, id FROM api_tokens
                  WHERE id = $2 AND service_account_id = $1))
            ORDER BY created_at DESC, id DESC LIMIT $3
            