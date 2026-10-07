SELECT count(*) AS "count!" FROM jobs
            WHERE attempt_id = $1 AND stage = $2 AND origin = 'auto_retry'
              AND run_number > (
                  SELECT coalesce(max(run_number), 0) FROM jobs
                  WHERE attempt_id = $1 AND stage = $2 AND origin <> 'auto_retry')
