
            INSERT INTO phase_outputs (project_id, attempt_id, stage, status, revision,
                                       front_matter, sha256, manifest_id, producer_user,
                                       producer_service, via_channel, via_client)
            SELECT $1, $2, $3, $4, coalesce(max(revision), 0) + 1, $5, $6, $7, $8, $9, $10, $11
            FROM phase_outputs WHERE attempt_id = $12 AND stage = $13
            RETURNING id AS "id!: EvidenceId"
            