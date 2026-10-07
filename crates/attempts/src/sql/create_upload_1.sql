
            INSERT INTO uploads (attempt_id, job_id, lease_generation, token_hash, role, backend,
                                 bucket, key, slot, declared_size, declared_sha256, media_type,
                                 interface, transfer, multipart_upload_id, part_size, expires_at)
            SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                   now() + make_interval(mins => $17)
            WHERE NOT EXISTS (
                SELECT 1 FROM artifacts
                WHERE backend = $18 AND bucket = $19 AND key IN ($20, $21)
            )
            ON CONFLICT (backend, bucket, slot) DO UPDATE SET
                id = gen_random_uuid(), attempt_id = EXCLUDED.attempt_id, job_id = EXCLUDED.job_id,
                lease_generation = EXCLUDED.lease_generation, token_hash = EXCLUDED.token_hash,
                role = EXCLUDED.role, key = EXCLUDED.key,
                declared_size = EXCLUDED.declared_size,
                declared_sha256 = EXCLUDED.declared_sha256, media_type = EXCLUDED.media_type,
                interface = EXCLUDED.interface, transfer = EXCLUDED.transfer,
                multipart_upload_id = EXCLUDED.multipart_upload_id,
                part_size = EXCLUDED.part_size, urls_expire_at = NULL,
                state = 'pending', expires_at = EXCLUDED.expires_at, created_at = now(),
                completed_at = NULL, receiving_since = NULL
            WHERE uploads.transfer = 'stream' AND uploads.expires_at <= now()
              AND (uploads.state IN ('pending', 'expired') OR (
    uploads.state = 'receiving'
    AND (uploads.receiving_since IS NULL
         OR uploads.receiving_since <= now() - make_interval(secs => $22))
))
            RETURNING id AS "id!: UploadId", attempt_id AS "attempt_id!: AttemptId", lease_generation AS "lease_generation!", token_hash AS "token_hash!", role AS "role!", backend AS "backend!", bucket AS "bucket!", key AS "key!", declared_size AS "declared_size!", declared_sha256 AS "declared_sha256!", media_type AS "media_type!", state AS "state!", expires_at AS "expires_at!: Timestamp", job_id AS "job_id?: JobId", interface AS "interface?", transfer AS "transfer!", multipart_upload_id AS "multipart_upload_id?", part_size AS "part_size?", urls_expire_at AS "urls_expire_at?: Timestamp"
            