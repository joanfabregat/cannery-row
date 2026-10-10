SELECT id AS "id!: _" FROM jobs
            WHERE project_id = $1 AND phase = 'decide' AND state = 'pending'
              AND performer = 'runner' AND verifier_id = $2
              AND spec -> 'decider' ->> 'revision' = $3::text
              AND EXISTS (SELECT 1 FROM attempts a
                          JOIN units h ON h.id = a.unit_id
                          WHERE a.id = jobs.attempt_id AND h.state = 'deciding')
            ORDER BY created_at, id
            LIMIT 1
            FOR UPDATE SKIP LOCKED
