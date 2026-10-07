SELECT c.id AS "id!: _", c.project_id AS "project_id!: _", c.hypothesis_id AS "hypothesis_id!: _", h.number AS hypothesis_number, c.attempt_id AS "attempt_id?: _", a.sequence AS "attempt_sequence?", c.author_user AS "author_user!: _", c.body_markdown, c.revision, c.created_at AS "created_at!: _", c.edited_at AS "edited_at?: _"
FROM comments c JOIN hypotheses h ON h.id=c.hypothesis_id LEFT JOIN attempts a ON a.id=c.attempt_id
WHERE c.id=$1 AND c.project_id=$2
