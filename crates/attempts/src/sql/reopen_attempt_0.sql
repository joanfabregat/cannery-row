
        UPDATE attempts SET state = $1, finished_at = NULL
        WHERE id = $2 AND state = 'failed' AND lease_token_hash IS NULL
        