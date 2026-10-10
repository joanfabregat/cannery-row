
                SELECT a.id AS "id!: AttemptId" FROM attempts a JOIN units h ON h.id = a.unit_id
                WHERE h.project_id = $1 AND h.number = $2 AND a.sequence = $3
                FOR UPDATE OF a
                