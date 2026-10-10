SELECT id AS "id!: _", project_id AS "project_id!: _", attempt_id AS "attempt_id!: _", phase, performer, run_number, state, science_revision, verifier_id, spec::text AS "spec!", deadline_seconds, created_at AS "created_at!: _", claimed_by_service AS "claimed_by_service: _", claimed_by_user AS "claimed_by_user: _", via_channel, via_client, lease_generation, lease_token_hash, lease_expires_at AS "lease_expires_at: _", claimed_at AS "claimed_at: _", deadline AS "deadline: _", finished_at AS "finished_at: _", evidence_id AS "evidence_id: _", manifest_id AS "manifest_id: _", error_step, error_code, error_reason, logs::text AS "logs!", origin, previous_run_id AS "previous_run_id: _" FROM jobs
            WHERE attempt_id = $1
              AND ($2::uuid IS NULL OR (phase, run_number) > (
                  SELECT phase, run_number FROM jobs
                  WHERE id = $2 AND attempt_id = $1))
            ORDER BY phase, run_number LIMIT $3::text::bigint
