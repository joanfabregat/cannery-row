SELECT count(*) AS "rows!",count(m.value) AS "measured!",coalesce(sum(m.sample_count),0) AS "sample_count!: _",
coalesce(array_agg(DISTINCT a.science_revision ORDER BY a.science_revision),'{}') AS "science_revisions!",
coalesce(jsonb_agg(DISTINCT hr.content->'control')FILTER(WHERE hr.content?'control'),'[]')::text AS "controls!"
FROM measurements m
JOIN attempts a ON a.id=m.attempt_id
JOIN hypotheses h ON h.id=a.hypothesis_id
JOIN tracks t ON t.id=a.track_id
JOIN hypothesis_revisions hr ON hr.hypothesis_id=h.id AND hr.revision=a.hypothesis_revision
WHERE m.project_id=$1 AND m.metric=$2
AND m.authority=ANY($3::text[])
AND m.evidence_id=(SELECT e.id FROM phase_outputs e WHERE e.attempt_id=m.attempt_id AND e.stage=$4 ORDER BY e.revision DESC LIMIT 1)
AND ($5::text IS NULL OR m.split=$5)
AND ($6 OR m.dimension_keys=$7::text[])
AND ($8::text::jsonb IS NULL OR NOT EXISTS(SELECT 1 FROM jsonb_each($8::text::jsonb) AS f(name,allowed) WHERE NOT coalesce(f.allowed ? (m.dimensions->>f.name),false)))
AND ($9::text[] IS NULL OR t.slug=ANY($9::text[]))
AND ($10::text[] IS NULL OR a.state=ANY($10))
AND ($11::text::bigint IS NULL OR a.science_revision=$11::text::bigint)
AND (coalesce($12::timestamptz,$13::timestamp::timestamptz) IS NULL OR m.recorded_at>=coalesce($12::timestamptz,$13::timestamp::timestamptz))
AND (coalesce($14::timestamptz,$15::timestamp::timestamptz) IS NULL OR m.recorded_at<coalesce($14::timestamptz,$15::timestamp::timestamptz))
