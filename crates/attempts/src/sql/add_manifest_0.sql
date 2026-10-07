
            INSERT INTO manifests (attempt_id, stage, content, sha256) VALUES ($1, $2, $3, $4)
            RETURNING id AS "id!: ManifestId", attempt_id AS "attempt_id!: AttemptId", stage AS "stage!", content AS "content!: JsonbText", sha256 AS "sha256!", created_at AS "created_at!: Timestamp"
            