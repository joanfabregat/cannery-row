SELECT id AS "id!: _", attempt_id AS "attempt_id!: _" FROM jobs
            WHERE state = 'claimed' AND (lease_expires_at <= now() OR deadline <= now())
              AND id <> ALL($1::uuid[])
            ORDER BY least(lease_expires_at, deadline), id
            LIMIT $2::text::bigint
