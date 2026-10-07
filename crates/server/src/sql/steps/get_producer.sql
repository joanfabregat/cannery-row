SELECT name, revision, content::text AS "content!", created_by AS "created_by!: UserId", created_at AS "created_at!: Timestamp"
FROM producer_manifests WHERE project_id = $1 AND name = $2 AND revision = $3
