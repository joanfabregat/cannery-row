SELECT kind,author,written_on AS "written_on?: _",written_at AS "written_at?: _",body_markdown,source_ref
FROM imported_reports WHERE attempt_id=$1
