
            SELECT r.content -> 'project_fields' AS "value?: JsonbText"
            FROM units h
            JOIN unit_revisions r
              ON r.unit_id = h.id AND r.revision = h.approved_revision
            WHERE h.id = $1
            