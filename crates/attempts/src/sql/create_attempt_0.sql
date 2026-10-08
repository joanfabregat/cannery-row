
            WITH bumped AS (
                UPDATE hypotheses SET state = 'active', lease_generation = lease_generation + 1,
                                      updated_at = now()
                WHERE id = $1
                RETURNING id, project_id, track_id, approved_revision, lease_generation
            ), previous AS (
                SELECT max(sequence) AS sequence,
                       (array_agg(id ORDER BY sequence DESC))[1] AS id
                FROM attempts WHERE hypothesis_id = $1
            )
            INSERT INTO attempts (project_id, hypothesis_id, sequence, hypothesis_revision,
                                  science_revision, track_id, producer,
                                  claimed_by_user, claimed_by_service, via_channel, via_client,
                                  predecessor_id, lease_generation, lease_token_hash,
                                  lease_expires_at, workflow, deadline, brief_revision)
            SELECT b.project_id, b.id, coalesce(p.sequence, 0) + 1, b.approved_revision,
                   $2, b.track_id, $3, $4, $5,
                   $6, $7, p.id, b.lease_generation, $8,
                   now() + make_interval(secs => $9), $10,
                   now() + make_interval(secs => $11::integer),
                   (SELECT max(revision) FROM briefs WHERE project_id = b.project_id)
            FROM bumped b CROSS JOIN previous p
            RETURNING id AS "id!: AttemptId"
            