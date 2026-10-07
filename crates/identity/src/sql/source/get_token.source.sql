SELECT 
    id, display_prefix, kind, user_id, service_account_id, name, scopes, created_at,
    expires_at, last_used_at, revoked_at
 FROM api_tokens WHERE id = $1