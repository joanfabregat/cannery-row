-- Identity: users, projects, memberships, service accounts, tokens, sessions,
-- OIDC login state, and the append-only audit log shared by every domain.

CREATE TABLE users (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    issuer        text NOT NULL,
    subject       text NOT NULL,
    email         text,
    email_verified boolean NOT NULL DEFAULT false,
    display_name  text,
    is_admin      boolean NOT NULL DEFAULT false,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz,
    UNIQUE (issuer, subject)
);
CREATE INDEX users_email_idx ON users (lower(email));

CREATE TABLE projects (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    slug        text NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    title       text NOT NULL CHECK (title ~ '\S'),
    description text NOT NULL DEFAULT '',
    created_by  uuid NOT NULL REFERENCES users (id),
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE memberships (
    project_id uuid NOT NULL REFERENCES projects (id),
    user_id    uuid NOT NULL REFERENCES users (id),
    role       text NOT NULL CHECK (role IN ('viewer', 'member', 'researcher')),
    granted_by uuid NOT NULL REFERENCES users (id),
    granted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, user_id)
);
CREATE INDEX memberships_user_idx ON memberships (user_id);

CREATE TABLE service_accounts (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  uuid NOT NULL REFERENCES projects (id),
    kind        text NOT NULL CHECK (kind IN ('agent', 'tester', 'evaluator')),
    name        text NOT NULL CHECK (name ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    description text NOT NULL DEFAULT '',
    created_by  uuid NOT NULL REFERENCES users (id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    disabled_at timestamptz,
    UNIQUE (project_id, name)
);

-- Bearer tokens. Only the SHA-256 of the secret is stored.
CREATE TABLE api_tokens (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    token_hash         bytea NOT NULL UNIQUE,
    display_prefix     text NOT NULL,
    kind               text NOT NULL CHECK (kind IN ('personal', 'service')),
    user_id            uuid REFERENCES users (id),
    service_account_id uuid REFERENCES service_accounts (id),
    name               text NOT NULL CHECK (name ~ '\S'),
    scopes             text[] NOT NULL CHECK (scopes <@ ARRAY['read', 'write']::text[]
                                              AND cardinality(scopes) > 0),
    created_at         timestamptz NOT NULL DEFAULT now(),
    expires_at         timestamptz NOT NULL,
    last_used_at       timestamptz,
    revoked_at         timestamptz,
    CHECK ((kind = 'personal' AND user_id IS NOT NULL AND service_account_id IS NULL)
        OR (kind = 'service' AND service_account_id IS NOT NULL AND user_id IS NULL))
);
CREATE INDEX api_tokens_user_idx ON api_tokens (user_id);
CREATE INDEX api_tokens_service_account_idx ON api_tokens (service_account_id);

-- Browser sessions. The cookie carries a random secret; only its hash is stored.
CREATE TABLE sessions (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    secret_hash  bytea NOT NULL UNIQUE,
    user_id      uuid NOT NULL REFERENCES users (id),
    csrf_token   text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    last_seen_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX sessions_user_idx ON sessions (user_id);

-- Pending OIDC authorization requests (state, nonce, PKCE verifier). The
-- browser binding is the hash of a random value set in a short-lived cookie at
-- login start, so a callback URL cannot be replayed in another browser.
CREATE TABLE oidc_login_requests (
    state         text PRIMARY KEY,
    nonce         text NOT NULL,
    code_verifier text NOT NULL,
    return_to     text NOT NULL,
    browser_hash  bytea NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);

-- Append-only audit log. "actor" is the accountable identity; "via" is the
-- channel and client used. Never updated or deleted.
CREATE TABLE audit_events (
    seq                bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    occurred_at        timestamptz NOT NULL DEFAULT now(),
    project_id         uuid REFERENCES projects (id),
    actor_kind         text NOT NULL CHECK (actor_kind IN ('user', 'service', 'system')),
    actor_user_id      uuid REFERENCES users (id),
    actor_service_id   uuid REFERENCES service_accounts (id),
    via_channel        text NOT NULL CHECK (via_channel IN ('ui', 'api', 'mcp', 'cli', 'system')),
    via_client         text,
    action             text NOT NULL,
    subject_type       text NOT NULL,
    subject_id         text NOT NULL,
    prior_state        jsonb,
    new_state          jsonb,
    reason             text,
    idempotency_key    text,
    CHECK ((actor_kind = 'user' AND actor_user_id IS NOT NULL AND actor_service_id IS NULL)
        OR (actor_kind = 'service' AND actor_service_id IS NOT NULL AND actor_user_id IS NULL)
        OR (actor_kind = 'system' AND actor_user_id IS NULL AND actor_service_id IS NULL))
);
CREATE INDEX audit_events_project_idx ON audit_events (project_id, seq);
CREATE INDEX audit_events_subject_idx ON audit_events (subject_type, subject_id, seq);

CREATE FUNCTION audit_events_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_events is append-only';
END;
$$;
CREATE TRIGGER audit_events_no_update BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION audit_events_immutable();
