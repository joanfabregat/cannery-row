
            SELECT r.content -> 'project_fields' AS "value?: JsonbText"
            FROM hypotheses h
            JOIN hypothesis_revisions r
              ON r.hypothesis_id = h.id AND r.revision = h.approved_revision
            WHERE h.id = $1
            