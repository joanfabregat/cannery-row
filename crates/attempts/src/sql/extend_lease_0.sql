
        UPDATE attempts SET lease_expires_at = now() + make_interval(secs => $1),
                            state = 'running', started_at = coalesce(started_at, now())
        WHERE id = $2
        