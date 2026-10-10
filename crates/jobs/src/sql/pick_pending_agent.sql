SELECT id AS "id!: _" FROM jobs
            WHERE project_id = $1 AND phase = $4 AND state = 'pending'
              AND performer = 'agent'
              AND NOT EXISTS (SELECT 1 FROM attempts a
                              WHERE a.id = jobs.attempt_id
                                AND (a.state = 'cancelled'
                                     OR ($4 = 'verify' AND (a.claimed_by_service = $2::uuid
                                                           OR a.claimed_by_user = $3::uuid))))
              AND ($4 = 'verify' OR EXISTS (SELECT 1 FROM attempts a
                                            JOIN hypotheses h ON h.id = a.hypothesis_id
                                            WHERE a.id = jobs.attempt_id
                                              AND h.state = 'documenting'))
            ORDER BY created_at, id
            LIMIT 1
            FOR UPDATE SKIP LOCKED
