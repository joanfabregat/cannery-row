INSERT INTO producer_manifests (project_id, name, revision, content, created_by)
SELECT $1, $2, coalesce(max(revision), 0) + 1, $3::text::jsonb, $4
FROM producer_manifests WHERE project_id = $1 AND name = $2
RETURNING name, revision, content::text AS "content!", created_by AS "created_by!: UserId", created_at AS "created_at!: Timestamp"
