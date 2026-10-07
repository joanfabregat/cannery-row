SELECT m.id, m.attempt_id AS "attempt_id!: _", h.number AS hypothesis_number,
h.title AS hypothesis_title,h.state AS hypothesis_state,a.sequence AS attempt_sequence,a.state AS attempt_state,
a.science_revision,a.claimed_at AS "claimed_at!: _",a.submitted_at AS "submitted_at: _",a.finished_at AS "finished_at: _",
t.slug AS track_slug,t.title AS track_title,m.metric,m.split,m.dimensions::text AS "dimensions!",m.value,m.missing_reason,m.unit,m.direction,m.sample_count AS "sample_count: _",
m.control_value,m.uncertainty_method,m.uncertainty_lower,m.uncertainty_upper,m.authority,m.source_ref,m.recorded_at AS "recorded_at!: _",
(hr.content->'control')::text AS control,(hr.content->'project_fields')::text AS project_fields,
ref.reference_value AS "reference_value?",ref.reference_label AS "reference_label?",ref.reference_kind AS "reference_kind?",ref.reference_ref AS "reference_ref?"
FROM measurements m
JOIN attempts a ON a.id=m.attempt_id
JOIN hypotheses h ON h.id=a.hypothesis_id
JOIN tracks t ON t.id=a.track_id
JOIN hypothesis_revisions hr ON hr.hypothesis_id=h.id AND hr.revision=a.hypothesis_revision
LEFT JOIN LATERAL(
 SELECT c.reference_value,c.reference_label,c.reference_kind,c.reference_ref FROM comparisons c
 WHERE c.evidence_id=(SELECT e.id FROM evidence_records e WHERE e.attempt_id=m.attempt_id AND e.stage='evaluator' AND e.status='completed' ORDER BY e.revision DESC LIMIT 1)
 AND c.metric=m.metric AND c.split=m.split AND c.dimensions=m.dimensions LIMIT 1
)ref ON true
WHERE m.project_id=$1 AND m.metric=$2
AND m.authority=ANY($3::text[])
AND m.evidence_id=(SELECT e.id FROM evidence_records e WHERE e.attempt_id=m.attempt_id AND e.stage=$4 ORDER BY e.revision DESC LIMIT 1)
AND ($5::text IS NULL OR m.split=$5)
AND ($6 OR m.dimension_keys=$7::text[])
AND ($8::text::jsonb IS NULL OR NOT EXISTS(SELECT 1 FROM jsonb_each($8::text::jsonb) AS f(name,allowed) WHERE NOT coalesce(f.allowed ? (m.dimensions->>f.name),false)))
AND ($9::text[] IS NULL OR t.slug=ANY($9::text[]))
AND ($10::text[] IS NULL OR a.state=ANY($10))
AND ($11::text::bigint IS NULL OR a.science_revision=$11::text::bigint)
AND (coalesce($12::timestamptz,$13::timestamp::timestamptz) IS NULL OR m.recorded_at>=coalesce($12::timestamptz,$13::timestamp::timestamptz))
AND (coalesce($14::timestamptz,$15::timestamp::timestamptz) IS NULL OR m.recorded_at<coalesce($14::timestamptz,$15::timestamp::timestamptz))
AND ($16::text::bigint IS NULL OR m.id<$16::text::bigint)
ORDER BY m.id DESC LIMIT $17::text::bigint
