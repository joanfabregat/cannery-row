SELECT coalesce(sum(declared_size), 0)::text AS "bytes!" FROM uploads
            WHERE job_id = $1
              AND (state = 'verified' OR (state IN ('pending', 'receiving') AND expires_at > now()))
