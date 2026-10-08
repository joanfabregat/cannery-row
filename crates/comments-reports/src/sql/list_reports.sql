SELECT e.id AS "id!: _",e.attempt_id AS "attempt_id!: _",e.status,(e.front_matter->'report')::text AS report,e.created_at AS "created_at!: _",e.producer_user AS "producer_user?: _",e.producer_service AS "producer_service?: _",h.number AS hypothesis_number,h.title AS hypothesis_title,t.slug AS track_slug,a.sequence AS attempt_sequence,a.state AS attempt_state,a.origin
FROM phase_outputs e JOIN attempts a ON a.id=e.attempt_id JOIN hypotheses h ON h.id=a.hypothesis_id JOIN tracks t ON t.id=a.track_id
WHERE e.project_id=$1 AND e.stage='agent' AND e.front_matter ? 'report'
AND e.revision=(SELECT max(x.revision) FROM phase_outputs x WHERE x.attempt_id=e.attempt_id AND x.stage='agent')
AND ($2::bigint IS NULL OR h.number=$2) AND ($3::text IS NULL OR t.slug=$3)
AND ($4::uuid IS NULL OR (e.created_at,e.id)<(SELECT created_at,id FROM phase_outputs WHERE id=$4 AND project_id=$1))
ORDER BY e.created_at DESC,e.id DESC LIMIT $5
