SELECT tag,value FROM (
SELECT 'token:'||t.id::text AS tag,(to_jsonb(t)||jsonb_build_object(
'expires_at',CASE WHEN c.expires_clock IS NOT NULL AND t.expires_at IS NOT NULL THEN jsonb_build_object('clock_delta_microseconds',(extract(epoch FROM(t.expires_at-c.expires_clock))*1000000)::bigint::text) ELSE to_jsonb(t.expires_at) END,
'last_used_at',CASE WHEN c.touch_clock IS NOT NULL AND t.last_used_at IS NOT NULL THEN jsonb_build_object('clock_delta_microseconds',(extract(epoch FROM(t.last_used_at-c.touch_clock))*1000000)::bigint::text) ELSE to_jsonb(t.last_used_at) END,
'revoked_at',CASE WHEN c.revoke_clock IS NOT NULL AND t.revoked_at IS NOT NULL THEN jsonb_build_object('clock_delta_microseconds',(extract(epoch FROM(t.revoked_at-c.revoke_clock))*1000000)::bigint::text) ELSE to_jsonb(t.revoked_at) END))::text AS value FROM api_tokens t LEFT JOIN identity_clocks c ON c.tag='token:'||t.id::text
UNION ALL
SELECT 'service:'||s.id::text,(to_jsonb(s)||jsonb_build_object('disabled_at',CASE WHEN c.disable_clock IS NOT NULL AND s.disabled_at IS NOT NULL THEN jsonb_build_object('clock_delta_microseconds',(extract(epoch FROM(s.disabled_at-c.disable_clock))*1000000)::bigint::text) ELSE to_jsonb(s.disabled_at) END))::text FROM service_accounts s LEFT JOIN identity_clocks c ON c.tag='service:'||s.id::text
) records ORDER BY tag
