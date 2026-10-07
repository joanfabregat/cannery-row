
        UPDATE uploads SET slot = NULL
        WHERE backend = $1 AND bucket = $2 AND slot = $3 AND transfer <> 'stream'
          AND expires_at <= now() AND (state IN ('pending', 'expired') OR (
    uploads.state = 'receiving'
    AND (uploads.receiving_since IS NULL
         OR uploads.receiving_since <= now() - make_interval(secs => $4))
))
        