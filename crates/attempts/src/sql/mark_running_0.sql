
        UPDATE attempts SET state = 'running', started_at = coalesce(started_at, now())
        WHERE id = $1 AND state = 'claimed'
        