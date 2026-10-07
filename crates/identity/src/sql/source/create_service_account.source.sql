
            INSERT INTO service_accounts (project_id, kind, name, description, created_by)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING 
    id, project_id, kind, name, description, created_by, created_at, disabled_at

            