SELECT revision,body_markdown,via_channel,via_client,created_at AS "created_at!: _"
FROM comment_revisions WHERE comment_id=$1 AND ($2::bigint IS NULL OR revision>$2)
ORDER BY revision LIMIT $3
