
            SELECT code AS "code!" FROM attempt_failures WHERE attempt_id = $1
            ORDER BY created_at DESC LIMIT 1
            