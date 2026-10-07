
        UPDATE uploads SET state = $1, completed_at = now(), refusal = $2,
                           object_pending_delete = $3
        WHERE id = $4
        