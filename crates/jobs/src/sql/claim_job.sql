UPDATE jobs SET state = 'claimed', claimed_by_service = $1,
                via_channel = $2, via_client = $3,
                lease_generation = lease_generation + 1, lease_token_hash = $4,
                claimed_at = now(), deadline = now() + make_interval(secs => deadline_seconds),
                lease_expires_at = now() + make_interval(secs => least($5, deadline_seconds))
            WHERE id = $6 AND state = 'pending'
            RETURNING id AS "id!: _", project_id AS "project_id!: _", attempt_id AS "attempt_id!: _", stage, run_number, state, science_revision, tester_id, spec::text AS "spec!", deadline_seconds, created_at AS "created_at!: _", claimed_by_service AS "claimed_by_service: _", via_channel, via_client, lease_generation, lease_token_hash, lease_expires_at AS "lease_expires_at: _", claimed_at AS "claimed_at: _", deadline AS "deadline: _", finished_at AS "finished_at: _", evidence_id AS "evidence_id: _", manifest_id AS "manifest_id: _", error_step, error_code, error_reason, logs::text AS "logs!", origin, previous_run_id AS "previous_run_id: _"
