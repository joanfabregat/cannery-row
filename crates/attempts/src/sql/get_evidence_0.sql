
            SELECT id AS "id!: EvidenceId", front_matter AS "content!: JsonbText", sha256 AS "sha256!" FROM phase_outputs
            WHERE attempt_id = $1 AND stage = $2 ORDER BY revision DESC LIMIT 1
            