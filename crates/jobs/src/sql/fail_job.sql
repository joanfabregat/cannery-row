UPDATE jobs SET state = 'failed', error_step = $1, error_code = $2,
                error_reason = $3, logs = $4, lease_token_hash = NULL, lease_expires_at = NULL,
                finished_at = now()
            WHERE id = $5 AND state = 'claimed'
            RETURNING id AS "id!: _", project_id AS "project_id!: _", attempt_id AS "attempt_id!: _", stage, run_number, state, science_revision, tester_id, spec::text AS "spec!", deadline_seconds, created_at AS "created_at!: _", claimed_by_service AS "claimed_by_service: _", via_channel, via_client, lease_generation, lease_token_hash, lease_expires_at AS "lease_expires_at: _", claimed_at AS "claimed_at: _", deadline AS "deadline: _", finished_at AS "finished_at: _", evidence_id AS "evidence_id: _", manifest_id AS "manifest_id: _", error_step, error_code, error_reason, logs::text AS "logs!", origin, previous_run_id AS "previous_run_id: _"
