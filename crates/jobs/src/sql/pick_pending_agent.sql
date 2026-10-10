SELECT id AS "id!: _" FROM jobs
            WHERE project_id = $1 AND phase = 'verify' AND state = 'pending'
              AND performer = 'agent'
              AND NOT EXISTS (SELECT 1 FROM attempts a
                              WHERE a.id = jobs.attempt_id
                                AND (a.state = 'cancelled' OR a.claimed_by_service = $2::uuid
                                     OR a.claimed_by_user = $3::uuid))
            ORDER BY created_at, id
            LIMIT 1
            FOR UPDATE SKIP LOCKED
