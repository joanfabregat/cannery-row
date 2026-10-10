
            SELECT h.id AS "id!: HypothesisId" FROM hypotheses h JOIN tracks t ON t.id = h.track_id
            WHERE h.project_id = $1 AND h.state = 'queued' AND t.state = 'active'
              AND t.mode = $2
              AND ($3::bigint IS NULL OR h.number = $4)
              AND ($5::text IS NULL OR t.slug = $6)
              AND t.slug <> ALL($7::text[])
              AND NOT EXISTS (SELECT 1 FROM concerns c WHERE c.track_id = t.id AND c.state = 'open')
            ORDER BY h.approved_at, h.number
            LIMIT 1
            FOR UPDATE OF h SKIP LOCKED
            