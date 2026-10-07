
            INSERT INTO artifacts (project_id, attempt_id, job_id, role, backend, bucket, key,
                                   generation, size_bytes, sha256, media_type, interface,
                                   content_validated)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            RETURNING id AS "id!: ArtifactId", attempt_id AS "attempt_id!: AttemptId", role AS "role!", backend AS "backend!", bucket AS "bucket!", key AS "key!", generation AS "generation?", size_bytes AS "size_bytes!", sha256 AS "sha256!", media_type AS "media_type!", verified_at AS "verified_at!: Timestamp", job_id AS "job_id?: JobId", interface AS "interface?", content_validated AS "content_validated?", origin AS "origin!", source_ref AS "source_ref?", uri AS "uri?"
            