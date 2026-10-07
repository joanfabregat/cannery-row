INSERT INTO jobs (id, project_id, attempt_id, stage, run_number, science_revision,
                              tester_id, spec, deadline_seconds, origin, previous_run_id)
            SELECT $1, $2, $3, $4, coalesce(max(run_number), 0) + 1,
                   $5, $6, $7, $8, $9, $10
            FROM jobs WHERE attempt_id = $3 AND stage = $4
            RETURNING id AS "id!: _", project_id AS "project_id!: _", attempt_id AS "attempt_id!: _", stage, run_number, state, science_revision, tester_id, spec AS "spec!: JsonbText", deadline_seconds, created_at AS "created_at!: DeferredTimestamp", claimed_by_service AS "claimed_by_service: _", via_channel, via_client, lease_generation, lease_token_hash, lease_expires_at AS "lease_expires_at: DeferredTimestamp", claimed_at AS "claimed_at: DeferredTimestamp", deadline AS "deadline: DeferredTimestamp", finished_at AS "finished_at: DeferredTimestamp", evidence_id AS "evidence_id: _", manifest_id AS "manifest_id: _", error_step, error_code, error_reason, logs AS "logs!: JsonbText", origin, previous_run_id AS "previous_run_id: _"
