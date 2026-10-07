
        UPDATE uploads SET state = 'receiving', receiving_since = now()
        WHERE id = $1 AND state = 'pending' AND expires_at > now()
        