SELECT count(*) AS "count!" FROM jobs
            WHERE attempt_id = $1 AND phase = $2 AND origin = 'auto_retry'
              AND run_number > (
                  SELECT coalesce(max(run_number), 0) FROM jobs
                  WHERE attempt_id = $1 AND phase = $2 AND origin <> 'auto_retry')
