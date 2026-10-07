
            SELECT count(*) AS "count!" FROM uploads
            WHERE attempt_id = $1 AND job_id IS NOT DISTINCT FROM $2
              AND state IN ('pending', 'receiving') AND expires_at > now()
            