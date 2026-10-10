INSERT INTO jobs (id, project_id, attempt_id, phase, run_number, science_revision, performer,
                              verifier_id, spec, deadline_seconds, origin, previous_run_id)
            SELECT $1, $2, $3, $4, coalesce(max(run_number), 0) + 1,
                   $5::text::integer, $6, $7, $8, $9::text::integer, $10, $11
            FROM jobs WHERE attempt_id = $3 AND phase = $4
            RETURNING id AS "id!: _", project_id AS "project_id!: _", attempt_id AS "attempt_id!: _", phase, performer, run_number, state, science_revision, verifier_id, spec::text AS "spec!", deadline_seconds, created_at AS "created_at!: _", claimed_by_service AS "claimed_by_service: _", claimed_by_user AS "claimed_by_user: _", via_channel, via_client, lease_generation, lease_token_hash, lease_expires_at AS "lease_expires_at: _", claimed_at AS "claimed_at: _", deadline AS "deadline: _", finished_at AS "finished_at: _", evidence_id AS "evidence_id: _", manifest_id AS "manifest_id: _", error_step, error_code, error_reason, logs::text AS "logs!", origin, previous_run_id AS "previous_run_id: _"
