
        INSERT INTO attempt_failures (attempt_id, stage, code, reason, details, log_refs,
                                      requeued)
        VALUES ($1, 'agent', $2, $3, $4, $5, true)
        