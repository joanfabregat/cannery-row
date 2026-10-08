SELECT front_matter->>'kind' AS "kind!",front_matter->>'author' AS "author!",(front_matter->>'written_on')::date AS "written_on?: _",(front_matter->>'written_at')::timestamptz AS "written_at?: _",body AS body_markdown,source_ref AS "source_ref!"
FROM phase_outputs WHERE attempt_id=$1 AND stage='writeup' AND origin='imported'
