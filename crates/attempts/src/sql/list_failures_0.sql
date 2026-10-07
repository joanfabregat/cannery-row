
            SELECT attempt_id AS "attempt_id!: AttemptId", id AS "id!: FailureId", stage AS "stage!", code AS "code!", reason AS "reason!", details AS "details!: JsonbText", created_at AS "created_at!: Timestamp", requeued AS "requeued!", log_refs AS "log_refs!: JsonbText"
            FROM attempt_failures WHERE attempt_id = ANY($1) ORDER BY created_at
            