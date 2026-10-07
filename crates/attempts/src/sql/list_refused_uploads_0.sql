
            SELECT id AS "id!: UploadId", role AS "role!", key AS "key!", interface AS "interface?", refusal AS "refusal!: JsonbText" FROM uploads
            WHERE job_id = $1 AND state = 'failed' AND refusal IS NOT NULL
            ORDER BY completed_at, id
            