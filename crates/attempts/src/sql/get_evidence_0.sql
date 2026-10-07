
            SELECT id AS "id!: EvidenceId", content AS "content!: JsonbText", sha256 AS "sha256!" FROM evidence_records
            WHERE attempt_id = $1 AND stage = $2 ORDER BY revision DESC LIMIT 1
            