
        UPDATE uploads SET urls_expire_at = greatest(
            urls_expire_at, now() + make_interval(secs => $1))
        WHERE id = $2
        