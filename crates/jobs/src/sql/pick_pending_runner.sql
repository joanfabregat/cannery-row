SELECT id AS "id!: _" FROM jobs
            WHERE project_id = $1 AND phase = 'verify' AND state = 'pending'
              AND performer = 'runner' AND verifier_id = $2
              AND spec -> 'verifier' ->> 'revision' = $3::text
              AND NOT EXISTS (SELECT 1 FROM attempts a
                              WHERE a.id = jobs.attempt_id AND a.state = 'cancelled')
            ORDER BY created_at, id
            LIMIT 1
            FOR UPDATE SKIP LOCKED
