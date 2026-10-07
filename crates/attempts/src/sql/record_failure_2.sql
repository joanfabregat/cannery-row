
            INSERT INTO review_cases (project_id, hypothesis_id, attempt_id, kind,
                                      subject_revision, failure_id)
            SELECT $1, $2, $3, 'failure', count(*), $4
            FROM attempt_failures WHERE attempt_id = $5
            RETURNING id AS "id!: ReviewCaseId"
            