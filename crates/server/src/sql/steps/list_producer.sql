SELECT name, revision, content::text AS "content!", created_by AS "created_by!: UserId", created_at AS "created_at!: Timestamp"
FROM producer_manifests
WHERE project_id = $1 AND ($2::text IS NULL OR name = $2)
AND ($3::text IS NULL OR name > $3 OR (name = $3 AND revision < $4::int))
ORDER BY name, revision DESC LIMIT $5
