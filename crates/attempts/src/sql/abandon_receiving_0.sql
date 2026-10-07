
        UPDATE uploads SET state = 'pending', receiving_since = NULL
        WHERE id = $1 AND state = 'receiving'
        