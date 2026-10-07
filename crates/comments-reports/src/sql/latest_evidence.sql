SELECT DISTINCT ON(stage) id AS "id!: _",stage,status,revision,content::text AS "content!",producer_user AS "producer_user?: _",producer_service AS "producer_service?: _",created_at AS "created_at!: _",origin,source_ref
FROM evidence_records WHERE attempt_id=$1 ORDER BY stage,revision DESC
