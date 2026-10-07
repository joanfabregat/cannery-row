SELECT jsonb_build_object(
'users',(SELECT jsonb_agg(jsonb_build_array(id,issuer,subject,email,email_verified,display_name,is_admin,created_at::text,last_login_at::text) ORDER BY id) FROM users),
'accounts',(SELECT jsonb_agg(jsonb_build_array(id,project_id,kind,name,description,created_by,created_at::text,disabled_at::text) ORDER BY id) FROM service_accounts),
'sessions',(SELECT jsonb_agg(jsonb_build_array(id,user_id,created_at::text,expires_at::text,CASE WHEN last_seen_at='2024-01-02Z'::timestamptz THEN last_seen_at::text ELSE 'operational-clock' END,csrf_token='fixture-é') ORDER BY id) FROM sessions),
 'tokens',(SELECT jsonb_agg(jsonb_build_array(id,display_prefix,kind,user_id,service_account_id,name,scopes,created_at::text,expires_at::text,CASE WHEN last_used_at IS NULL THEN NULL ELSE 'operational-clock' END,revoked_at::text) ORDER BY id) FROM api_tokens))::text
