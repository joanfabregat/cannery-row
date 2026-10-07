SELECT (to_jsonb(j)||CASE WHEN c.id IS NOT NULL THEN jsonb_build_object(
'claimed_at',jsonb_build_object('microseconds',trunc(extract(epoch from(j.claimed_at-c.clock))*1000000)::text),
'deadline',jsonb_build_object('microseconds',trunc(extract(epoch from(j.deadline-c.clock))*1000000)::text),
'lease_expires_at',jsonb_build_object('microseconds',trunc(extract(epoch from(j.lease_expires_at-c.clock))*1000000)::text)) ELSE '{}'::jsonb END)::text
FROM jobs j LEFT JOIN creation_clocks c ON c.id=j.id ORDER BY j.id
