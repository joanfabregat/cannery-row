
WITH q AS (
    SELECT websearch_to_tsquery('simple', coalesce($2::text, '')) AS tsq
), base AS (
    SELECT d.id, d.kind, d.source_id, d.attempt_id, d.title AS doc_title, d.body,
           d.occurred_at, d.actor_user, d.actor_service, p.slug AS project, t.slug AS track,
           h.number AS hypothesis_number, h.title AS hypothesis_title,
           a.sequence AS attempt_sequence, h.state AS hypothesis_state,
           a.state AS attempt_state,
           CASE WHEN d.kind IN ('track', 'comment') THEN 'live'
                WHEN d.kind = 'decision_reason'
                     THEN (SELECT x.origin FROM decisions x WHERE x.id = d.source_id)
                ELSE coalesce(a.origin, h.origin, 'live') END AS origin,
           CASE WHEN $2::text IS NULL THEN 0::float8
                WHEN $3::boolean THEN ts_rank_cd(d.tsv, q.tsq)::float8
                     + word_similarity($2::text, d.title)::float8
                     + word_similarity($2::text, d.body)::float8 / 2
                ELSE ts_rank_cd(d.tsv, q.tsq)::float8
           END AS score
    FROM search_documents d
    CROSS JOIN q
    JOIN projects p ON p.id = d.project_id
    LEFT JOIN hypotheses h ON h.id = d.hypothesis_id
    LEFT JOIN attempts a ON a.id = d.attempt_id
    LEFT JOIN tracks t ON t.id = coalesce(d.track_id, a.track_id, h.track_id)
    WHERE ($1::uuid[] IS NULL OR d.project_id = ANY($1::uuid[]))
      AND ($7::text[] IS NULL OR p.slug = ANY($7::text[]))
      AND ($8::text[] IS NULL OR d.kind = ANY($8::text[]))
      AND (coalesce($15::timestamptz,$16::timestamp::timestamptz) IS NULL OR d.occurred_at >= coalesce($15::timestamptz,$16::timestamp::timestamptz))
      AND (coalesce($17::timestamptz,$18::timestamp::timestamptz) IS NULL OR d.occurred_at < coalesce($17::timestamptz,$18::timestamp::timestamptz))
      AND ($14::uuid[] IS NULL OR d.actor_user = ANY($14::uuid[])
           OR d.actor_service = ANY($14::uuid[]))
      AND ($2::text IS NULL
           OR d.tsv @@ q.tsq
           OR ($3::boolean
               AND ($2::text <% d.title OR $2::text <% d.body)))
      AND ($5::bigint IS NULL OR (
           h.number = $5::bigint
           AND ($4::text IS NULL OR p.slug = $4::text)
           AND CASE WHEN $6::bigint IS NULL THEN d.kind = 'hypothesis'
                    ELSE a.sequence = $6::bigint END))
      AND ($9::text[] IS NULL OR t.slug = ANY($9::text[]))
      AND ($10::text[] IS NULL
           OR h.state = ANY($10::text[]))
      AND ($11::text[] IS NULL OR a.state = ANY($11::text[]))
)

, facts AS (
    SELECT x.attempt_id, v.verdict, dec.action AS decision
    FROM (SELECT DISTINCT attempt_id FROM base WHERE attempt_id IS NOT NULL) x
    LEFT JOIN LATERAL (
        SELECT e.content #>> '{assessment,verdict}' AS verdict
        FROM evidence_records e
        WHERE e.attempt_id = x.attempt_id AND e.stage = 'evaluator'
          AND e.status = 'completed' AND e.content ? 'assessment'
        ORDER BY e.revision DESC LIMIT 1
    ) v ON true
    LEFT JOIN LATERAL (
        SELECT d.action
        FROM review_cases c JOIN decisions d ON d.review_case_id = c.id
        WHERE c.attempt_id = x.attempt_id AND c.kind = 'result'
          AND NOT EXISTS (SELECT 1 FROM decisions s WHERE s.supersedes = d.id)
        ORDER BY d.decided_at DESC LIMIT 1
    ) dec ON true
)

, filtered AS (
    SELECT b.*, f.verdict, f.decision
    FROM base b LEFT JOIN facts f ON f.attempt_id = b.attempt_id
    WHERE ($12::text[] IS NULL OR f.verdict = ANY($12::text[]))
      AND ($13::text[] IS NULL OR f.decision = ANY($13::text[]))
)

            SELECT 'kind' AS "facet!", kind AS value, count(*) AS "count!" FROM filtered GROUP BY kind
            UNION ALL SELECT 'project', project, count(*) FROM filtered GROUP BY project
            UNION ALL SELECT 'track', track, count(*) FROM filtered
                WHERE track IS NOT NULL GROUP BY track
            UNION ALL SELECT 'hypothesis_state', hypothesis_state, count(*) FROM filtered
                WHERE hypothesis_state IS NOT NULL GROUP BY hypothesis_state
            UNION ALL SELECT 'attempt_state', attempt_state, count(*) FROM filtered
                WHERE attempt_state IS NOT NULL GROUP BY attempt_state
            UNION ALL SELECT 'verdict', verdict, count(*) FROM filtered
                WHERE verdict IS NOT NULL GROUP BY verdict
            UNION ALL SELECT 'decision', decision, count(*) FROM filtered
                WHERE decision IS NOT NULL GROUP BY decision
            UNION ALL SELECT 'actor', coalesce(actor_user, actor_service)::text, count(*)
                FROM filtered WHERE coalesce(actor_user, actor_service) IS NOT NULL
                GROUP BY coalesce(actor_user, actor_service)
            UNION ALL SELECT 'total', NULL, count(*) FROM filtered
            