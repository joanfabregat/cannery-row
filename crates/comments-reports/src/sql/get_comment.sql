SELECT c.id AS "id!: _", c.project_id AS "project_id!: _", c.unit_id AS "unit_id!: _", h.number AS unit_number, c.attempt_id AS "attempt_id?: _", a.sequence AS "attempt_sequence?", c.author_user AS "author_user!: _", c.body_markdown, c.revision, c.created_at AS "created_at!: _", c.edited_at AS "edited_at?: _"
FROM comments c JOIN units h ON h.id=c.unit_id LEFT JOIN attempts a ON a.id=c.attempt_id
WHERE c.id=$1 AND c.project_id=$2
