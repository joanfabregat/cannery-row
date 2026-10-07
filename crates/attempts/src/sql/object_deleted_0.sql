
        UPDATE uploads SET object_pending_delete = false
        WHERE id = $1 AND (urls_expire_at IS NULL OR urls_expire_at <= now())
        