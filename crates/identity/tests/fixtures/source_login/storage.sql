SELECT 'user:'||u.id::text,
 (to_jsonb(u)||CASE WHEN c.clock IS NOT NULL AND u.last_login_at IS NOT NULL THEN jsonb_build_object('last_login_at',(extract(epoch FROM u.last_login_at-c.clock)*1000000)::numeric::text) ELSE '{}'::jsonb END)::text
FROM users u LEFT JOIN login_clocks c ON c.tag='user:'||u.id::text
UNION ALL
SELECT 'session:'||s.id::text,
 (to_jsonb(s)||CASE WHEN e.clock IS NOT NULL THEN jsonb_build_object('expires_at',(extract(epoch FROM s.expires_at-e.clock)*1000000)::numeric::text) ELSE '{}'::jsonb END||CASE WHEN t.clock IS NOT NULL THEN jsonb_build_object('last_seen_at',(extract(epoch FROM s.last_seen_at-t.clock)*1000000)::numeric::text) ELSE '{}'::jsonb END)::text
FROM sessions s LEFT JOIN login_clocks e ON e.tag='expires:'||s.id::text LEFT JOIN login_clocks t ON t.tag='touch:'||s.id::text
UNION ALL SELECT 'request:'||r.state,to_jsonb(r)::text FROM oidc_login_requests r ORDER BY 1
