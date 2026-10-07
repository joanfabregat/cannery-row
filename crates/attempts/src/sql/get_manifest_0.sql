
            SELECT id AS "id!: ManifestId", attempt_id AS "attempt_id!: AttemptId", stage AS "stage!", content AS "content!: JsonbText", sha256 AS "sha256!", created_at AS "created_at!: Timestamp" FROM manifests
            WHERE attempt_id = $1 AND id = $2
            