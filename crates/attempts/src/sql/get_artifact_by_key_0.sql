
            SELECT id AS "id!: ArtifactId", attempt_id AS "attempt_id!: AttemptId", role AS "role!", backend AS "backend!", bucket AS "bucket!", key AS "key!", generation AS "generation?", size_bytes AS "size_bytes!", sha256 AS "sha256!", media_type AS "media_type!", verified_at AS "verified_at!: Timestamp", job_id AS "job_id?: JobId", interface AS "interface?", content_validated AS "content_validated?", origin AS "origin!", source_ref AS "source_ref?", uri AS "uri?" FROM artifacts
            WHERE backend = $1 AND bucket = $2 AND key = $3 AND project_id = $4
            