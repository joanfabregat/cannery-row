SELECT count(*) AS "count!" FROM jobs JOIN attempts a ON a.id = jobs.attempt_id
            WHERE jobs.project_id = $1 AND jobs.phase = 'verify' AND jobs.state = 'pending'
              AND jobs.performer = 'agent' AND a.state <> 'cancelled'
              AND (a.claimed_by_service = $2::uuid OR a.claimed_by_user = $3::uuid)
