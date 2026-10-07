SELECT jsonb_build_object('table','uploads','row',to_jsonb(t)-ARRAY['expires_at','receiving_since','completed_at','urls_expire_at'],
 'expires_delta',extract(epoch FROM expires_at-now())::text,'receiving_now',receiving_since=now(),
 'completed_now',completed_at=now(),'urls_delta',extract(epoch FROM urls_expire_at-now())::text)::text
FROM uploads t
UNION ALL
SELECT jsonb_build_object('table','artifacts','row',to_jsonb(t))::text FROM artifacts t
UNION ALL
SELECT jsonb_build_object('table','jobs','row',to_jsonb(t)-'created_at','created_now',created_at=now())::text FROM jobs t
ORDER BY 1;
