SELECT id AS "id!: _" FROM jobs
            WHERE project_id = $1 AND stage = $2 AND state = 'pending' AND tester_id = $3
              AND ($4::text IS NULL OR spec -> 'evaluator' ->> 'revision' = $4::text)
              AND NOT EXISTS (SELECT 1 FROM attempts a
                              WHERE a.id = jobs.attempt_id AND a.state = 'cancelled')
            ORDER BY created_at, id
            LIMIT 1
            FOR UPDATE SKIP LOCKED
