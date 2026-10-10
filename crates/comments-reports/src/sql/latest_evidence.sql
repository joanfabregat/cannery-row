SELECT DISTINCT ON(stage) id AS "id!: _",stage,status,revision,front_matter::text AS "content!",body,producer_user AS "producer_user?: _",producer_service AS "producer_service?: _",created_at AS "created_at!: _",origin,source_ref
FROM phase_outputs WHERE attempt_id=$1 AND stage<>'writeup' ORDER BY stage,revision DESC
