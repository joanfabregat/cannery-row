SELECT c.id AS "id!: _", c.project_id AS "project_id!: _", c.unit_id AS "unit_id!: _", h.number AS unit_number, c.attempt_id AS "attempt_id?: _", a.sequence AS "attempt_sequence?", c.author_user AS "author_user!: _", c.body_markdown, c.revision, c.created_at AS "created_at!: _", c.edited_at AS "edited_at?: _"
FROM comments c JOIN units h ON h.id=c.unit_id LEFT JOIN attempts a ON a.id=c.attempt_id
WHERE c.unit_id=$1 AND ($2::uuid IS NULL OR c.attempt_id=$2)
AND ($3::uuid IS NULL OR (c.created_at,c.id)<(SELECT created_at,id FROM comments WHERE id=$3 AND unit_id=$1))
ORDER BY c.created_at DESC,c.id DESC LIMIT $4
