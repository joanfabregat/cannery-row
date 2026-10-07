
            UPDATE service_accounts SET disabled_at = now()
            WHERE id = $1 AND disabled_at IS NULL RETURNING 
    id, project_id, kind, name, description, created_by, created_at, disabled_at

            