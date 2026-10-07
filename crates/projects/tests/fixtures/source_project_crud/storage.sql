SELECT 'project', id::text, slug, title, description, created_by::text, created_at::text
FROM projects WHERE slug<>'clock'
UNION ALL
SELECT 'member', project_id::text, user_id::text, role, granted_by::text, granted_at::text, NULL
FROM memberships WHERE project_id<>md5('fixture:clock')::uuid
ORDER BY 1,2,3
