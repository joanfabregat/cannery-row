
            SELECT count(*) AS "count!" FROM attempt_failures f JOIN attempts a ON a.id = f.attempt_id
            WHERE a.unit_id = $1 AND f.requeued AND f.code <> 'unanswered_question'
              AND f.created_at > coalesce((
                  SELECT max(f2.created_at) FROM attempt_failures f2
                  JOIN attempts a2 ON a2.id = f2.attempt_id
                  WHERE a2.unit_id = $1 AND NOT f2.requeued), '-infinity')
            