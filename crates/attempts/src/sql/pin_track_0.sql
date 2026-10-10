
            SELECT t.slug AS "slug!", t.state AS "state!", t.producer AS "producer?: JsonbText", t.mode AS "mode!", t.workflow AS "workflow?: JsonbText"
            FROM units h JOIN tracks t ON t.id = h.track_id
            WHERE h.id = $1
            FOR SHARE OF t
            