
            DELETE FROM oidc_login_requests WHERE state = $1
            RETURNING nonce, code_verifier, return_to, browser_hash
            