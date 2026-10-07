
        UPDATE attempts SET state = $1,
            finished_at = CASE WHEN $2 IN ('failed', 'cancelled', 'promoted', 'rejected',
                                           'inconclusive')
                               THEN coalesce(finished_at, now()) ELSE finished_at END
        WHERE id = $3 AND state = $4 AND lease_token_hash IS NULL
        