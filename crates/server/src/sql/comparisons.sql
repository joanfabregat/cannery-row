SELECT jsonb_build_object(
    'id', c.id, 'attempt_ref', '#' || h.number || '.' || a.sequence,
    'hypothesis', h.number, 'hypothesis_title', h.title, 'hypothesis_state', h.state,
    'attempt_state', a.state, 'track', t.slug, 'science_revision', a.science_revision,
    'metric', c.metric, 'split', c.split, 'dimensions', c.dimensions, 'value', c.value,
    'source', c.source, 'reference', jsonb_build_object('value', c.reference_value,
       'label', c.reference_label, 'kind', c.reference_kind, 'ref', c.reference_ref),
    'verdict', c.verdict, 'policy_revision', c.policy_revision,
    'evidence_id', c.evidence_id, 'recorded_at', c.recorded_at, 'origin', a.origin
)::text AS "row!"
FROM comparisons c
JOIN attempts a ON a.id = c.attempt_id
JOIN hypotheses h ON h.id = c.hypothesis_id
JOIN tracks t ON t.id = c.track_id
WHERE c.project_id = $1
AND ($2::text IS NULL OR c.metric = $2)
AND ($3::text IS NULL OR c.split = $3)
AND ($4 OR c.dimension_keys = $5::text[])
AND ($6::text::jsonb IS NULL OR NOT EXISTS (
    SELECT 1 FROM jsonb_each($6::text::jsonb) AS f(name, allowed)
    WHERE NOT coalesce(f.allowed ? (c.dimensions ->> f.name), false)))
AND ($7::text[] IS NULL OR t.slug = ANY($7))
AND ($8::text[] IS NULL OR a.state = ANY($8))
AND ($9::text[] IS NULL OR c.verdict = ANY($9))
AND ($10::text::timestamptz IS NULL OR c.recorded_at >= $10::text::timestamptz)
AND ($11::text::timestamptz IS NULL OR c.recorded_at < $11::text::timestamptz)
AND ($12::text IS NULL OR a.origin = $12)
AND ($13::bigint IS NULL OR c.id < $13)
ORDER BY c.id DESC LIMIT $14
