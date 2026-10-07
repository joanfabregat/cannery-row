SELECT count(*) AS "count!" FROM attempts a JOIN tracks t ON t.id=a.track_id
WHERE a.project_id=$1 AND a.state='failed' AND a.origin=$2
AND ($3::text[] IS NULL OR t.slug=ANY($3::text[]))
AND ($4::text[] IS NULL OR 'failed'=ANY($4))
AND ($5::text::bigint IS NULL OR a.science_revision=$5::text::bigint)
AND (coalesce($6::timestamptz,$7::timestamp::timestamptz) IS NULL OR coalesce(a.finished_at,a.claimed_at)>=coalesce($6::timestamptz,$7::timestamp::timestamptz))
AND (coalesce($8::timestamptz,$9::timestamp::timestamptz) IS NULL OR coalesce(a.finished_at,a.claimed_at)<coalesce($8::timestamptz,$9::timestamp::timestamptz))
