
            INSERT INTO attempt_failures (attempt_id, stage, code, reason, details, log_refs)
            VALUES ($1, $2, $3, $4, $5, $6) RETURNING id AS "id!: FailureId"
            