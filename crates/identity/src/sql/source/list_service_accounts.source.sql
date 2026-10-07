
            SELECT 
    id, project_id, kind, name, description, created_by, created_at, disabled_at
 FROM service_accounts
            WHERE project_id = $1 AND ($2::text IS NULL OR name > $3)
            ORDER BY name LIMIT $4
            