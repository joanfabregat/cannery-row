
        UPDATE attempts SET state = $1, lease_token_hash = NULL, lease_expires_at = NULL,
            submitted_at = CASE WHEN $2 = 'submitted' THEN now() ELSE submitted_at END,
            finished_at = CASE WHEN $3 IN ('failed', 'cancelled') THEN now() ELSE finished_at END
        WHERE id = $4 AND state IN ('claimed', 'running')
        